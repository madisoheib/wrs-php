use crate::state::{frame, sign, State};
use axum::{
    body::Bytes,
    extract::{Path, Query, State as AxState},
    http::StatusCode,
    Json,
};
use md5::{Digest, Md5};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

/// POST /apps/{app_id}/events — publish an event to one or more channels.
/// This is the hot path: verify signature, serialize once per channel, fan out.
pub async fn events(
    AxState(state): AxState<Arc<State>>,
    Path(app_id): Path<String>,
    Query(q): Query<BTreeMap<String, String>>,
    body: Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let app = match state.app_by_id(&app_id) {
        Some(a) => a.clone(),
        None => return err(404, "Unknown app"),
    };

    if let Err(msg) = verify(&app.key, &app.secret, "POST", &format!("/apps/{app_id}/events"), &q, &body) {
        return err(401, msg);
    }

    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return err(400, "Invalid body"),
    };

    if publish(&state, &app.id, &payload).is_err() {
        return err(400, "Missing data");
    }
    (StatusCode::OK, Json(serde_json::json!({})))
}

/// POST /apps/{app_id}/batch_events — `{"batch":[{name,channel,data,...}, ...]}`
/// (what pusher-php-server's triggerBatch sends).
pub async fn batch_events(
    AxState(state): AxState<Arc<State>>,
    Path(app_id): Path<String>,
    Query(q): Query<BTreeMap<String, String>>,
    body: Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let app = match state.app_by_id(&app_id) {
        Some(a) => a.clone(),
        None => return err(404, "Unknown app"),
    };
    if let Err(msg) = verify(&app.key, &app.secret, "POST", &format!("/apps/{app_id}/batch_events"), &q, &body) {
        return err(401, msg);
    }
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return err(400, "Invalid body"),
    };
    let batch = match payload.get("batch").and_then(|b| b.as_array()) {
        Some(b) => b,
        None => return err(400, "Missing batch"),
    };
    for event in batch {
        let _ = publish(&state, &app.id, event); // skip malformed entries, deliver the rest
    }
    (StatusCode::OK, Json(serde_json::json!({"batch": []})))
}

