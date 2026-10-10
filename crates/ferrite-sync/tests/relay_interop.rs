//! Live interop against the real Go relay (`relay/`).
//!
//! Hermetic by default: this file compiles always but runs ONLY with
//! `FERRITE_LIVE_RELAY=ws://host:port/sync` set, so CI without a relay
//! stays green. To prove interop manually:
//!
//! ```powershell
//! cd relay
//! $env:FERRITE_RELAY_ALLOW_LOOPBACK=1; go run . # :8080 /sync, TURN :3478
//! cd ..
//! $env:FERRITE_LIVE_RELAY="ws://127.0.0.1:8080/sync"
//! cargo test -p ferrite-sync --test relay_interop
//! ```
//!
//! What it proves: our handshake+auth+signal path satisfies the real
//! server's pinning, spoofing, routing, and 404 rules — the in-tree
//! harness only proves we satisfy ourselves.

use ferrite_sync::{
    RelayConnection, RelayLink, ServeOutcome, crdt_payload, parse_crdt_payload,
    sync_actions::CrdtMessage,
};
use std::sync::Arc;
use std::time::Duration;

fn live_url() -> Option<String> {
    std::env::var("FERRITE_LIVE_RELAY")
        .ok()
        .filter(|u| !u.trim().is_empty())
}

fn test_identity() -> (ferrite_sync::DeviceIdentity, String) {
    let id = ferrite_sync::DeviceIdentity::generate().expect("identity");
    let name = id.device_id.to_string();
    (id, name)
}

#[tokio::test]
async fn live_auth_and_signal_round_trip() {
    let Some(url) = live_url() else {
        eprintln!("SKIP: set FERRITE_LIVE_RELAY to run live interop");
        return;
    };
    let (a, name_a) = test_identity();
    let (b, name_b) = test_identity();
    let mut conn_a = RelayConnection::connect(&url, &name_a, &a)
        .await
        .expect("live auth a");
    let mut conn_b = RelayConnection::connect(&url, &name_b, &b)
        .await
        .expect("live auth b");
    conn_a
        .send_signal("signal", &name_b, serde_json::json!({"live": 1}))
        .await
        .expect("live send");
    let got = tokio::time::timeout(Duration::from_secs(10), conn_b.recv_signal())
        .await
        .expect("live recv on time")
        .expect("live signal");
    assert_eq!(got.from, name_a);
    assert_eq!(got.payload.get("live").and_then(|v| v.as_i64()), Some(1));
}

#[tokio::test]
async fn live_link_request_response() {
    let Some(url) = live_url() else {
        eprintln!("SKIP: set FERRITE_LIVE_RELAY to run live interop");
        return;
    };
    let (a, name_a) = test_identity();
    let (b, name_b) = test_identity();
    let link_a = Arc::new(RelayLink::new(vec![url.clone()], name_a.clone(), a));
    let mut conn_b = RelayConnection::connect(&url, &name_b, &b)
        .await
        .expect("live auth b");
    // Responder: answer CRDT requests, ignore the rest.
    let responder = tokio::spawn(async move {
        loop {
            let Ok(sig) = conn_b.recv_signal().await else {
                return;
            };
            let Some(msg) = parse_crdt_payload(&sig.payload) else {
                continue;
            };
            if !matches!(msg, CrdtMessage::SyncRequest { .. }) {
                continue;
            }
            let reply = crdt_payload(&CrdtMessage::SyncResponse { deltas: vec![] });
            if conn_b
                .send_signal("signal", &sig.from, reply)
                .await
                .is_err()
            {
                return;
            }
        }
    });
    let serve_link = Arc::clone(&link_a);
    let serve = tokio::spawn(async move {
        loop {
            if serve_link
                .serve_once(|_| true, |_, _| None, |_, _| {})
                .await
                == ServeOutcome::Gone
            {
                break;
            }
        }
    });
    let req = CrdtMessage::SyncRequest {
        since_vector_clock: Default::default(),
        enabled_categories: Default::default(),
    };
    let reply = tokio::time::timeout(Duration::from_secs(20), link_a.request(&name_b, &req))
        .await
        .expect("live round on time")
        .expect("live round trip");
    assert!(matches!(reply, CrdtMessage::SyncResponse { .. }));
    responder.abort();
    serve.abort();
}

#[tokio::test]
async fn live_store_drain_ack() {
    use std::sync::Arc;
    let Some(url) = live_url() else {
        eprintln!("SKIP: set FERRITE_LIVE_RELAY to run live interop");
        return;
    };
    let (a, name_a) = test_identity();
    let (b, name_b) = test_identity();
    // B offline: store mail for it with no session live.
    let link_a = Arc::new(RelayLink::new(vec![url.clone()], name_a.clone(), a));
    let payload = ferrite_sync::crdt_payload(&CrdtMessage::SyncResponse { deltas: vec![] });
    let id = link_a
        .store(&name_b, payload)
        .await
        .expect("live store accepted");
    assert!(!id.is_empty());
    // B connects raw: the drain arrives tagged with the slot id.
    let mut conn_b = RelayConnection::connect(&url, &name_b, &b)
        .await
        .expect("live auth b");
    let got = tokio::time::timeout(Duration::from_secs(10), conn_b.recv_signal())
        .await
        .expect("drain on time")
        .expect("drained signal");
    assert_eq!(got.blob.as_deref(), Some(id.as_str()));
    assert_eq!(got.from, name_a);
    // Ack raw; a fresh session drains nothing further.
    let ack = serde_json::json!({
        "type": "ack", "from": name_b, "to": "",
        "payload": {"blob": id}, "ts": 1,
    });
    conn_b.send_json(&ack).await.expect("live ack");
    drop(conn_b);
    let mut conn_b2 = RelayConnection::connect(&url, &name_b, &b)
        .await
        .expect("live re-auth b");
    let nothing = tokio::time::timeout(Duration::from_secs(2), conn_b2.recv_signal()).await;
    assert!(nothing.is_err(), "acked slot must not redeliver");
}
