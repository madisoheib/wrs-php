use crate::state::{error_frame, frame, sign, App, ChannelKey, Conn, State, Sub};
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
    state.connections.insert(
        socket_id.clone(),
        Conn { kill: kill.clone(), app_id: app.id.clone() },
    );

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
    loop {
        tokio::select! {
            item = stream.next() => {
                deadline = Instant::now() + activity;
                pinged = false;
                match item {
                    Some(Ok(Message::Text(t))) => {
                        on_text(&state, &app, &socket_id, &tx, &kill, &mut subs, t.as_str()).await;
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

async fn on_text(
    state: &Arc<State>,
    app: &App,
    socket_id: &str,
    tx: &mpsc::Sender<Message>,
    kill: &Arc<Notify>,
    subs: &mut HashSet<String>,
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
        // client-* events and presence are v1; ignore other events for now.
        _ => {}
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
        // ponytail: presence is v1. Refuse cleanly instead of half-supporting.
        let _ = tx.send(error_frame(4009, "Presence channels not supported yet")).await;
        return;
    }

    if channel.starts_with("private-") || channel.starts_with("private-encrypted-") {
        if !auth_ok(app, socket_id, &channel, &data) {
            let _ = tx.send(error_frame(4009, "Connection not authorized")).await;
            return;
        }
    }

    state
        .channels
        .entry((app.id.clone(), channel.clone()))
        .or_default()
        .subscribers
        .insert(socket_id.to_string(), Sub { tx: tx.clone(), kill: kill.clone() });
    subs.insert(channel.clone());

    let _ = tx
        .send(frame("pusher_internal:subscription_succeeded", Some(&channel), "{}".into()))
        .await;
}

/// Verify `auth = "{key}:{hmac_hex}"` where hmac = HMAC-SHA256(secret, "socket_id:channel").
fn auth_ok(app: &App, socket_id: &str, channel: &str, data: &serde_json::Value) -> bool {
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
    let expected = sign(app.secret.as_bytes(), format!("{socket_id}:{channel}").as_bytes());
    // Constant-time compare to avoid leaking signature bytes via timing.
    provided.as_bytes().ct_eq(expected.as_bytes()).into()
}

fn remove_sub(state: &State, app_id: &str, channel: &str, socket_id: &str) {
    let key: ChannelKey = (app_id.to_string(), channel.to_string());
    if let Some(mut e) = state.channels.get_mut(&key) {
        e.subscribers.remove(socket_id);
        if e.subscribers.is_empty() {
            drop(e);
            state.channels.remove(&key);
        }
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
