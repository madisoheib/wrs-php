use axum::extract::ws::{Message, Utf8Bytes};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, Notify};

pub type HmacSha256 = Hmac<Sha256>;
pub type ChannelKey = (String, String); // (app_id, channel_name)

#[derive(Clone)]
pub struct App {
    pub id: String,
    pub key: String,
    pub secret: String,
    pub max_connections: usize, // 0 = unlimited
}

pub struct Limits {
    pub max_message_size: usize, // bytes
    pub activity_timeout_s: u64,
    pub max_channels_per_connection: usize,
    // Browser origins allowed to open WS connections. Empty = allow all (dev).
    pub allowed_origins: Vec<String>,
    // client-* events per second per connection (Pusher default ~10).
    pub client_event_rate: u32,
}

pub struct Conn {
    pub app_id: String,
}

/// Per-subscriber handle stored directly in the channel: fan-out clones two
/// refcounted pointers (Sender + Notify) — zero allocations, no second map hop,
/// and a slow consumer can be killed without a connections lookup.
#[derive(Clone)]
pub struct Sub {
    pub tx: mpsc::Sender<Message>,
    pub kill: Arc<Notify>,
}

#[derive(Clone)]
pub struct PresenceMember {
    pub user_id: String,
    pub user_info: serde_json::Value,
}

#[derive(Default)]
pub struct ChannelState {
    // socket_id -> subscriber handle
    pub subscribers: HashMap<String, Sub>,
    // presence channels only: socket_id -> member identity
    pub presence: Option<HashMap<String, PresenceMember>>,
}

pub struct State {
    pub apps: Vec<App>, // a handful, from config — linear scan is fine
    pub connections: DashMap<String, Conn>,
    pub channels: DashMap<ChannelKey, ChannelState>,
    pub limits: Limits,
    socket_seq: AtomicU64,
    socket_hi: u64,
}

impl State {
    pub fn new(apps: Vec<App>, limits: Limits) -> Arc<Self> {
        // ponytail: socket_id hi part seeded from startup nanos; low part is a
        // counter. Unique per run, which is all a Pusher socket_id needs.
        let hi = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 % 1_000_000_000)
            .unwrap_or(1);
        Arc::new(State {
            apps,
            connections: DashMap::new(),
            channels: DashMap::new(),
            limits,
            socket_seq: AtomicU64::new(1),
            socket_hi: hi,
        })
    }

    pub fn new_socket_id(&self) -> String {
        let n = self.socket_seq.fetch_add(1, Ordering::Relaxed);
        format!("{}.{}", self.socket_hi, n)
    }

    pub fn app_by_key(&self, key: &str) -> Option<&App> {
        self.apps.iter().find(|a| a.key == key)
    }

    pub fn app_by_id(&self, id: &str) -> Option<&App> {
        self.apps.iter().find(|a| a.id == id)
    }

    pub fn app_connection_count(&self, app_id: &str) -> usize {
        // ponytail: O(n) over live connections, only checked at connect time.
        // Add a per-app counter if connect rate ever makes this hot.
        self.connections
            .iter()
            .filter(|c| c.value().app_id == app_id)
            .count()
    }
}

// --- Pusher message helpers -------------------------------------------------

/// Build an outer Pusher frame. `data` is embedded as a JSON-encoded STRING
/// (Pusher's double-encoding — sending a bare object breaks clients).
pub fn frame(event: &str, channel: Option<&str>, data: String) -> Message {
    let v = match channel {
        Some(ch) => serde_json::json!({"event": event, "channel": ch, "data": data}),
        None => serde_json::json!({"event": event, "data": data}),
    };
    Message::Text(Utf8Bytes::from(v.to_string()))
}

pub fn error_frame(code: u16, message: &str) -> Message {
    let data = serde_json::json!({"code": code, "message": message}).to_string();
    frame("pusher:error", None, data)
}

pub fn sign(secret: &[u8], msg: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(msg);
    hex::encode(mac.finalize().into_bytes())
}
