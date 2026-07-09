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

    let name = payload.get("name").and_then(|n| n.as_str()).unwrap_or("");
    // data is already a JSON-encoded string from the client.
    let data = match payload.get("data") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => return err(400, "Missing data"),
    };
    let except = payload.get("socket_id").and_then(|s| s.as_str());

    let mut channels: Vec<String> = Vec::new();
    if let Some(c) = payload.get("channel").and_then(|c| c.as_str()) {
        channels.push(c.to_string());
    }
    if let Some(arr) = payload.get("channels").and_then(|c| c.as_array()) {
        channels.extend(arr.iter().filter_map(|c| c.as_str()).map(String::from));
    }

    for channel in &channels {
        // Serialize the outgoing frame ONCE per channel; each subscriber gets a
        // cheap refcounted clone (Utf8Bytes/Bytes), never a re-serialization.
        let msg = frame(name, Some(channel), data.clone());

        // Brief shard lock: clone the senders, then release before sending.
        let targets: Vec<(String, tokio::sync::mpsc::Sender<_>)> = match state
            .channels
            .get(&(app.id.clone(), channel.clone()))
        {
            Some(cs) => cs
                .subscribers
                .iter()
                .filter(|(sid, _)| Some(sid.as_str()) != except)
                .map(|(sid, tx)| (sid.clone(), tx.clone()))
                .collect(),
            None => continue,
        };

        for (sid, tx) in targets {
            // Non-blocking. Full buffer == slow consumer: kill it so it can't
            // drag down the fan-out for everyone else.
            if tx.try_send(msg.clone()).is_err() {
                if let Some(conn) = state.connections.get(&sid) {
                    conn.kill.notify_one();
                }
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({})))
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

    if let Some(expected_md5) = q.get("body_md5") {
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
    }
}
