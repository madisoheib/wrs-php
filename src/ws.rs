use crate::state::{error_frame, frame, sign, App, ChannelKey, Conn, PresenceMember, State, Sub};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant;

pub async fn handle(ws: WebSocket, state: Arc<State>, app: App) {
    let socket_id = state.new_socket_id();

    if app.max_connections != 0 && state.app_connection_count(&app.id) >= app.max_connections {
        // Best-effort refusal; the socket just closes.
        let _ = pipe_close(ws).await;
        return;
    }

    let (tx, rx) = mpsc::channel::<Message>(64);
    let kill = Arc::new(Notify::new());
    state.connections.insert(socket_id.clone(), Conn { app_id: app.id.clone() });
    state.metrics.connections_total.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Handshake: activity_timeout advertised to the client.
    let est = serde_json::json!({
        "socket_id": socket_id,
        "activity_timeout": state.limits.activity_timeout_s,
    })
    .to_string();
    let _ = tx.send(frame("pusher:connection_established", None, est)).await;

    let (mut sink, mut stream) = ws.split();
    let mut rx = rx;
    let writer = tokio::spawn(async move {
        // Batch queued messages per flush: one syscall for a fan-out burst
        // instead of one per message.
        let mut batch: Vec<Message> = Vec::with_capacity(16);
        loop {
            if rx.recv_many(&mut batch, 16).await == 0 {
                break;
            }
            let mut dead = false;
            for m in batch.drain(..) {
                if sink.feed(m).await.is_err() {
                    dead = true;
                    break;
                }
            }
            if dead || sink.flush().await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Activity timeout (spec 3.5): any inbound traffic resets the clock; after
    // activity_timeout we ping, and evict if nothing comes back within 30s.
    let activity = Duration::from_secs(state.limits.activity_timeout_s);
    let grace = Duration::from_secs(30);
    let mut deadline = Instant::now() + activity;
    let mut pinged = false;

    let mut subs: HashSet<String> = HashSet::new();
    let mut rate = RateLimiter::new(state.limits.client_event_rate);
    loop {
        tokio::select! {
            item = stream.next() => {
                deadline = Instant::now() + activity;
                pinged = false;
                match item {
                    Some(Ok(Message::Text(t))) => {
                        on_text(&state, &app, &socket_id, &tx, &kill, &mut subs, &mut rate, t.as_str()).await;
                    }
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            },
            _ = tokio::time::sleep_until(deadline) => {
                if pinged {
                    break; // no pong within grace -> dead connection
                }
                let _ = tx.try_send(frame("pusher:ping", None, "{}".into()));
                pinged = true;
                deadline = Instant::now() + grace;
            },
            _ = kill.notified() => break,
        }
    }

    writer.abort();
    cleanup(&state, &app.id, &socket_id, &subs);
}

#[allow(clippy::too_many_arguments)]
async fn on_text(
    state: &Arc<State>,
    app: &App,
    socket_id: &str,
    tx: &mpsc::Sender<Message>,
    kill: &Arc<Notify>,
    subs: &mut HashSet<String>,
    rate: &mut RateLimiter,
    text: &str,
) {
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            let _ = tx.send(error_frame(4200, "Invalid JSON")).await;
            return;
        }
    };
    let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
    match event {
        "pusher:ping" => {
            let _ = tx.send(frame("pusher:pong", None, "{}".into())).await;
        }
        "pusher:subscribe" => subscribe(state, app, socket_id, tx, kill, subs, &v).await,
        "pusher:unsubscribe" => {
            if let Some(ch) = v.get("data").and_then(|d| d.get("channel")).and_then(|c| c.as_str()) {
                if subs.remove(ch) {
                    remove_sub(state, &app.id, ch, socket_id);
                }
            }
        }
        e if e.starts_with("client-") => {
            if !rate.allow() {
                let _ = tx.send(error_frame(4301, "Client event rate limit exceeded")).await;
                return;
            }
            client_event(state, app, socket_id, tx, subs, &v).await;
        }
        _ => {}
    }
}

/// Fixed-window rate limiter — good enough for a per-connection cap.
/// ponytail: 1s fixed window (not sliding); upgrade to token bucket if burst
/// shaping ever matters.
pub struct RateLimiter {
    window: Instant,
    count: u32,
    max: u32,
}

impl RateLimiter {
    pub fn new(max: u32) -> Self {
        RateLimiter { window: Instant::now(), count: 0, max }
    }
    pub fn allow(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window) >= Duration::from_secs(1) {
            self.window = now;
            self.count = 0;
        }
        self.count += 1;
        self.count <= self.max
    }
}

async fn subscribe(
    state: &Arc<State>,
    app: &App,
    socket_id: &str,
    tx: &mpsc::Sender<Message>,
    kill: &Arc<Notify>,
    subs: &mut HashSet<String>,
    v: &serde_json::Value,
) {
    let data = v.get("data").cloned().unwrap_or(serde_json::Value::Null);
    let channel = match data.get("channel").and_then(|c| c.as_str()) {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => {
            let _ = tx.send(error_frame(4009, "No channel provided")).await;
            return;
        }
    };

    if subs.len() >= state.limits.max_channels_per_connection && !subs.contains(&channel) {
        let _ = tx.send(error_frame(4100, "Channel limit reached")).await;
        return;
    }

    if channel.starts_with("presence-") {
        subscribe_presence(state, app, socket_id, tx, kill, subs, &channel, &data).await;
        return;
    }

    if channel.starts_with("private-") && !auth_ok(app, socket_id, &channel, &data, None) {
        let _ = tx.send(error_frame(4009, "Connection not authorized")).await;
        return;
    }

    let first = {
        let mut e = state.channels.entry((app.id.clone(), channel.clone())).or_default();
        let first = e.subscribers.is_empty();
        e.subscribers
            .insert(socket_id.to_string(), Sub { tx: tx.clone(), kill: kill.clone() });
        first
    };
    subs.insert(channel.clone());
    if first {
        state.webhook(&app.id, serde_json::json!({"name": "channel_occupied", "channel": channel}));
    }

    let _ = tx
        .send(frame("pusher_internal:subscription_succeeded", Some(&channel), "{}".into()))
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn subscribe_presence(
    state: &Arc<State>,
    app: &App,
    socket_id: &str,
    tx: &mpsc::Sender<Message>,
    kill: &Arc<Notify>,
    subs: &mut HashSet<String>,
    channel: &str,
    data: &serde_json::Value,
) {
    // Presence auth signs socket_id:channel:channel_data.
    let channel_data = match data.get("channel_data").and_then(|d| d.as_str()) {
        Some(d) => d,
        None => {
            let _ = tx.send(error_frame(4009, "channel_data required for presence")).await;
            return;
        }
    };
    if !auth_ok(app, socket_id, channel, data, Some(channel_data)) {
        let _ = tx.send(error_frame(4009, "Connection not authorized")).await;
        return;
    }
    let member: serde_json::Value = match serde_json::from_str(channel_data) {
        Ok(m) => m,
        Err(_) => {
            let _ = tx.send(error_frame(4009, "Invalid channel_data")).await;
            return;
        }
    };
    // user_id may be a string or a number in channel_data.
    let user_id = match member.get("user_id") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => {
            let _ = tx.send(error_frame(4009, "channel_data.user_id required")).await;
            return;
        }
    };
    let user_info = member.get("user_info").cloned().unwrap_or(serde_json::Value::Null);

    // Insert member + build the roster and member_added targets under one
    // short lock, send after release.
    let (roster, added_msg_targets, first, is_new_user) = {
        let mut e = state.channels.entry((app.id.clone(), channel.to_string())).or_default();
        let first = e.subscribers.is_empty();
        e.subscribers
            .insert(socket_id.to_string(), Sub { tx: tx.clone(), kill: kill.clone() });
        let presence = e.presence.get_or_insert_with(std::collections::HashMap::new);
        let is_new_user = !presence.values().any(|m| m.user_id == user_id);
        presence.insert(
            socket_id.to_string(),
            PresenceMember { user_id: user_id.clone(), user_info: user_info.clone() },
        );

        let mut ids = Vec::new();
        let mut hash = serde_json::Map::new();
        for m in presence.values() {
            if !hash.contains_key(&m.user_id) {
                ids.push(m.user_id.clone());
                hash.insert(m.user_id.clone(), m.user_info.clone());
            }
        }
        let roster = serde_json::json!({
            "presence": {"count": ids.len(), "ids": ids, "hash": hash}
        });
        let targets: Vec<Sub> = if is_new_user {
            e.subscribers
                .iter()
                .filter(|(sid, _)| sid.as_str() != socket_id)
                .map(|(_, s)| s.clone())
                .collect()
        } else {
            Vec::new()
        };
        (roster, targets, first, is_new_user)
    };
    subs.insert(channel.to_string());
    if first {
        state.webhook(&app.id, serde_json::json!({"name": "channel_occupied", "channel": channel}));
    }
    if is_new_user {
        state.webhook(
            &app.id,
            serde_json::json!({"name": "member_added", "channel": channel, "user_id": user_id}),
        );
    }

    let _ = tx
        .send(frame("pusher_internal:subscription_succeeded", Some(channel), roster.to_string()))
        .await;

    if !added_msg_targets.is_empty() {
        let added = serde_json::json!({"user_id": user_id, "user_info": user_info});
        let msg = frame("pusher_internal:member_added", Some(channel), added.to_string());
        for s in added_msg_targets {
            if s.tx.try_send(msg.clone()).is_err() {
                s.kill.notify_one();
            }
        }
    }
}

/// Relay a client-* event to the channel's other subscribers.
/// Only allowed on private-/presence- channels the sender is subscribed to.
async fn client_event(
    state: &Arc<State>,
    app: &App,
    socket_id: &str,
    tx: &mpsc::Sender<Message>,
    subs: &HashSet<String>,
    v: &serde_json::Value,
) {
    let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
    let channel = match v.get("channel").and_then(|c| c.as_str()) {
        Some(c) => c,
        None => return,
    };
    if !(channel.starts_with("private-") || channel.starts_with("presence-")) || !subs.contains(channel) {
        let _ = tx
            .send(error_frame(4009, "Client events are only allowed on subscribed private/presence channels"))
            .await;
        return;
    }
    // Relay data verbatim (client events are not double-encoded by the server).
    let out = serde_json::json!({
        "event": event,
        "channel": channel,
        "data": v.get("data").cloned().unwrap_or(serde_json::Value::Null),
    });
    let msg = Message::Text(out.to_string().into());

    let targets: Vec<Sub> = match state.channels.get(&(app.id.clone(), channel.to_string())) {
        Some(cs) => cs
            .subscribers
            .iter()
            .filter(|(sid, _)| sid.as_str() != socket_id) // never echo back to sender
            .map(|(_, s)| s.clone())
            .collect(),
        None => return,
    };
    for s in targets {
        if s.tx.try_send(msg.clone()).is_err() {
            s.kill.notify_one();
        }
    }
}

/// Verify `auth = "{key}:{hmac_hex}"` where hmac = HMAC-SHA256(secret,
/// "socket_id:channel") — presence adds ":channel_data".
fn auth_ok(
    app: &App,
    socket_id: &str,
    channel: &str,
    data: &serde_json::Value,
    channel_data: Option<&str>,
) -> bool {
    let auth = match data.get("auth").and_then(|a| a.as_str()) {
        Some(a) => a,
        None => return false,
    };
    let (key, provided) = match auth.split_once(':') {
        Some(kv) => kv,
        None => return false,
    };
    if key != app.key {
        return false;
    }
    let to_sign = match channel_data {
        Some(cd) => format!("{socket_id}:{channel}:{cd}"),
        None => format!("{socket_id}:{channel}"),
    };
    let expected = sign(app.secret.as_bytes(), to_sign.as_bytes());
    // Constant-time compare to avoid leaking signature bytes via timing.
    provided.as_bytes().ct_eq(expected.as_bytes()).into()
}

fn remove_sub(state: &State, app_id: &str, channel: &str, socket_id: &str) {
    let key: ChannelKey = (app_id.to_string(), channel.to_string());
    let mut removed_member: Option<(String, Vec<Sub>)> = None;
    let mut vacated = false;
    if let Some(mut e) = state.channels.get_mut(&key) {
        e.subscribers.remove(socket_id);
        // Presence: emit member_removed only when the user's LAST socket leaves.
        if let Some(presence) = e.presence.as_mut() {
            if let Some(m) = presence.remove(socket_id) {
                if !presence.values().any(|o| o.user_id == m.user_id) {
                    let targets = e.subscribers.values().cloned().collect();
                    removed_member = Some((m.user_id, targets));
                }
            }
        }
        if e.subscribers.is_empty() {
            drop(e);
            state.channels.remove(&key);
            vacated = true;
        }
    }
    if let Some((user_id, targets)) = removed_member {
        let data = serde_json::json!({"user_id": user_id}).to_string();
        let msg = frame("pusher_internal:member_removed", Some(channel), data);
        for s in targets {
            if s.tx.try_send(msg.clone()).is_err() {
                s.kill.notify_one();
            }
        }
        state.webhook(
            app_id,
            serde_json::json!({"name": "member_removed", "channel": channel, "user_id": user_id}),
        );
    }
    if vacated {
        state.webhook(app_id, serde_json::json!({"name": "channel_vacated", "channel": channel}));
    }
}

fn cleanup(state: &State, app_id: &str, socket_id: &str, subs: &HashSet<String>) {
    state.connections.remove(socket_id);
    for ch in subs {
        remove_sub(state, app_id, ch, socket_id);
    }
}

async fn pipe_close(ws: WebSocket) -> Result<(), axum::Error> {
    let mut ws = ws;
    ws.close().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App {
            id: "a".into(),
            key: "k".into(),
            secret: "s".into(),
            max_connections: 0,
            webhook_url: None,
        }
    }

    // ponytail: deterministic adversarial corpus, not libFuzzer — cargo-fuzz
    // needs a nightly toolchain this env lacks. Panics are the only crash
    // vector in safe Rust; this hammers the parsing paths with hostile input.
    #[test]
    fn auth_ok_never_panics_on_garbage() {
        let a = app();
        let mut lcg: u64 = 0x5EED;
        let mut junk = String::new();
        for i in 0..5000 {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let c = char::from_u32((lcg % 0x2FFFF) as u32).unwrap_or('\u{FFFD}');
            junk.push(c);
            if junk.len() > 300 {
                junk.clear();
            }
            let cases = [
                serde_json::json!({"auth": junk}),
                serde_json::json!({"auth": format!("{junk}:{junk}")}),
                serde_json::json!({"auth": format!("k:{junk}")}),
                serde_json::json!({"auth": null}),
                serde_json::json!({"auth": i}),
                serde_json::json!({}),
                serde_json::json!({"auth": ":"}),
                serde_json::json!({"auth": "k:"}),
            ];
            for data in &cases {
                // must never panic; garbage must never authenticate
                assert!(!auth_ok(&a, "1.1", "private-x", data, None));
                assert!(!auth_ok(&a, &junk, &junk, data, Some(&junk)));
            }
        }
    }

    #[test]
    fn rate_limiter_caps_within_window() {
        let mut r = RateLimiter::new(3);
        assert!(r.allow());
        assert!(r.allow());
        assert!(r.allow());
        assert!(!r.allow()); // 4th in the same second is refused
    }
}
