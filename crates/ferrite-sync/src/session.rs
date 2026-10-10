//! Live LAN sync session: inbound accept loop plus dial-out rounds over
//! pinned QUIC. Trust comes from the encrypted trust store (pairing UI);
//! the mTLS handshake enforces it, discovery pre-filtering only avoids
//! dialing strangers on hostile LANs. No relay client yet (Phase 2b):
//! both devices must share a LAN, matching the decided both-online-or-
//! nothing semantics. Nothing here persists besides the vault rows the
//! CRDT engine owns (deltas, clock); presence stays a TTL inbox.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tracing::{debug, warn};

use crate::crdt_engine::CrdtEngine;
use crate::crypto::DeviceIdentity;
use crate::discovery::{DiscoveredDevice, DiscoveryConfig, DiscoveryService};
use crate::multiplex::{MAX_FRAME_BYTES, decode, encode_crdt};
use crate::sync_actions::{CrdtMessage, LiveTabAction, SyncCategory};
use crate::transport::{P2pConfig, P2pTransport};
use crate::trusted::TrustedStore;

/// Per-peer dial + round budget (LAN failures must surface fast).
const PEER_TIMEOUT: Duration = Duration::from_secs(30);
/// QUIC connect budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Idle budget per accepted stream and per accepted connection wait.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Max inbound streams served per connection (DoS bound, generous: a sync
/// round needs one).
const MAX_STREAMS_PER_CONN: usize = 16;
/// Max frames decoded per stream (8MB each worst case, deltas cap lower).
const MAX_FRAMES_PER_STREAM: usize = 64;
/// Peer dial-backoff after a failure (flapping peers must not spin rounds).
const PEER_BACKOFF: Duration = Duration::from_secs(60);

/// What one round did. Surfaced to settings/diagnostics; `actions_received`
/// counts SyncResponse actions handed to the engine (apply is atomic per
/// batch and warns on rollback; a counted-then-rolled-back batch still
/// refreshes the UI, which is idempotent). `deferred` counts actions
/// stored as TTL blobs for offline peers (not yet delivered: waiting,
/// not lost).
#[derive(Debug, Clone, Default)]
pub struct RoundReport {
    pub peers_seen: usize,
    pub peers_synced: usize,
    pub actions_received: usize,
    pub deferred: usize,
    pub at_ms: i64,
}

pub struct SyncSession {
    engine: Mutex<CrdtEngine>,
    discovery: DiscoveryService,
    transport: tokio::sync::Mutex<P2pTransport>,
    trusted: Arc<Mutex<TrustedStore>>,
    settings: Arc<RwLock<crate::selective::SyncSettings>>,
    device_id: String,
    local_tab: Mutex<Option<(String, i64)>>,
    backoff: Mutex<HashMap<String, Instant>>,
    last_round: Mutex<Option<RoundReport>>,
    /// Off-LAN transport (Go relay via `RelayLink`). None until the user
    /// configures relay URLs: LAN-only operation is the default, and an
    /// absent relay changes nothing about LAN rounds.
    relay: Mutex<Option<Arc<crate::relay_client::RelayLink>>>,
    /// Per-peer high-water mark of deltas already stored as TTL blobs
    /// (memory-only: a restart re-stores bounded full deltas once;
    /// idempotent on apply, so the cost is bandwidth, never correctness).
    relay_stored: Mutex<HashMap<String, i64>>,
}

impl SyncSession {
    /// Bind QUIC, advertise + browse mDNS, and return the live session.
    /// Fails closed with a reason when the platform refuses (bind, mDNS):
    /// the shell runs transport-off and says so in settings.
    pub async fn start(
        conn: Arc<Mutex<Connection>>,
        identity: &DeviceIdentity,
        device_name: String,
        settings: Arc<RwLock<crate::selective::SyncSettings>>,
    ) -> Result<Arc<Self>, String> {
        let device_id = identity.device_id.to_string();
        let engine = CrdtEngine::new(Arc::clone(&conn), device_id.clone());
        let trusted = Arc::new(Mutex::new(TrustedStore::new(Arc::clone(&conn))));
        trusted
            .lock()
            .map_err(|e| format!("trust lock: {e}"))?
            .init_schema()
            .map_err(|e| format!("trust schema: {e}"))?;
        let mut transport = P2pTransport::new(P2pConfig::new(device_id.clone(), 0));
        let port = transport
            .bind(identity, Arc::clone(&trusted))
            .await
            .map_err(|e| format!("quic bind: {e}"))?;
        let discovery = DiscoveryService::new(DiscoveryConfig {
            device_id: device_id.clone(),
            device_name,
            fingerprint: identity.fingerprint(),
            port,
        })
        .map_err(|e| format!("discovery: {e}"))?;
        discovery
            .broadcast()
            .map_err(|e| format!("advertise: {e}"))?;
        discovery
            .start_browsing()
            .map_err(|e| format!("browse: {e}"))?;
        Ok(Arc::new(Self {
            engine: Mutex::new(engine),
            discovery,
            transport: tokio::sync::Mutex::new(transport),
            trusted,
            settings,
            device_id,
            local_tab: Mutex::new(None),
            backoff: Mutex::new(HashMap::new()),
            last_round: Mutex::new(None),
            relay: Mutex::new(None),
            relay_stored: Mutex::new(HashMap::new()),
        }))
    }

