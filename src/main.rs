mod api;
mod state;
mod ws;

use axum::{
    extract::{ws::WebSocketUpgrade, Path, State as AxState},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use state::{App, Limits, State};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "resonance", about = "Pusher-compatible WebSocket server")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the server.
    Start {
        #[arg(short, long, default_value = "resonance.toml")]
        config: String,
    },
}

// --- Config (TOML, with a few env overrides for Docker) ---------------------

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    server: Server,
    #[serde(default)]
    apps: Vec<AppCfg>,
    #[serde(default)]
    limits: LimitsCfg,
    tls: Option<TlsCfg>,
}

#[derive(Deserialize)]
struct TlsCfg {
    cert: String,
    key: String,
}

#[derive(Deserialize)]
struct Server {
    #[serde(default = "default_host")]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
}
impl Default for Server {
    fn default() -> Self {
        Server { host: default_host(), port: default_port() }
    }
}
fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    8080
}

#[derive(Deserialize)]
struct AppCfg {
    id: String,
    key: String,
    secret: String,
    #[serde(default)]
    max_connections: usize,
    #[serde(default)]
    webhook_url: Option<String>,
}

#[derive(Deserialize)]
struct LimitsCfg {
    #[serde(default = "default_msg_kb")]
    max_message_size_kb: usize,
    #[serde(default = "default_timeout")]
    activity_timeout_s: u64,
    #[serde(default = "default_max_channels")]
    max_channels_per_connection: usize,
    #[serde(default)]
    allowed_origins: Vec<String>,
    #[serde(default = "default_client_event_rate")]
    client_event_rate: u32,
}
impl Default for LimitsCfg {
    fn default() -> Self {
        LimitsCfg {
            max_message_size_kb: default_msg_kb(),
            activity_timeout_s: default_timeout(),
            max_channels_per_connection: default_max_channels(),
            allowed_origins: Vec::new(),
            client_event_rate: default_client_event_rate(),
        }
    }
}
fn default_msg_kb() -> usize {
    10
}
fn default_client_event_rate() -> u32 {
    10
}
fn default_timeout() -> u64 {
    120
}
fn default_max_channels() -> usize {
    100
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let Cli { cmd } = Cli::parse();
    let Cmd::Start { config } = cmd;

    let raw = match std::fs::read_to_string(&config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Cannot read config '{config}': {e}\nSee resonance.toml.example for a template.");
            std::process::exit(1);
        }
    };
    let mut cfg: Config = match toml::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Invalid config: {e}");
            std::process::exit(1);
        }
    };

    // Env overrides (indispensable for Docker).
    if let Ok(h) = std::env::var("RESONANCE_HOST") {
        cfg.server.host = h;
    }
    if let Some(p) = std::env::var("RESONANCE_PORT").ok().and_then(|p| p.parse().ok()) {
        cfg.server.port = p;
    }

    if cfg.apps.is_empty() {
        eprintln!("No [[apps]] configured — nothing to serve.");
        std::process::exit(1);
    }

    let apps: Vec<App> = cfg
        .apps
        .into_iter()
        .map(|a| App {
            id: a.id,
            key: a.key,
            secret: a.secret,
            max_connections: a.max_connections,
            webhook_url: a.webhook_url,
        })
        .collect();
    // RESONANCE_ALLOWED_ORIGINS="https://a.com,https://b.com" overrides config.
    if let Ok(v) = std::env::var("RESONANCE_ALLOWED_ORIGINS") {
        cfg.limits.allowed_origins = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    let limits = Limits {
        max_message_size: cfg.limits.max_message_size_kb * 1024,
        activity_timeout_s: cfg.limits.activity_timeout_s,
        max_channels_per_connection: cfg.limits.max_channels_per_connection,
        allowed_origins: cfg.limits.allowed_origins,
        client_event_rate: cfg.limits.client_event_rate,
    };
    // Webhook worker: only spawned if at least one app wants webhooks.
    let webhook_tx = if apps.iter().any(|a| a.webhook_url.is_some()) {
        // ponytail: bounded queue, drops on overflow, no retry — webhooks are
        // best-effort notifications. Add retry/batching if consumers need it.
        let (tx, rx) = tokio::sync::mpsc::channel::<state::WebhookEvent>(1024);
        tokio::spawn(webhook_worker(rx, apps.clone()));
        Some(tx)
    } else {
        None
    };
    let state = State::new(apps, limits, webhook_tx);

    let app = Router::new()
        .route("/app/{key}", get(ws_route))
        .route("/apps/{app_id}/events", post(api::events))
        .route("/apps/{app_id}/batch_events", post(api::batch_events))
        .route("/apps/{app_id}/channels", get(api::channels_index))
        .route("/apps/{app_id}/channels/{name}", get(api::channel_show))
        .route("/apps/{app_id}/channels/{name}/users", get(api::channel_users))
        .route("/metrics", get(api::metrics))
        .with_state(state.clone());

    let addr = format!("{}:{}", cfg.server.host, cfg.server.port);

    if let Some(tls) = cfg.tls {
        // Native TLS (rustls/ring — keeps the musl static build intact).
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("install rustls provider");
        let rustls_cfg = axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls.cert, &tls.key)
            .await
            .unwrap_or_else(|e| {
                eprintln!("Cannot load TLS cert/key ({} / {}): {e}", tls.cert, tls.key);
                std::process::exit(1);
            });
        let sock_addr: std::net::SocketAddr = addr.parse().unwrap_or_else(|e| {
            eprintln!("Invalid listen address {addr}: {e}");
            std::process::exit(1);
        });
        tracing::info!("resonance listening on {addr} (TLS, {} app(s))", state.apps.len());
        // Compose: NoDelayAcceptor sets TCP_NODELAY, then rustls handshakes.
        let acceptor = axum_server::tls_rustls::RustlsAcceptor::new(rustls_cfg)
            .acceptor(axum_server::accept::NoDelayAcceptor);
        axum_server::bind(sock_addr)
            .acceptor(acceptor)
            .serve(app.into_make_service())
            .await
            .unwrap();
    } else {
        let listener = tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| {
            eprintln!("Cannot bind {addr}: {e}");
            std::process::exit(1);
        });
        // Latency beats throughput for real-time: disable Nagle on every socket.
        let listener = axum::serve::ListenerExt::tap_io(listener, |io| {
            let _ = io.set_nodelay(true);
        });
        tracing::info!("resonance listening on {addr} ({} app(s))", state.apps.len());
        axum::serve(listener, app).await.unwrap();
    }
}

