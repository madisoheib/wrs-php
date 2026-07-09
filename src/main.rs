mod api;
mod state;
mod ws;

use axum::{
    extract::{ws::WebSocketUpgrade, Path, State as AxState},
    response::Response,
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
}

#[derive(Deserialize)]
struct LimitsCfg {
    #[serde(default = "default_msg_kb")]
    max_message_size_kb: usize,
    #[serde(default = "default_timeout")]
    activity_timeout_s: u64,
    #[serde(default = "default_max_channels")]
    max_channels_per_connection: usize,
}
impl Default for LimitsCfg {
    fn default() -> Self {
        LimitsCfg {
            max_message_size_kb: default_msg_kb(),
            activity_timeout_s: default_timeout(),
            max_channels_per_connection: default_max_channels(),
        }
    }
}
fn default_msg_kb() -> usize {
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

    let apps = cfg
        .apps
        .into_iter()
        .map(|a| App { id: a.id, key: a.key, secret: a.secret, max_connections: a.max_connections })
        .collect();
    let limits = Limits {
        max_message_size: cfg.limits.max_message_size_kb * 1024,
        activity_timeout_s: cfg.limits.activity_timeout_s,
        max_channels_per_connection: cfg.limits.max_channels_per_connection,
    };
    let state = State::new(apps, limits);

    let app = Router::new()
        .route("/app/{key}", get(ws_route))
        .route("/apps/{app_id}/events", post(api::events))
        .with_state(state.clone());

    let addr = format!("{}:{}", cfg.server.host, cfg.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| {
        eprintln!("Cannot bind {addr}: {e}");
        std::process::exit(1);
    });
    // ponytail: TCP_NODELAY not set per-socket (axum::serve owns the accept
    // loop). Add a custom accept loop with set_nodelay(true) when latency
    // benchmarks call for it — see spec 3.4 / 5.4.
    tracing::info!("resonance listening on {addr} ({} app(s))", state.apps.len());
    axum::serve(listener, app).await.unwrap();
}

async fn ws_route(
    ws: WebSocketUpgrade,
    Path(key): Path<String>,
    AxState(state): AxState<Arc<State>>,
) -> Response {
    let app = state.app_by_key(&key).cloned();
    let max = state.limits.max_message_size;
    // Default tungstenite write buffer is ~128KB/conn — far above the spec's
    // <20KB/conn target. Real-time frames are small; keep buffers tiny.
    ws.max_message_size(max)
        .max_frame_size(max)
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