    /// Attach the off-LAN relay transport. Replaces any previous relay
    /// config (URLs are operator input: re-setting re-validates from
    /// scratch, and the old session's waiters fail fast via drop).
    /// Identity is cloned: the link signs auth claims per reconnect.
    pub fn set_relay(&self, urls: Vec<String>, identity: &DeviceIdentity) {
        let clean: Vec<String> = urls
            .into_iter()
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty())
            .take(8)
            .collect();
        if let Ok(mut slot) = self.relay.lock() {
            if clean.is_empty() {
                if let Some(old) = slot.take() {
                    old.drop_pending();
                }
            } else {
                *slot = Some(Arc::new(crate::relay_client::RelayLink::new(
                    clean,
                    self.device_id.clone(),
                    identity.clone(),
                )));
            }
        }
    }

    /// Remove the relay transport (back to LAN-only). Pending waiters
    /// fail fast via drop; the serve loop exits on its next step because
    /// there is nothing to serve.
    pub fn clear_relay(&self) {
        if let Ok(mut slot) = self.relay.lock()
            && let Some(old) = slot.take()
        {
            old.drop_pending();
        }
    }

    /// Relay presence snapshot for settings/diagnostics (URLs + whether
    /// the session is live: never secrets, never payload contents).
    pub fn relay_status(&self) -> Option<crate::relay_client::RelayStatus> {
        self.relay
            .lock()
            .ok()
            .and_then(|r| r.as_ref().map(|link| link.status()))
    }

    /// One sync round over every discovered, trusted, unbacked-off peer:
    /// CRDT request/response/apply plus presence fan-out, then clock persist
    /// when anything arrived. Never throws: per-peer failures back off.
    /// Every address of a peer is tried in order, sharing the per-peer
    /// budget: first reachable wins (mDNS often reports stale + live).
    /// Trusted peers NOT on LAN go through the relay instead (same shapes,
    /// same gates); relay counts fold into the same report.
    pub async fn round(&self) -> RoundReport {
        let pairs: Vec<(Vec<SocketAddr>, String)> = self
            .discovery
            .snapshot()
            .iter()
            .map(|p| {
                (
                    p.addresses
                        .iter()
                        .map(|ip| SocketAddr::new(*ip, p.port))
                        .collect::<Vec<_>>(),
                    p.device_id.clone(),
                )
            })
            .filter(|(addrs, _)| !addrs.is_empty())
            .collect();
        let mut report = self.sync_known_peers(&pairs).await;
        let (relayed, actions, deferred) = self.relay_round().await;
        report.peers_synced += relayed;
        report.actions_received += actions;
        report.deferred += deferred;
        if let Ok(mut last) = self.last_round.lock() {
            *last = Some(report.clone());
        }
        report
    }

    /// Round core over explicit peers (same gates as `round`: self-skip,
    /// trust pre-filter, backoff). Tests drive this directly (no multicast
    /// needed), while production feeds it discovery snapshots. Each peer
    /// carries all its addresses in order; attempts share `PEER_TIMEOUT`
    /// and the first reachable wins.
    pub async fn sync_known_peers(&self, pairs: &[(Vec<SocketAddr>, String)]) -> RoundReport {
        let known: HashSet<String> = self
            .trusted
            .lock()
            .map(|t| {
                t.all()
                    .into_iter()
                    .filter(|d| !d.is_revoked)
                    .map(|d| d.device_id)
                    .collect()
            })
            .unwrap_or_default();
        let mut report = RoundReport {
            peers_seen: pairs.len(),
            at_ms: now_millis(),
            ..Default::default()
        };
        for (addrs, id) in pairs {
            if *id == self.device_id || !known.contains(id) {
                continue;
            }
            if addrs.is_empty() || self.backed_off(id) {
                continue;
            }
            match tokio::time::timeout(PEER_TIMEOUT, self.sync_with_addrs(addrs, id)).await {
                Ok(Ok(n)) => {
                    report.peers_synced += 1;
                    report.actions_received += n;
                    self.clear_backoff(id);
                }
                Ok(Err(e)) => {
                    debug!(peer = %id, error = %e, "sync round peer failed");
                    self.note_backoff(id);
                }
                Err(_) => {
                    debug!(peer = %id, "sync round peer timed out");
                    self.note_backoff(id);
                }
            }
        }
        if report.actions_received > 0
            && let Ok(engine) = self.engine.lock()
        {
            engine.persist_clock();
        }
        report.at_ms = now_millis();
        if let Ok(mut last) = self.last_round.lock() {
            *last = Some(report.clone());
        }
        report
    }

    /// Sync with one peer across all its addresses in order: first
    /// reachable wins. Called inside the shared per-peer `PEER_TIMEOUT`,
    /// so the budget covers every attempt (no per-address timeout reset).
    async fn sync_with_addrs(&self, addrs: &[SocketAddr], peer_id: &str) -> Result<usize, String> {
        let mut last_err = "no addresses".to_owned();
        for addr in addrs {
            match self.sync_with(addr, peer_id).await {
                Ok(n) => return Ok(n),
                Err(e) => {
                    debug!(peer = %peer_id, addr = %addr, error = %e, "sync addr failed, trying next");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    /// Sync with one peer address: CRDT round-trip then presence push.
    /// Returns received action count. The mTLS handshake enforces trust
    /// (unknown keys fail here even if discovery was spoofed).
    pub async fn sync_with(&self, addr: &SocketAddr, peer_id: &str) -> Result<usize, String> {
        let conn = {
            let transport = self.transport.lock().await;
            tokio::time::timeout(CONNECT_TIMEOUT, transport.connect(*addr, peer_id))
                .await
                .map_err(|_| "connect timed out".to_owned())?
                .map_err(|e| format!("connect: {e}"))?
        };
        let (mut send, mut recv) = tokio::time::timeout(CONNECT_TIMEOUT, conn.open_bi())
            .await
            .map_err(|_| "open stream timed out".to_owned())?
            .map_err(|e| format!("open stream: {e}"))?;
        let (clock, enabled) = self
            .engine
            .lock()
            .map(|e| (e.clock_snapshot(), self.enabled_set()))
            .map_err(|e| format!("engine lock: {e}"))?;
        let request = CrdtMessage::SyncRequest {
            since_vector_clock: clock,
            enabled_categories: enabled,
        };
        write_frame(&mut send, &request).await?;
        // Incremental read: stop at the first SyncResponse (the only reply
        // a request earns). Anything else is handled in passing. Waiting for
        // a timeout here instead would stall every round to the full budget.
        let mut received = 0usize;
        let mut buf = Vec::new();
        let started = Instant::now();
        loop {
            if started.elapsed() > PEER_TIMEOUT {
                break;
            }
            let remaining = PEER_TIMEOUT
                .checked_sub(started.elapsed())
                .unwrap_or(Duration::ZERO);
            let Some(msg) = read_one_message(&mut recv, &mut buf, remaining).await else {
                break;
            };
            let is_response = matches!(msg, CrdtMessage::SyncResponse { .. });
            if let CrdtMessage::SyncResponse { deltas } = &msg {
                received += deltas.len();
            }
            if let Ok(mut engine) = self.engine.lock() {
                let _ = engine.handle_crdt_message(msg);
            }
            if is_response {
                break;
            }
        }
        // Presence fan-out addressed to this peer (ephemeral, TTL'd).
        if let Some((url, ts)) = self.local_tab.lock().ok().and_then(|g| g.clone()) {
            let presence = CrdtMessage::PushLiveTab {
                action: LiveTabAction::PushTab {
                    url,
                    source_device: self.device_id.clone(),
                    target_device: peer_id.to_owned(),
                    timestamp: ts,
                },
            };
            let _ = write_frame(&mut send, &presence).await;
        }
        let _ = send.finish();
        // Drain the responder's FIN before dropping the connection: without
        // this the close races our presence write out of existence and the
        // peer never sees it. Bounded (5s); a dead peer cannot stall a round.
        let mut fin_buf = Vec::new();
        let _ = read_one_message(&mut recv, &mut fin_buf, Duration::from_secs(5)).await;
        Ok(received)
    }

    /// Off-LAN round over the relay: every trusted peer NOT currently
    /// discovered on LAN gets a CRDT round trip through the signal
    /// transport, then a presence push. Same gates as LAN (self-skip,
    /// trust pre-filter, per-peer backoff, clock persist on arrival) and
    /// the same engine code paths; only the pipe differs. Returns
    /// (peers synced, actions received, actions deferred as TTL blobs)
    /// for folding into the report.
    /// No relay configured → (0, 0, 0) immediately, LAN untouched.
    pub async fn relay_round(&self) -> (usize, usize, usize) {
        let link = match self.relay.lock().ok().and_then(|r| r.clone()) {
            Some(link) => link,
            None => return (0, 0, 0),
        };
        let on_lan: HashSet<String> = self
            .discovery
            .snapshot()
            .iter()
            .map(|p| p.device_id.clone())
            .collect();
        let known: HashSet<String> = self
            .trusted
            .lock()
            .map(|t| {
                t.all()
                    .into_iter()
                    .filter(|d| !d.is_revoked)
                    .map(|d| d.device_id)
                    .collect()
            })
            .unwrap_or_default();
        let mut synced = 0usize;
        let mut actions = 0usize;
        let mut deferred = 0usize;
        for id in known {
            if id == self.device_id || on_lan.contains(&id) || self.backed_off(&id) {
                continue;
            }
            match tokio::time::timeout(PEER_TIMEOUT, self.sync_via_relay(&link, &id)).await {
                Ok(Ok((n, d))) => {
                    synced += 1;
                    actions += n;
                    deferred += d;
                    self.clear_backoff(&id);
                }
                Ok(Err(e)) => {
                    debug!(peer = %id, error = %e, "relay round peer failed");
                    self.note_backoff(&id);
                }
                Err(_) => {
                    debug!(peer = %id, "relay round peer timed out");
                    self.note_backoff(&id);
                }
            }
        }
        if actions > 0
            && let Ok(engine) = self.engine.lock()
        {
            engine.persist_clock();
        }
        (synced, actions, deferred)
    }

    /// One CRDT round trip with a peer over the relay: request our deltas,
    /// apply the reply, then fan out presence. Mirrors `sync_with` (same
    /// message shapes, same engine calls) minus the QUIC stream plumbing.
    /// When the peer is unreachable, new deltas are stored as a TTL blob
    /// instead of dropped: returns (applied, deferred). Deferred is not
    /// failure; it is mail waiting, reported separately so the UI can
    /// say "waiting for device" instead of failing the round.
    async fn sync_via_relay(
        &self,
        link: &Arc<crate::relay_client::RelayLink>,
        peer_id: &str,
    ) -> Result<(usize, usize), String> {
        let (clock, enabled) = self
            .engine
            .lock()
            .map(|e| (e.clock_snapshot(), self.enabled_set()))
            .map_err(|e| format!("engine lock: {e}"))?;
        let request = CrdtMessage::SyncRequest {
            since_vector_clock: clock,
            enabled_categories: enabled,
        };
        match link.request(peer_id, &request).await {
            Ok(reply) => {
                let mut received = 0usize;
                if let CrdtMessage::SyncResponse { deltas } = &reply {
                    received += deltas.len();
                }
                if let Ok(mut engine) = self.engine.lock() {
                    let _ = engine.handle_crdt_message(reply);
                }
                if let Some((url, ts)) = self.local_tab.lock().ok().and_then(|g| g.clone()) {
                    let presence = CrdtMessage::PushLiveTab {
                        action: LiveTabAction::PushTab {
                            url,
                            source_device: self.device_id.clone(),
                            target_device: peer_id.to_owned(),
                            timestamp: ts,
                        },
                    };
                    let _ = link
                        .send_to(peer_id, crate::relay_client::crdt_payload(&presence))
                        .await;
                }
                Ok((received, 0))
            }
            Err(_) => {
                // Peer unreachable (offline, or relay down): persist what
                // is new since the last stored batch as an addressed blob.
                // Empty means nothing new: not a failure, not deferred.
                let stored = self.store_for_offline(link, peer_id).await?;
                Ok((0, stored))
            }
        }
    }

    /// Persist unsynced deltas for an offline peer as one TTL blob shaped
    /// like a SyncResponse (the recipient's serve loop applies it exactly
    /// like a live reply: same code path, same LWW guards). Bounded by
    /// the engine's per-category/total delta caps; the per-peer watermark
    /// advances only on confirmed store, so a failed store retries the
    /// same batch next round instead of skipping it.
    async fn store_for_offline(
        &self,
        link: &Arc<crate::relay_client::RelayLink>,
        peer_id: &str,
    ) -> Result<usize, String> {
        let watermark = self
            .relay_stored
            .lock()
            .ok()
            .and_then(|m| m.get(peer_id).copied())
            .unwrap_or(0);
        let (deltas, max_hlc) = self
            .engine
            .lock()
            .map(|e| {
                let clock = [("stored".to_owned(), watermark as u64)]
                    .into_iter()
                    .collect();
                let deltas = e.deltas_for_peer(&clock, &self.enabled_set());
                let max = deltas
                    .iter()
                    .map(|a| a.hlc() as i64)
                    .max()
                    .unwrap_or(watermark);
                (deltas, max)
            })
            .map_err(|e| format!("engine lock: {e}"))?;
        if deltas.is_empty() {
            return Ok(0);
        }
        let count = deltas.len();
        let response = CrdtMessage::SyncResponse { deltas };
        link.store(peer_id, crate::relay_client::crdt_payload(&response))
            .await?;
        if let Ok(mut stored) = self.relay_stored.lock() {
            stored.insert(peer_id.to_owned(), max_hlc.max(watermark));
        }
        Ok(count)
    }

    /// Inbound relay serve loop: answers CRDT requests from trusted peers
    /// and applies presence, for as long as `stop` stays clear. The link
    /// owns the only reader; rounds borrow it only to write. Ping
    /// keepalive rides here too (the server drops sessions unread for
    /// 60s, and rounds alone may not talk that often). No relay
    /// configured → returns immediately, LAN serve unaffected.
    pub async fn serve_relay(self: &Arc<Self>, stop: &AtomicBool) {
        let link = match self.relay.lock().ok().and_then(|r| r.clone()) {
            Some(link) => link,
            None => return,
        };
        let mut last_ping = Instant::now();
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let outcome = link
                .serve_once(
                    |id| self.is_known_trusted(id),
                    |_from, msg| {
                        self.engine
                            .lock()
                            .ok()
                            .and_then(|mut engine| engine.handle_crdt_message(msg))
                    },
                    |_from, msg| {
                        if let Ok(mut engine) = self.engine.lock() {
                            let _ = engine.handle_crdt_message(msg);
                        }
                    },
                )
                .await;
            if outcome == crate::relay_client::ServeOutcome::Gone && link.status().connected {
                // Session died mid-read but something still claims live:
                // force the drop so the next step reconnects cleanly.
                link.drop_pending();
            }
            if last_ping.elapsed() > crate::relay_client::RELAY_PING_INTERVAL
                && link.ping().await.is_ok()
            {
                last_ping = Instant::now();
            }
        }
    }

    /// Trust pre-filter shared by the relay serve loop (strangers are
    /// dropped before their bytes reach the engine: same rule as the
    /// LAN dial pre-filter, enforced again at the mTLS handshake there).
    fn is_known_trusted(&self, id: &str) -> bool {
        self.trusted
            .lock()
            .map(|t| {
                t.all()
                    .into_iter()
                    .any(|d| d.device_id == id && !d.is_revoked)
            })
            .unwrap_or(false)
    }

    /// Inbound accept loop. Returns when `stop` flips; per-connection
    /// failures never escape (each conn is its own task). This loop is not
    /// optional: a QUIC endpoint that never polls accept() never answers
    /// handshakes, so production starts it with the session and tests run
    /// it on every responder; a deaf node syncs nothing, silently.
    pub async fn serve_forever(self: &Arc<Self>, stop: &AtomicBool) {
        let Some(endpoint) = self.transport.lock().await.endpoint() else {
            warn!("sync serve: transport unbound, serve loop exiting");
            return;
        };
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let conn = match tokio::time::timeout(Duration::from_secs(1), endpoint.accept()).await {
                Ok(Some(incoming)) => match incoming.await {
                    Ok(c) => c,
                    Err(_) => continue,
                },
                _ => continue,
            };
            let this = Arc::clone(self);
            tokio::spawn(async move {
                this.serve_conn(conn).await;
            });
        }
    }

    async fn serve_conn(&self, conn: quinn::Connection) {
        for _ in 0..MAX_STREAMS_PER_CONN {
            let (mut send, mut recv) =
                match tokio::time::timeout(IDLE_TIMEOUT, conn.accept_bi()).await {
                    Ok(Ok(s)) => s,
                    _ => return,
                };
            // Incremental dispatch: each frame is handled the moment it
            // completes (responses go out while the peer is still reading),
            // never batched to a timeout. Batching both sides to a timeout
            // deadlocks the round: each side would wait out the other.
            let mut buf = Vec::new();
            let mut frames = 0usize;
            loop {
                if frames >= MAX_FRAMES_PER_STREAM {
                    break;
                }
                let Some(msg) = read_one_message(&mut recv, &mut buf, IDLE_TIMEOUT).await else {
                    break;
                };
                frames += 1;
                let response = self
                    .engine
                    .lock()
                    .ok()
                    .and_then(|mut engine| engine.handle_crdt_message(msg));
                if let Some(reply) = response
                    && write_frame(&mut send, &reply).await.is_err()
                {
                    return;
                }
            }
            // Graceful stream end: our FIN tells the peer we answered
            // everything it sent, so its connection teardown cannot race
            // our last write out of existence (fire-and-forget tails like
            // presence would otherwise lose to a fast conn drop).
            let _ = send.finish();
        }
    }

    /// Record this device's current tab for presence fan-out. Only http(s)
    /// pages, bounded: anything else is ignored (never an error; navigation
    /// must not fail because sync exists).
    pub fn push_local_tab(&self, url: &str) {
        let url = url.trim();
        let lower = url.to_lowercase();
        if url.is_empty()
            || url.len() > 2048
            || !(lower.starts_with("http://") || lower.starts_with("https://"))
        {
            return;
        }
        if let Ok(mut slot) = self.local_tab.lock() {
            *slot = Some((url.to_owned(), now_millis()));
        }
    }

    /// Snapshot of unexpired live-tab pushes from the mesh.
    pub fn live_tabs(&self) -> Vec<LiveTabAction> {
        self.engine
            .lock()
            .map(|mut e| e.live_tabs())
            .unwrap_or_default()
    }

    /// Currently discovered LAN peers (trusted or not: trust gates sync,
    /// not listing; strangers never see this list leave the device).
    pub fn peers(&self) -> Vec<DiscoveredDevice> {
        self.discovery.snapshot()
    }

    pub fn last_round(&self) -> Option<RoundReport> {
        self.last_round.lock().ok().and_then(|g| g.clone())
    }

    fn enabled_set(&self) -> HashSet<SyncCategory> {
        self.settings
            .read()
            .map(|s| s.enabled_set())
            .unwrap_or_default()
    }

    fn backed_off(&self, id: &str) -> bool {
        self.backoff.lock().is_ok_and(|mut m| {
            m.retain(|_, at| at.elapsed() < PEER_BACKOFF);
            m.contains_key(id)
        })
    }

    fn note_backoff(&self, id: &str) {
        if let Ok(mut m) = self.backoff.lock() {
            m.insert(id.to_owned(), Instant::now());
        }
    }

    fn clear_backoff(&self, id: &str) {
        if let Ok(mut m) = self.backoff.lock() {
            m.remove(id);
        }
    }
}

async fn write_frame(send: &mut quinn::SendStream, msg: &CrdtMessage) -> Result<(), String> {
    let bytes = encode_crdt(msg);
    if bytes.len() > MAX_FRAME_BYTES + 5 {
        return Err("frame exceeds cap".to_owned());
    }
    send.write_all(&bytes)
        .await
        .map_err(|e| format!("write: {e}"))?;
    Ok(())
}

/// Read exactly one framed message, accumulating into `buf` across calls
/// (callers reuse the buffer for the stream's lifetime). Returns None on
/// FIN, error, oversize, or idle budget exhaustion: all normal ways a
/// peer says "done for now", never a failure by itself.
async fn read_one_message(
    recv: &mut quinn::RecvStream,
    buf: &mut Vec<u8>,
    budget: Duration,
) -> Option<CrdtMessage> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some((crate::multiplex::Decoded::Crdt(msg), consumed)) = decode(buf) {
            buf.drain(..consumed);
            return Some(msg);
        }
        if buf.len() > MAX_FRAME_BYTES + 5 {
            buf.clear();
            return None;
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            return None;
        }
        let mut chunk = [0u8; 16 * 1024];
        match tokio::time::timeout(remaining, recv.read(&mut chunk)).await {
            Ok(Ok(Some(n))) => buf.extend_from_slice(&chunk[..n]),
            _ => return None,
        }
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selective::SyncSettings;

    fn memory_conn() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE bookmarks(id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL,
              parent_folder_id TEXT, sort_order REAL DEFAULT 0, hlc_timestamp INTEGER NOT NULL, is_deleted INTEGER DEFAULT 0);
             CREATE TABLE history(id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT,
              visit_count INTEGER DEFAULT 1, last_visit_timestamp INTEGER NOT NULL, hlc_timestamp INTEGER NOT NULL);
             CREATE TABLE web_snapshots(id TEXT PRIMARY KEY, url TEXT NOT NULL, capture_timestamp INTEGER NOT NULL,
              compressed_html BLOB NOT NULL, is_pinned INTEGER DEFAULT 0, hlc_timestamp INTEGER NOT NULL);
             CREATE TABLE credentials(id TEXT PRIMARY KEY, url TEXT NOT NULL, username TEXT NOT NULL,
              encrypted_password BLOB NOT NULL, hlc_timestamp INTEGER NOT NULL, is_deleted INTEGER DEFAULT 0);
             CREATE TABLE autofill_records(id TEXT PRIMARY KEY, record_type TEXT NOT NULL, label TEXT NOT NULL,
              encrypted_fields BLOB NOT NULL, hlc_timestamp INTEGER NOT NULL, is_deleted INTEGER DEFAULT 0);
             CREATE TABLE totp_secrets(credential_id TEXT PRIMARY KEY, encrypted_secret BLOB NOT NULL,
              digits INTEGER NOT NULL, period_seconds INTEGER NOT NULL);
             CREATE TABLE trusted_devices(device_id TEXT PRIMARY KEY, device_name TEXT NOT NULL,
              public_key BLOB NOT NULL, paired_via_device_id TEXT, first_paired_at INTEGER NOT NULL,
              last_seen_at INTEGER, last_synced_at INTEGER, hlc_timestamp INTEGER NOT NULL, is_revoked INTEGER DEFAULT 0);
             CREATE TABLE sync_vector_clock(device_id TEXT PRIMARY KEY, counter INTEGER NOT NULL);",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn trust_peer(store: &TrustedStore, peer: &DeviceIdentity) {
        store
            .add_trusted(&crate::trusted::TrustedDevice {
                device_id: peer.device_id.to_string(),
                device_name: "peer".to_owned(),
                public_key: peer.public_key_bytes(),
                paired_via_device_id: None,
                first_paired_at: 1,
                last_seen_at: None,
                last_synced_at: None,
                hlc_timestamp: 1,
                is_revoked: false,
            })
            .unwrap();
    }

    /// Session bound to loopback without touching mDNS (multicast is
    /// unavailable in sandboxes): returns None when even the daemon fails,
    /// and tests skip instead of failing on platform networking.
    async fn session_on(
        conn: Arc<Mutex<Connection>>,
        identity: &DeviceIdentity,
    ) -> Option<(Arc<SyncSession>, std::net::SocketAddr)> {
        let engine = CrdtEngine::new(Arc::clone(&conn), identity.device_id.to_string());
        let mut transport = P2pTransport::new(P2pConfig::new(identity.device_id.to_string(), 0));
        let port = transport
            .bind(
                identity,
                Arc::new(Mutex::new(TrustedStore::new(Arc::clone(&conn)))),
            )
            .await
            .ok()?;
        let discovery = DiscoveryService::new(crate::discovery::DiscoveryConfig {
            device_id: identity.device_id.to_string(),
            device_name: "test".to_owned(),
            fingerprint: identity.fingerprint(),
            port,
        })
        .ok()?;
        let session = Arc::new(SyncSession {
            engine: Mutex::new(engine),
            discovery,
            transport: tokio::sync::Mutex::new(transport),
            trusted: Arc::new(Mutex::new(TrustedStore::new(conn))),
            settings: Arc::new(RwLock::new(SyncSettings::default())),
            device_id: identity.device_id.to_string(),
            local_tab: Mutex::new(None),
            backoff: Mutex::new(HashMap::new()),
            last_round: Mutex::new(None),
            relay: Mutex::new(None),
            relay_stored: Mutex::new(HashMap::new()),
        });
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        Some((session, addr))
    }

    #[tokio::test]
    async fn pair_syncs_bookmark_and_presence_over_loopback() {
        let alpha_id = DeviceIdentity::generate().unwrap();
        let beta_id = DeviceIdentity::generate().unwrap();
        let (ca, cb) = (memory_conn(), memory_conn());
        // Seed one bookmark on alpha with a nonzero HLC (watermark starts 0).
        ca.lock()
            .unwrap()
            .execute(
                "INSERT INTO bookmarks(id,url,title,hlc_timestamp,is_deleted) VALUES('bm1','https://example.com/a','A',10,0)",
                [],
            )
            .unwrap();
        let Some((alpha, addr_a)) = session_on(ca, &alpha_id).await else {
            return;
        };
        let Some((beta, addr_b)) = session_on(cb, &beta_id).await else {
            return;
        };
        // Mutual trust rows in the SESSIONS' own stores (pre-filter skips
        // strangers before dialing; the handshake enforces the key).
        let beta_id_s = beta_id.device_id.to_string();
        let alpha_id_s = alpha_id.device_id.to_string();
        {
            let t = alpha.trusted.lock().unwrap();
            trust_peer(&t, &beta_id);
        }
        {
            let t = beta.trusted.lock().unwrap();
            trust_peer(&t, &alpha_id);
        }
        // Both sides serve: a QUIC endpoint that never polls accept() never
        // answers handshakes (same reason production starts serve_forever
        // with the session; a deaf node syncs nothing).
        let stop = Arc::new(AtomicBool::new(false));
        let serve = |s: Arc<SyncSession>| {
            let stop = Arc::clone(&stop);
            tokio::spawn(async move { s.serve_forever(&stop).await })
        };
        let (alpha_serve, beta_serve) = (serve(Arc::clone(&alpha)), serve(Arc::clone(&beta)));
        // Alpha's round pushes presence addressed at beta; beta's rows flow
        // the other way. Mesh sync is symmetric: each side pulls.
        alpha.push_local_tab("https://example.com/live");
        let report = alpha.sync_known_peers(&[(vec![addr_b], beta_id_s)]).await;
        assert_eq!(report.peers_seen, 1);
        assert_eq!(report.peers_synced, 1);
        assert!(report.actions_received >= 1, "trust delta arrived");
        let tabs = beta.live_tabs();
        assert_eq!(tabs.len(), 1);
        match &tabs[0] {
            LiveTabAction::PushTab { url, .. } => assert_eq!(url, "https://example.com/live"),
        }
        let back = beta.sync_known_peers(&[(vec![addr_a], alpha_id_s)]).await;
        assert_eq!(back.peers_synced, 1);
        assert!(back.actions_received >= 1, "bookmark delta arrived");
        stop.store(true, Ordering::Relaxed);
        let _ = beta_serve.await;
        let _ = alpha_serve.await;
        // Beta applied the bookmark: its clock advanced past the delta.
        let watermark = beta
            .engine
            .lock()
            .unwrap()
            .clock_snapshot()
            .values()
            .copied()
            .max()
            .unwrap_or(0);
        assert!(watermark >= 10, "beta clock advanced past the delta");
    }

    /// Same mesh convergence as the loopback test, but the only pipe is
    /// the relay: no LAN addresses are ever offered (sandbox discovery
    /// stays empty), so every byte flows through signal envelopes.
    /// Proves the session treats the relay as a real transport: same
    /// engine paths, same trust gates, same clock persistence.
    #[tokio::test]
    async fn pair_syncs_bookmark_over_relay_without_lan() {
        use crate::relay_client::tests::harness::FakeRelay;
        let relay = FakeRelay::spawn().await;
        let alpha_id = DeviceIdentity::generate().unwrap();
        let beta_id = DeviceIdentity::generate().unwrap();
        let (ca, cb) = (memory_conn(), memory_conn());
        ca.lock()
            .unwrap()
            .execute(
                "INSERT INTO bookmarks(id,url,title,hlc_timestamp,is_deleted) VALUES('bm1','https://example.com/a','A',10,0)",
                [],
            )
            .unwrap();
        let Some((alpha, _)) = session_on(ca, &alpha_id).await else {
            return;
        };
        let Some((beta, _)) = session_on(cb, &beta_id).await else {
            return;
        };
        assert!(alpha.relay_status().is_none(), "relay off by default");
        {
            let t = alpha.trusted.lock().unwrap();
            trust_peer(&t, &beta_id);
        }
        {
            let t = beta.trusted.lock().unwrap();
            trust_peer(&t, &alpha_id);
        }
        alpha.set_relay(vec![relay.url.clone()], &alpha_id);
        beta.set_relay(vec![relay.url.clone()], &beta_id);
        assert!(
            alpha.relay_status().is_some_and(|s| !s.connected),
            "configured but not yet dialed"
        );
        // Both sides serve the relay: a node that never reads its
        // session receives nothing (same rule as QUIC serve_forever;
        // a deaf node syncs nothing, silently).
        let stop = Arc::new(AtomicBool::new(false));
        let serve = |s: Arc<SyncSession>| {
            let stop = Arc::clone(&stop);
            tokio::spawn(async move { s.serve_relay(&stop).await })
        };
        let (alpha_serve, beta_serve) = (serve(Arc::clone(&alpha)), serve(Arc::clone(&beta)));
        // Presence is fire-and-forget to live sessions only (the relay
        // 404s offline devices instead of buffering): both serve loops
        // must hold their sessions before the round, or the push is
        // correctly lost. Poll for presence, don't sleep.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && !(alpha.relay_status().is_some_and(|s| s.connected)
                && beta.relay_status().is_some_and(|s| s.connected))
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            alpha.relay_status().is_some_and(|s| s.connected),
            "alpha session live"
        );
        assert!(
            beta.relay_status().is_some_and(|s| s.connected),
            "beta session live"
        );
        // Alpha's round pushes presence addressed at beta; beta's rows flow
        // the other way. Mesh sync is symmetric: each side pulls.
        alpha.push_local_tab("https://example.com/live");
        let report = alpha.round().await;
        assert_eq!(report.peers_synced, 1, "relay peer synced");
        assert!(
            report.actions_received >= 1,
            "trust delta arrived via relay"
        );
        assert!(
            alpha.relay_status().is_some_and(|s| s.connected),
            "session live after relay round"
        );
        let tabs = {
            // Presence is fire-and-forget (no ack by design): the push
            // lands on beta's next serve quantum (~1s), not before this
            // round returns. Poll, don't assume.
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let tabs = beta.live_tabs();
                if !tabs.is_empty() || Instant::now() >= deadline {
                    break tabs;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        assert_eq!(tabs.len(), 1, "presence arrived via relay");
        match &tabs[0] {
            LiveTabAction::PushTab { url, .. } => assert_eq!(url, "https://example.com/live"),
        }
        let back = beta.round().await;
        assert_eq!(back.peers_synced, 1);
        assert!(
            back.actions_received >= 1,
            "bookmark delta arrived via relay"
        );
        stop.store(true, Ordering::Relaxed);
        let _ = beta_serve.await;
        let _ = alpha_serve.await;
        let watermark = beta
            .engine
            .lock()
            .unwrap()
            .clock_snapshot()
            .values()
            .copied()
            .max()
            .unwrap_or(0);
        assert!(watermark >= 10, "beta clock advanced past the relay delta");
        // Clearing works: empty URLs remove the transport (waiters fail
        // fast via drop) and rounds go LAN-only again.
        alpha.set_relay(vec![], &alpha_id);
        assert!(alpha.relay_status().is_none(), "relay cleared");
    }

    /// Offline peer stores, then drains on reconnect: A rounds while B
    /// has never connected (no serve task, no rounds: truly offline),
    /// so new deltas become TTL blobs (deferred, not lost); a second
    /// round stores nothing further (watermark advanced, no spam); B
    /// joining later applies the blob and converges.
    #[tokio::test]
    async fn relay_offline_store_then_drain() {
        use crate::relay_client::tests::harness::FakeRelay;
        let relay = FakeRelay::spawn().await;
        let alpha_id = DeviceIdentity::generate().unwrap();
        let beta_id = DeviceIdentity::generate().unwrap();
        let (ca, cb) = (memory_conn(), memory_conn());
        let cb_db = Arc::clone(&cb);
        ca.lock()
            .unwrap()
            .execute(
                "INSERT INTO bookmarks(id,url,title,hlc_timestamp,is_deleted) VALUES('bm1','https://example.com/a','A',10,0)",
                [],
            )
            .unwrap();
        let Some((alpha, _)) = session_on(ca, &alpha_id).await else {
            return;
        };
        let Some((beta, _)) = session_on(cb, &beta_id).await else {
            return;
        };
        {
            let t = alpha.trusted.lock().unwrap();
            trust_peer(&t, &beta_id);
        }
        {
            let t = beta.trusted.lock().unwrap();
            trust_peer(&t, &alpha_id);
        }
        alpha.set_relay(vec![relay.url.clone()], &alpha_id);
        beta.set_relay(vec![relay.url.clone()], &beta_id);
        // Only alpha serves: beta is offline server-side (never dialed).
        let stop = Arc::new(AtomicBool::new(false));
        let alpha_stop = Arc::clone(&stop);
        let alpha_session = Arc::clone(&alpha);
        let alpha_serve = tokio::spawn(async move { alpha_session.serve_relay(&alpha_stop).await });
        let report = alpha.round().await;
        assert!(report.deferred >= 1, "offline deltas stored, not lost");
        // Second round: watermark advanced, nothing new to store.
        let again = alpha.round().await;
        assert_eq!(again.deferred, 0, "no re-store without new deltas");
        // Beta joins late: drain applies the blob, clock converges.
        let beta_stop = Arc::clone(&stop);
        let beta_session = Arc::clone(&beta);
        let beta_serve = tokio::spawn(async move { beta_session.serve_relay(&beta_stop).await });
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !beta.relay_status().is_some_and(|s| s.connected) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let back = beta.round().await;
        assert_eq!(back.peers_synced, 1, "beta reaches alpha");
        stop.store(true, Ordering::Relaxed);
        let _ = beta_serve.await;
        let _ = alpha_serve.await;
        let watermark = beta
            .engine
            .lock()
            .unwrap()
            .clock_snapshot()
            .values()
            .copied()
            .max()
            .unwrap_or(0);
        assert!(
            watermark >= 10,
            "beta converged from the stored blob (or live; either way nothing was lost)"
        );
        let count: i64 = cb_db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bookmarks WHERE id='bm1' AND is_deleted=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "bookmark arrived exactly once");
    }

    #[tokio::test]
    async fn revoked_peer_is_skipped_before_dialing() {
        let alpha_id = DeviceIdentity::generate().unwrap();
        let beta_id = DeviceIdentity::generate().unwrap();
        let (ca, cb) = (memory_conn(), memory_conn());
        let Some((alpha, _)) = session_on(ca, &alpha_id).await else {
            return;
        };
        let Some((_beta, addr_b)) = session_on(cb, &beta_id).await else {
            return;
        };
        // Revoked (not merely absent): pre-filter skips without dialing, so
        // no handshake, no clock leak to a distrusted device.
        {
            let t = alpha.trusted.lock().unwrap();
            trust_peer(&t, &beta_id);
            t.revoke(&beta_id.device_id.to_string(), 2).unwrap();
        }
        let report = alpha
            .sync_known_peers(&[(vec![addr_b], beta_id.device_id.to_string())])
            .await;
        assert_eq!(report.peers_seen, 1);
        assert_eq!(report.peers_synced, 0);
        assert_eq!(report.actions_received, 0);
    }

    #[tokio::test]
    async fn falls_back_to_second_address_when_first_unroutable() {
        use std::net::{IpAddr, Ipv4Addr};
        let alpha_id = DeviceIdentity::generate().unwrap();
        let beta_id = DeviceIdentity::generate().unwrap();
        let (ca, cb) = (memory_conn(), memory_conn());
        // Bookmark lives on beta; alpha must pull it via the second address.
        cb.lock()
            .unwrap()
            .execute(
                "INSERT INTO bookmarks(id,url,title,hlc_timestamp,is_deleted) VALUES('bm-fallback','https://example.com/f','F',10,0)",
                [],
            )
            .unwrap();
        let Some((alpha, _)) = session_on(Arc::clone(&ca), &alpha_id).await else {
            return;
        };
        let Some((beta, addr_b)) = session_on(cb, &beta_id).await else {
            return;
        };
        {
            let t = alpha.trusted.lock().unwrap();
            trust_peer(&t, &beta_id);
        }
        {
            let t = beta.trusted.lock().unwrap();
            trust_peer(&t, &alpha_id);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let serve = |s: Arc<SyncSession>| {
            let stop = Arc::clone(&stop);
            tokio::spawn(async move { s.serve_forever(&stop).await })
        };
        let (alpha_serve, beta_serve) = (serve(Arc::clone(&alpha)), serve(Arc::clone(&beta)));
        // First address is TEST-NET-1 (unroutable, connect times out),
        // second is the live loopback listener. First reachable wins.
        let unroutable = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), addr_b.port());
        let report = alpha
            .sync_known_peers(&[(vec![unroutable, addr_b], beta_id.device_id.to_string())])
            .await;
        assert_eq!(report.peers_seen, 1);
        assert_eq!(report.peers_synced, 1, "fallback address must sync");
        assert!(
            report.actions_received >= 1,
            "bookmark delta arrived via fallback"
        );
        stop.store(true, Ordering::Relaxed);
        let _ = beta_serve.await;
        let _ = alpha_serve.await;
        let got: String = ca
            .lock()
            .unwrap()
            .query_row(
                "SELECT title FROM bookmarks WHERE id='bm-fallback'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(got, "F");
    }

    #[test]
    fn local_tab_gate_keeps_junk_out() {
        let conn = memory_conn();
        let id = DeviceIdentity::generate().unwrap();
        let engine = CrdtEngine::new(Arc::clone(&conn), id.device_id.to_string());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let discovery = DiscoveryService::new(crate::discovery::DiscoveryConfig {
                device_id: id.device_id.to_string(),
                device_name: "t".into(),
                fingerprint: id.fingerprint(),
                port: 0,
            });
            // mDNS may fail in sandboxes: skip instead of failing.
            let Ok(discovery) = discovery else { return };
            let session = SyncSession {
                engine: Mutex::new(engine),
                discovery,
                transport: tokio::sync::Mutex::new(P2pTransport::new(P2pConfig::new(
                    id.device_id.to_string(),
                    0,
                ))),
                trusted: Arc::new(Mutex::new(TrustedStore::new(conn))),
                settings: Arc::new(RwLock::new(SyncSettings::default())),
                device_id: id.device_id.to_string(),
                local_tab: Mutex::new(None),
                backoff: Mutex::new(HashMap::new()),
                last_round: Mutex::new(None),
                relay: Mutex::new(None),
                relay_stored: Mutex::new(HashMap::new()),
            };
            session.push_local_tab("javascript:alert(1)");
            session.push_local_tab("");
            session.push_local_tab(&("https://x.test/".to_owned() + &"y".repeat(3000)));
            assert!(session.local_tab.lock().unwrap().is_none());
            session.push_local_tab("https://example.com/ok");
            assert_eq!(
                session
                    .local_tab
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|(u, _)| u.as_str()),
                Some("https://example.com/ok")
            );
        });
    }
}