/// Drains the webhook queue and POSTs Pusher-format webhooks:
/// body {"time_ms":..., "events":[...]}, signed with X-Pusher-Key +
/// X-Pusher-Signature (HMAC-SHA256 of the raw body).
async fn webhook_worker(
    mut rx: tokio::sync::mpsc::Receiver<state::WebhookEvent>,
    apps: Vec<App>,
) {
    let client = reqwest::Client::new();
    while let Some(ev) = rx.recv().await {
        let Some(app) = apps.iter().find(|a| a.id == ev.app_id) else { continue };
        let Some(url) = &app.webhook_url else { continue };
        let time_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let body = serde_json::json!({"time_ms": time_ms, "events": [ev.event]}).to_string();
        let signature = state::sign(app.secret.as_bytes(), body.as_bytes());
        let res = client
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Pusher-Key", &app.key)
            .header("X-Pusher-Signature", signature)
            .body(body)
            .send()
            .await;
        if let Err(e) = res {
            tracing::warn!("webhook delivery to {url} failed: {e}");
        }
    }
}

async fn ws_route(
    ws: WebSocketUpgrade,
    Path(key): Path<String>,
    AxState(state): AxState<Arc<State>>,
    headers: axum::http::HeaderMap,
) -> Response {
    // Origin allow-list (empty = allow all, for dev). Browsers always send
    // Origin on WS upgrades; non-browser clients (no Origin) are allowed —
    // they aren't subject to cross-site attacks and can't be blocked anyway.
    if !state.limits.allowed_origins.is_empty() {
        if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
            if !state.limits.allowed_origins.iter().any(|o| o == origin) {
                return axum::http::StatusCode::FORBIDDEN.into_response();
            }
        }
    }
    let app = state.app_by_key(&key).cloned();
    let max = state.limits.max_message_size;
    // tungstenite defaults to a 128KB read + 128KB write buffer PER connection,
    // eagerly allocated — ~256KB/conn, far above the spec's <20KB target. Pusher
    // frames are tiny, so shrink both. read_buffer_size is the big idle win.
    ws.max_message_size(max)
        .max_frame_size(max)
        .read_buffer_size(4 * 1024)
        .write_buffer_size(4 * 1024)
        .max_write_buffer_size(64 * 1024)
        .on_upgrade(move |socket| async move {
        match app {
            Some(app) => ws::handle(socket, state, app).await,
            None => {
                use futures_util::SinkExt;
                let mut socket = socket;
                let _ = socket
                    .send(state::error_frame(4001, "Application does not exist"))
                    .await;
                let _ = SinkExt::close(&mut socket).await;
            }
        }
    })
}