/// Fan an event out to its channel(s). Payload: {name, data, channel|channels, socket_id?}.
fn publish(state: &State, app_id: &str, payload: &serde_json::Value) -> Result<(), ()> {
    let name = payload.get("name").and_then(|n| n.as_str()).unwrap_or("");
    // data is already a JSON-encoded string from the client.
    let data = match payload.get("data") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => return Err(()),
    };
    let except = payload.get("socket_id").and_then(|s| s.as_str());

    let mut channels: Vec<String> = Vec::new();
    if let Some(c) = payload.get("channel").and_then(|c| c.as_str()) {
        channels.push(c.to_string());
    }
    if let Some(arr) = payload.get("channels").and_then(|c| c.as_array()) {
        channels.extend(arr.iter().filter_map(|c| c.as_str()).map(String::from));
    }

    state.metrics.events_received_total.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for channel in &channels {
        // Serialize the outgoing frame ONCE per channel; each subscriber gets a
        // cheap refcounted clone (Utf8Bytes/Bytes), never a re-serialization.
        let msg = frame(name, Some(channel), data.clone());

        // Brief shard lock: clone the subscriber handles (Sender + Notify Arc
        // bumps only — no allocation per subscriber), release before sending.
        let targets: Vec<crate::state::Sub> = match state
            .channels
            .get(&(app_id.to_string(), channel.clone()))
        {
            Some(cs) => cs
                .subscribers
                .iter()
                .filter(|(sid, _)| Some(sid.as_str()) != except)
                .map(|(_, sub)| sub.clone())
                .collect(),
            None => continue,
        };

        let mut sent = 0u64;
        for sub in targets {
            // Non-blocking. Full buffer == slow consumer: kill it so it can't
            // drag down the fan-out for everyone else.
            if sub.tx.try_send(msg.clone()).is_err() {
                sub.kill.notify_one();
                state.metrics.slow_consumers_killed_total.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                sent += 1;
            }
        }
        state.metrics.messages_sent_total.fetch_add(sent, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// GET /metrics — Prometheus text exposition. Unauthenticated by convention;
/// firewall it or keep the port internal in production.
pub async fn metrics(AxState(state): AxState<Arc<State>>) -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let m = &state.metrics;
    format!(
        "# TYPE resonance_connections gauge\n\
         resonance_connections {}\n\
         # TYPE resonance_channels gauge\n\
         resonance_channels {}\n\
         # TYPE resonance_connections_total counter\n\
         resonance_connections_total {}\n\
         # TYPE resonance_events_received_total counter\n\
         resonance_events_received_total {}\n\
         # TYPE resonance_messages_sent_total counter\n\
         resonance_messages_sent_total {}\n\
         # TYPE resonance_slow_consumers_killed_total counter\n\
         resonance_slow_consumers_killed_total {}\n",
        state.connections.len(),
        state.channels.len(),
        m.connections_total.load(Relaxed),
        m.events_received_total.load(Relaxed),
        m.messages_sent_total.load(Relaxed),
        m.slow_consumers_killed_total.load(Relaxed),
    )
}

/// GET /apps/{app_id}/channels — occupied channels (optional ?filter_by_prefix=).
pub async fn channels_index(
    AxState(state): AxState<Arc<State>>,
    Path(app_id): Path<String>,
    Query(q): Query<BTreeMap<String, String>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let app = match state.app_by_id(&app_id) {
        Some(a) => a.clone(),
        None => return err(404, "Unknown app"),
    };
    if let Err(msg) = verify(&app.key, &app.secret, "GET", &format!("/apps/{app_id}/channels"), &q, b"") {
        return err(401, msg);
    }
    let prefix = q.get("filter_by_prefix").map(String::as_str).unwrap_or("");
    let want_user_count = q.get("info").map(|i| i.contains("user_count")).unwrap_or(false);

    let mut out = serde_json::Map::new();
    for e in state.channels.iter() {
        let (aid, name) = e.key();
        if aid != &app.id || !name.starts_with(prefix) {
            continue;
        }
        let mut info = serde_json::Map::new();
        if want_user_count {
            if let Some(p) = &e.value().presence {
                let users: std::collections::HashSet<&str> =
                    p.values().map(|m| m.user_id.as_str()).collect();
                info.insert("user_count".into(), users.len().into());
            }
        }
        out.insert(name.clone(), serde_json::Value::Object(info));
    }
    (StatusCode::OK, Json(serde_json::json!({"channels": out})))
}

/// GET /apps/{app_id}/channels/{name} — occupancy + counts for one channel.
pub async fn channel_show(
    AxState(state): AxState<Arc<State>>,
    Path((app_id, name)): Path<(String, String)>,
    Query(q): Query<BTreeMap<String, String>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let app = match state.app_by_id(&app_id) {
        Some(a) => a.clone(),
        None => return err(404, "Unknown app"),
    };
    if let Err(msg) = verify(&app.key, &app.secret, "GET", &format!("/apps/{app_id}/channels/{name}"), &q, b"") {
        return err(401, msg);
    }
    let mut out = serde_json::Map::new();
    match state.channels.get(&(app.id.clone(), name.clone())) {
        Some(cs) => {
            out.insert("occupied".into(), true.into());
            out.insert("subscription_count".into(), cs.subscribers.len().into());
            if let Some(p) = &cs.presence {
                let users: std::collections::HashSet<&str> =
                    p.values().map(|m| m.user_id.as_str()).collect();
                out.insert("user_count".into(), users.len().into());
            }
        }
        None => {
            out.insert("occupied".into(), false.into());
        }
    }
    (StatusCode::OK, Json(serde_json::Value::Object(out)))
}

/// GET /apps/{app_id}/channels/{name}/users — presence member ids.
pub async fn channel_users(
    AxState(state): AxState<Arc<State>>,
    Path((app_id, name)): Path<(String, String)>,
    Query(q): Query<BTreeMap<String, String>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let app = match state.app_by_id(&app_id) {
        Some(a) => a.clone(),
        None => return err(404, "Unknown app"),
    };
    if let Err(msg) = verify(&app.key, &app.secret, "GET", &format!("/apps/{app_id}/channels/{name}/users"), &q, b"") {
        return err(401, msg);
    }
    if !name.starts_with("presence-") {
        return err(400, "Users can only be retrieved for presence channels");
    }
    let mut seen = std::collections::HashSet::new();
    let mut users = Vec::new();
    if let Some(cs) = state.channels.get(&(app.id.clone(), name.clone())) {
        if let Some(p) = &cs.presence {
            for m in p.values() {
                if seen.insert(m.user_id.clone()) {
                    users.push(serde_json::json!({"id": m.user_id}));
                }
            }
        }
    }
    (StatusCode::OK, Json(serde_json::json!({"users": users})))
}

/// Verify the Pusher REST auth scheme (what pusher-php-server generates).
fn verify(
    key: &str,
    secret: &str,
    method: &str,
    path: &str,
    q: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<(), &'static str> {
    let auth_key = q.get("auth_key").ok_or("Missing auth_key")?;
    if auth_key != key {
        return Err("Bad auth_key");
    }

    let ts: u64 = q
        .get("auth_timestamp")
        .and_then(|t| t.parse().ok())
        .ok_or("Missing auth_timestamp")?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if now.abs_diff(ts) > 600 {
        return Err("Stale auth_timestamp");
    }

    // Required whenever there IS a body: the HMAC only covers the query string,
    // so without body_md5 the body would be unauthenticated (replayable with a
    // swapped payload). GET requests have no body and no body_md5.
    if !body.is_empty() {
        let expected_md5 = q.get("body_md5").ok_or("Missing body_md5")?;
        let got = hex::encode(Md5::digest(body));
        if got.as_bytes().ct_eq(expected_md5.as_bytes()).unwrap_u8() == 0 {
            return Err("body_md5 mismatch");
        }
    }

    let provided = q.get("auth_signature").ok_or("Missing auth_signature")?;

    // string_to_sign = METHOD\nPATH\n<params sorted by key, excluding auth_signature>
    let params: Vec<String> = q
        .iter()
        .filter(|(k, _)| k.as_str() != "auth_signature")
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    let string_to_sign = format!("{method}\n{path}\n{}", params.join("&"));
    let expected = sign(secret.as_bytes(), string_to_sign.as_bytes());

    if provided.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 0 {
        return Err("Signature mismatch");
    }
    Ok(())
}

fn err(code: u16, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST),
        Json(serde_json::json!({ "error": msg })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirror how pusher-php-server builds the signed request, then verify().
    fn signed(secret: &str, key: &str, body: &[u8], ts: u64) -> BTreeMap<String, String> {
        let mut q = BTreeMap::new();
        q.insert("auth_key".into(), key.into());
        q.insert("auth_timestamp".into(), ts.to_string());
        q.insert("auth_version".into(), "1.0".into());
        q.insert("body_md5".into(), hex::encode(Md5::digest(body)));
        let params: Vec<String> = q.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let sts = format!("POST\n/apps/app1/events\n{}", params.join("&"));
        q.insert("auth_signature".into(), sign(secret.as_bytes(), sts.as_bytes()));
        q
    }

    #[test]
    fn accepts_valid_and_rejects_tampering() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let body = br#"{"name":"e","channel":"c","data":"{}"}"#;
        let q = signed("secret", "key", body, now);

        assert!(verify("key", "secret", "POST", "/apps/app1/events", &q, body).is_ok());

        // Wrong secret -> signature mismatch.
        assert!(verify("key", "nope", "POST", "/apps/app1/events", &q, body).is_err());
        // Tampered body -> md5 mismatch.
        assert!(verify("key", "secret", "POST", "/apps/app1/events", &q, b"other").is_err());
        // Stale timestamp.
        let old = signed("secret", "key", body, now - 601);
        assert!(verify("key", "secret", "POST", "/apps/app1/events", &old, body).is_err());
        // body_md5 must be present — otherwise the body is unauthenticated.
        let mut no_md5 = signed("secret", "key", body, now);
        no_md5.remove("body_md5");
        assert!(verify("key", "secret", "POST", "/apps/app1/events", &no_md5, body).is_err());
    }

    // Adversarial sweep: verify() and publish() must never panic and never
    // accept garbage, whatever the query/payload shape.
    #[test]
    fn hostile_inputs_never_panic_never_authenticate() {
        let state = crate::state::State::new(
            vec![crate::state::App {
                id: "a".into(),
                key: "k".into(),
                secret: "s".into(),
                max_connections: 0,
                webhook_url: None,
            }],
            crate::state::Limits {
                max_message_size: 10240,
                activity_timeout_s: 120,
                max_channels_per_connection: 100,
                allowed_origins: vec![],
                client_event_rate: 10,
            },
            None,
        );

        let mut lcg: u64 = 0xC0FFEE;
        let mut junk = String::new();
        for _ in 0..3000 {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            junk.push(char::from_u32((lcg % 0x2FFFF) as u32).unwrap_or('\u{FFFD}'));
            if junk.len() > 200 {
                junk.clear();
            }
            // hostile query maps
            let mut q = BTreeMap::new();
            q.insert(junk.clone(), junk.clone());
            q.insert("auth_key".into(), junk.clone());
            q.insert("auth_timestamp".into(), junk.clone());
            q.insert("auth_signature".into(), junk.clone());
            q.insert("body_md5".into(), junk.clone());
            assert!(verify("k", "s", "POST", &junk, &q, junk.as_bytes()).is_err());

            // hostile publish payloads
            let payloads = [
                serde_json::json!({"name": junk, "data": junk, "channel": junk}),
                serde_json::json!({"name": null, "data": {"deep": [[[[junk.clone()]]]]}, "channels": [junk, 42, null]}),
                serde_json::json!({"channels": junk, "socket_id": {"x": 1}}),
                serde_json::json!([junk]),
                serde_json::json!(junk),
            ];
            for p in &payloads {
                let _ = publish(&state, "a", p); // Err or Ok, but no panic
            }
        }
    }
}
