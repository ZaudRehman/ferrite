//! Relay client: WebSocket signal transport to the Go relay
//! (`relay/main.go`). Zero new transport dependencies: the HTTP upgrade
//! and RFC 6455 framing are hand-rolled over tokio, and TLS reuses the
//! in-tree ring + webpki-roots stack (same explicit-provider pattern as
//! `ferrite-proxy::direct`, see the engine-bump checklist on crypto
//! provider ambiguity). Payloads stay opaque here too: this layer moves
//! `SignalMessage` envelopes, it never inspects sync content.
//!
//! The wire contract mirrors the server exactly: `ws(s)://host[:port]/path`
//! URLs only, first message must be signed auth
//! (`ferrite-relay-auth-v1|device_id|ts`, 60s skew), then the server
//! confirms `{"status":"authed"}` and routes `signal` envelopes by `To`.
//! Oversize reads fail the connection, exactly like the server's read
//! limit, so a hostile relay cannot grow our memory.

use std::collections::HashMap;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::oneshot;

use crate::crypto::DeviceIdentity;
use crate::relay_auth::{self, RelayAuthPayload};
use crate::sync_actions::CrdtMessage;

/// Mirror of the server's `maxMessageBytes` (256KiB + 4KiB headroom).
/// Anything larger fails the connection on both sides.
pub const RELAY_MAX_MESSAGE_BYTES: usize = 256 * 1024 + 4096;
/// Budget for TCP connect + TLS + WS handshake + auth confirm.
pub const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-IO-operation budget once connected.
pub const RELAY_IO_TIMEOUT: Duration = Duration::from_secs(10);
/// Client ping cadence. The server drops sessions unread for 60s, so a
/// persistent presence needs traffic well inside that window.
pub const RELAY_PING_INTERVAL: Duration = Duration::from_secs(25);
/// Reconnect backoff after a failed relay (mirrors the LAN peer backoff).
pub const RELAY_BACKOFF: Duration = Duration::from_secs(60);
/// WebSocket GUID from RFC 6455 §1.3 (handshake accept derivation).
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Awaiting-response budget per relay request (matches LAN PEER_TIMEOUT).
const RELAY_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RelayError {
    #[error("relay url rejected (want ws(s)://host[:port][/path])")]
    BadUrl,
    #[error("relay dns/connect failed")]
    Connect,
    #[error("relay tls failed")]
    Tls,
    #[error("relay websocket handshake rejected")]
    Handshake,
    #[error("relay auth refused: {0}")]
    Auth(String),
    #[error("relay message over cap")]
    Oversize,
    #[error("relay frame protocol error")]
    Frame,
    #[error("relay connection closed")]
    Closed,
    #[error("relay operation timed out")]
    Timeout,
    #[error("relay io error")]
    Io,
}

pub type RelayResult<T> = Result<T, RelayError>;

/// One signal envelope. Field names match `relay/main.go SignalMessage`
/// exactly (`type`, `from`, `to`, `payload`, `ts`, plus server-set
/// `blob`). `blob` is present only on drained slot deliveries (the slot
/// id the recipient acks); clients never set it on send (ignored there).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelaySignal {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub from: String,
    pub to: String,
    pub payload: serde_json::Value,
    pub ts: i64,
    #[serde(default)]
    pub blob: Option<String>,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Opaque CRDT transport inside a signal payload: multiplex-framed bytes
/// (same `encode_crdt`/`decode` as the QUIC path) base64'd under `data`.
/// The relay routes it by `To` without ever parsing it.
pub fn crdt_payload(msg: &CrdtMessage) -> serde_json::Value {
    let bytes = crate::multiplex::encode_crdt(msg);
    serde_json::json!({
        "kind": "crdt",
        "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

/// Inverse of [`crdt_payload`]: `None` on wrong shape, bad base64, or an
/// undecodable frame (a corrupt or hostile payload never reaches the
/// engine; same posture as the multiplex length-prefix check).
pub fn parse_crdt_payload(v: &serde_json::Value) -> Option<CrdtMessage> {
    let data = v.get("data")?.as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let (decoded, _) = crate::multiplex::decode(&bytes)?;
    match decoded {
        crate::multiplex::Decoded::Crdt(msg) => Some(msg),
    }
}

#[derive(Debug)]
enum WsStream {
    Plain(tokio::net::TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>),
}
// Manual fan-out: one read/write implementation for both transports so
// connection code never branches on TLS after the handshake.
impl AsyncRead for WsStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            WsStream::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            WsStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for WsStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        match &mut *self {
            WsStream::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            WsStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        match &mut *self {
            WsStream::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            WsStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        match &mut *self {
            WsStream::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            WsStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// One authed relay session: handshake + auth done, frames flowing.
/// Not Clone: exactly one owner drives the socket (the link), so no two
/// tasks can interleave writes.
#[derive(Debug)]
pub struct RelayConnection {
    stream: WsStream,
    device_id: String,
    frag: Vec<u8>,
}

struct ParsedUrl {
    tls: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_relay_url(url: &str) -> Option<ParsedUrl> {
    let url = url.trim();
    if url.chars().any(|c| c.is_control()) {
        return None;
    }
    let (scheme, rest) = url.split_once("://")?;
    let (tls, default_port) = match scheme.to_ascii_lowercase().as_str() {
        "ws" => (false, 80u16),
        "wss" => (true, 443u16),
        _ => return None,
    };
    if rest.is_empty() || rest.contains('@') || rest.contains(' ') {
        return None;
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_owned()),
        None => (rest, "/sync".to_owned()),
    };
    if authority.is_empty() || path.is_empty() || !path.starts_with('/') {
        return None;
    }
    // Authority is `host`, `host:port`, or `[v6]:port`. Anything else
    // (userinfo, spaces, unbracketed multi-colon, empty) is rejected:
    // a relay URL must not smuggle surprises into dial or SNI.
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if h.starts_with('[') => {
            let h = h.strip_prefix('[')?.strip_suffix(']')?;
            if h.is_empty() {
                return None;
            }
            (h.to_owned(), p.parse::<u16>().ok()?)
        }
        Some((h, p)) if !h.is_empty() && !h.contains(':') => {
            let port = p.parse::<u16>().ok()?;
            if port == 0 {
                return None;
            }
            (h.to_ascii_lowercase(), port)
        }
        Some(_) => return None,
        None => (authority.to_ascii_lowercase(), default_port),
    };
    if host.is_empty() || host.len() > 253 {
        return None;
    }
    Some(ParsedUrl {
        tls,
        host,
        port,
        path,
    })
}

fn ws_accept(key: &str) -> String {
    use sha1::Digest;
    let mut h = sha1::Sha1::new();
    h.update(key.as_bytes());
    h.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}

async fn read_head(stream: &mut WsStream, cap: usize) -> RelayResult<Vec<u8>> {
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];
    loop {
        if buf.len() > cap {
            return Err(RelayError::Oversize);
        }
        let n = tokio::time::timeout(RELAY_IO_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| RelayError::Timeout)?
            .map_err(|_| RelayError::Io)?;
        if n == 0 {
            return Err(RelayError::Closed);
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(buf);
        }
    }
}

fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    let target = name.to_ascii_lowercase();
    head.lines().skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(&target) {
            Some(v.trim())
        } else {
            None
        }
    })
}

impl RelayConnection {
    /// Dial, TLS (for `wss`), WebSocket upgrade, then the signed auth
    /// claim. Returns only on `{"status":"authed"}`: anything else (a
    /// rejection object, a timeout, a bad accept key) fails closed and
    /// the socket is dropped, never used half-authed.
    pub async fn connect(
        url: &str,
        device_id: &str,
        identity: &DeviceIdentity,
    ) -> RelayResult<Self> {
        if device_id.is_empty() || device_id.len() > 128 {
            return Err(RelayError::BadUrl);
        }
        let parsed = parse_relay_url(url).ok_or(RelayError::BadUrl)?;
        let addrs: Vec<std::net::SocketAddr> = tokio::time::timeout(
            RELAY_CONNECT_TIMEOUT,
            tokio::net::lookup_host((parsed.host.as_str(), parsed.port)),
        )
        .await
        .map_err(|_| RelayError::Timeout)?
        .map_err(|_| RelayError::Connect)?
        .collect();
        let addr = addrs.into_iter().next().ok_or(RelayError::Connect)?;
        let tcp = tokio::time::timeout(RELAY_CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr))
            .await
            .map_err(|_| RelayError::Timeout)?
            .map_err(|_| RelayError::Connect)?;
        let _ = tcp.set_nodelay(true);
        let mut stream = if parsed.tls {
            // Explicit ring provider (see direct.rs): feature unification
            // can enable both ring and aws-lc, and auto-detect panics.
            let _ = rustls::crypto::ring::default_provider().install_default();
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = std::sync::Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            );
            let connector = tokio_rustls::TlsConnector::from(config);
            let name: rustls_pki_types::ServerName<'static> =
                match parsed.host.parse::<std::net::IpAddr>() {
                    Ok(std::net::IpAddr::V4(v4)) => rustls_pki_types::ServerName::IpAddress(
                        rustls_pki_types::IpAddr::V4(v4.into()),
                    ),
                    Ok(std::net::IpAddr::V6(v6)) => rustls_pki_types::ServerName::IpAddress(
                        rustls_pki_types::IpAddr::V6(v6.into()),
                    ),
                    Err(_) => parsed
                        .host
                        .clone()
                        .try_into()
                        .map_err(|_| RelayError::Tls)?,
                };
            let tls = tokio::time::timeout(RELAY_CONNECT_TIMEOUT, connector.connect(name, tcp))
                .await
                .map_err(|_| RelayError::Timeout)?
                .map_err(|_| RelayError::Tls)?;
            WsStream::Tls(Box::new(tls))
        } else {
            WsStream::Plain(tcp)
        };
        // Upgrade handshake: fresh random key, strict accept validation.
        let mut key_bytes = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut key_bytes);
        let key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
            parsed.path, parsed.host, key
        );
        tokio::time::timeout(RELAY_IO_TIMEOUT, stream.write_all(req.as_bytes()))
            .await
            .map_err(|_| RelayError::Timeout)?
            .map_err(|_| RelayError::Io)?;
        let raw = read_head(&mut stream, 16 * 1024).await?;
        let head = String::from_utf8_lossy(&raw);
        let status = head.lines().next().unwrap_or("");
        if !status.contains(" 101") {
            return Err(RelayError::Handshake);
        }
        let accept = header_value(&head, "sec-websocket-accept").ok_or(RelayError::Handshake)?;
        if accept != ws_accept(&key) {
            return Err(RelayError::Handshake);
        }
        // First message must be our signed auth claim; the server answers
        // explicit accept or explicit rejection (never silence).
        let mut conn = Self {
            stream,
            device_id: device_id.to_owned(),
            frag: Vec::new(),
        };
        let auth: RelayAuthPayload = relay_auth::build_auth(identity, device_id, now_secs());
        let auth_payload = serde_json::to_value(&auth).map_err(|_| RelayError::Io)?;
        let envelope = serde_json::json!({
            "type": "auth",
            "from": device_id,
            "to": "",
            "payload": auth_payload,
            "ts": now_secs(),
        });
        conn.send_json(&envelope).await?;
        let reply = tokio::time::timeout(RELAY_CONNECT_TIMEOUT, conn.recv_json())
            .await
            .map_err(|_| RelayError::Timeout)??;
        if reply.get("status").and_then(|s| s.as_str()) == Some("authed") {
            Ok(conn)
        } else {
            let err = reply
                .get("error")
                .and_then(|s| s.as_str())
                .unwrap_or("auth rejected")
                .chars()
                .take(128)
                .collect::<String>();
            Err(RelayError::Auth(err))
        }
    }

    /// Send one signal envelope (`signal`/`erase_me`/…): caller sets the
    /// type, this stamps `from`/`ts` from the session. Payloads stay
    /// opaque here by construction (serde_json::Value, never parsed).
    pub async fn send_signal(
        &mut self,
        msg_type: &str,
        to: &str,
        payload: serde_json::Value,
    ) -> RelayResult<()> {
        if msg_type.is_empty() || msg_type.len() > 32 || to.len() > 128 {
            return Err(RelayError::Frame);
        }
        let envelope = serde_json::json!({
            "type": msg_type,
            "from": self.device_id,
            "to": to,
            "payload": payload,
            "ts": now_secs(),
        });
        self.send_json(&envelope).await
    }

    /// GDPR self-erasure for a memory-only relay: drops our session, the
    /// id→key pin, and cached TURN credentials server-side. Fire-and-
    /// forget by design (the server replies `forgotten`, then hangs up).
    pub async fn send_erase_me(&mut self) -> RelayResult<()> {
        self.send_signal("erase_me", "", serde_json::Value::Null)
            .await
    }

    pub async fn send_json(&mut self, v: &serde_json::Value) -> RelayResult<()> {
        let bytes = serde_json::to_vec(v).map_err(|_| RelayError::Frame)?;
        self.send_frame(0x1, &bytes).await
    }

    /// Next text signal from the server. Control frames are answered
    /// inline (ping→pong); fragmented messages reassemble bounded by the
    /// same cap as the server, so a hostile relay cannot grow memory.
    pub async fn recv_signal(&mut self) -> RelayResult<RelaySignal> {
        let bytes = self.recv_text(RELAY_IO_TIMEOUT).await?;
        serde_json::from_slice::<RelaySignal>(&bytes).map_err(|_| RelayError::Frame)
    }

    pub async fn recv_json(&mut self) -> RelayResult<serde_json::Value> {
        self.recv_json_quantum(RELAY_IO_TIMEOUT).await
    }

    async fn recv_json_quantum(&mut self, quantum: Duration) -> RelayResult<serde_json::Value> {
        let bytes = self.recv_text(quantum).await?;
        serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|_| RelayError::Frame)
    }

    pub async fn ping(&mut self) -> RelayResult<()> {
        self.send_frame(0x9, &[]).await
    }

    /// Graceful close: best-effort close frame, then drop the socket.
    pub async fn close(mut self) {
        let _ = self.send_frame(0x8, &[]).await;
    }

    async fn send_frame(&mut self, opcode: u8, data: &[u8]) -> RelayResult<()> {
        if data.len() > RELAY_MAX_MESSAGE_BYTES {
            return Err(RelayError::Oversize);
        }
        let mut head = vec![0x80 | (opcode & 0x0F), 0x80];
        if data.len() < 126 {
            head[1] |= data.len() as u8;
        } else if data.len() <= 0xFFFF {
            head[1] |= 126;
            head.extend_from_slice(&(data.len() as u16).to_be_bytes());
        } else {
            head[1] |= 127;
            head.extend_from_slice(&(data.len() as u64).to_be_bytes());
        }
        let mut mask = [0u8; 4];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut mask);
        head.extend_from_slice(&mask);
        tokio::time::timeout(RELAY_IO_TIMEOUT, self.stream.write_all(&head))
            .await
            .map_err(|_| RelayError::Timeout)?
            .map_err(|_| RelayError::Io)?;
        // Mask in bounded chunks: full-message buffering of a
        // near-cap payload is fine (cap is the bound), streaming the
        // write keeps a slow relay from stalling one giant write.
        for chunk in data.chunks(8192) {
            let masked: Vec<u8> = chunk
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ mask[i % 4])
                .collect();
            tokio::time::timeout(RELAY_IO_TIMEOUT, self.stream.write_all(&masked))
                .await
                .map_err(|_| RelayError::Timeout)?
                .map_err(|_| RelayError::Io)?;
        }
        Ok(())
    }

    async fn read_exact_capped(&mut self, len: usize, quantum: Duration) -> RelayResult<Vec<u8>> {
        if len > RELAY_MAX_MESSAGE_BYTES {
            return Err(RelayError::Oversize);
        }
        let mut buf = vec![0u8; len];
        tokio::time::timeout(quantum, self.stream.read_exact(&mut buf))
            .await
            .map_err(|_| RelayError::Timeout)?
            .map_err(|_| RelayError::Io)?;
        Ok(buf)
    }

    async fn recv_text(&mut self, quantum: Duration) -> RelayResult<Vec<u8>> {
        loop {
            let mut hdr = [0u8; 2];
            tokio::time::timeout(quantum, self.stream.read_exact(&mut hdr))
                .await
                .map_err(|_| RelayError::Timeout)?
                .map_err(|_| RelayError::Closed)?;
            // No extensions negotiated: RSV bits set is a protocol error.
            if hdr[0] & 0x70 != 0 {
                return Err(RelayError::Frame);
            }
            let fin = hdr[0] & 0x80 != 0;
            let opcode = hdr[0] & 0x0F;
            let mut len = (hdr[1] & 0x7F) as usize;
            if len == 126 {
                let ext = self.read_exact_capped(2, quantum).await?;
                len = u16::from_be_bytes([ext[0], ext[1]]) as usize;
            } else if len == 127 {
                let ext = self.read_exact_capped(8, quantum).await?;
                let big =
                    u64::from_be_bytes(ext.as_slice().try_into().map_err(|_| RelayError::Frame)?);
                len = usize::try_from(big).map_err(|_| RelayError::Oversize)?;
            }
            // Server-to-client frames arrive unmasked; tolerate masked
            // ones (unmask) rather than failing interop over it.
            let masked = hdr[1] & 0x80 != 0;
            let mask = if masked {
                let m = self.read_exact_capped(4, quantum).await?;
                [m[0], m[1], m[2], m[3]]
            } else {
                [0u8; 4]
            };
            if self.frag.len().saturating_add(len) > RELAY_MAX_MESSAGE_BYTES {
                return Err(RelayError::Oversize);
            }
            let mut payload = self.read_exact_capped(len, quantum).await?;
            if masked {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            match opcode {
                0x0 => {
                    // Continuation of a fragmented message: append and
                    // return only on FIN. A lone continuation still
                    // decodes downstream (empty or partial JSON fails
                    // closed at the serde layer, never panics).
                    self.frag.extend_from_slice(&payload);
                    if fin {
                        return Ok(std::mem::take(&mut self.frag));
                    }
                }
                0x1 | 0x2 => {
                    if !fin {
                        self.frag.extend_from_slice(&payload);
                        continue;
                    }
                    if !self.frag.is_empty() {
                        self.frag.extend_from_slice(&payload);
                        return Ok(std::mem::take(&mut self.frag));
                    }
                    return Ok(payload);
                }
                0x9 => {
                    // Ping: bounded pong, then keep reading. Control
                    // payloads over 125 bytes are a protocol error.
                    if payload.len() > 125 {
                        return Err(RelayError::Frame);
                    }
                    self.send_frame(0xA, &payload).await?;
                }
                0xA => { /* pong: keepalive answered, keep reading */ }
                0x8 => return Err(RelayError::Closed),
                _ => return Err(RelayError::Frame),
            }
        }
    }
}

/// Live relay session: one persistent authed connection, reconnected
/// with backoff, shared by request/response rounds and the inbound
/// serve task. The socket lives behind an async mutex (never a std
/// lock across an await; that would pin the future !Send); short
/// bookkeeping locks stay std. Response matching is by sender id:
/// exactly one request per peer is ever in flight (rounds are sequential
/// per peer), so a single oneshot slot per peer is sufficient and cannot
/// leak; slots are removed on reply, timeout, or revoke.
pub struct RelayLink {
    urls: Vec<String>,
    device_id: String,
    identity: DeviceIdentity,
    conn: tokio::sync::Mutex<Option<RelayConnection>>,
    /// Serializes dialing: check+dial+store is one critical section, so
    /// two tasks racing first-connect (serve loop vs first round) cannot
    /// create two server sessions; the reply would route to the socket
    /// nobody reads while the stored one stays silent.
    dial: tokio::sync::Mutex<()>,
    /// Sync read of session presence for diagnostics (the socket lock
    /// is async-only). Set on auth, cleared on drop/backoff.
    live: AtomicBool,
    pending: Mutex<HashMap<String, oneshot::Sender<RelayReply>>>,
    backoff_until: Mutex<Instant>,
}

/// Awaited answer for one relay request: the peer's reply, or the
/// reason it never comes (offline, revoked mid-flight, shutdown).
pub type RelayReply = Result<CrdtMessage, String>;

/// What one serve-loop step did. The caller loops until `stop` flips;
/// `Idle` (timeout, no traffic) is the stop-check point, `Gone` means
/// the session died and the next step reconnects with backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeOutcome {
    Handled,
    Idle,
    Gone,
}

impl RelayLink {
    pub fn new(urls: Vec<String>, device_id: String, identity: DeviceIdentity) -> Self {
        Self {
            urls,
            device_id,
            identity,
            conn: tokio::sync::Mutex::new(None),
            dial: tokio::sync::Mutex::new(()),
            live: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
            backoff_until: Mutex::new(Instant::now()),
        }
    }

    /// Snapshot for settings/diagnostics (no secrets: URL + presence only).
    pub fn status(&self) -> RelayStatus {
        RelayStatus {
            urls: self.urls.clone(),
            connected: self.live.load(Ordering::Relaxed),
        }
    }

    fn backing_off(&self) -> bool {
        self.backoff_until
            .lock()
            .map(|t| Instant::now() < *t)
            .unwrap_or(true)
    }

    fn note_backoff(&self) {
        if let Ok(mut t) = self.backoff_until.lock() {
            *t = Instant::now() + RELAY_BACKOFF;
        }
        self.live.store(false, Ordering::Relaxed);
    }

    /// Connect (trying URLs in order) and auth if no live session exists.
    /// Failure backs off for a minute so a dead relay costs one dial per
    /// round, not a hot spin. Never throws across the await boundary
    /// with the lock held: the connection is built outside, stored after.
    pub async fn ensure_connected(&self) -> Result<(), String> {
        // Whole check+dial+store under one lock: concurrent first-use
        // (serve loop vs round) must yield exactly one server session.
        let _dial = self.dial.lock().await;
        if self.conn.lock().await.is_some() {
            return Ok(());
        }
        if self.backing_off() {
            return Err("relay backoff".to_owned());
        }
        if self.urls.is_empty() {
            return Err("no relay urls".to_owned());
        }
        // Failover probe first (TCP reachability, no auth): mirrors the
        // backup failover semantics: first reachable wins.
        let probe_urls: Vec<String> = self.urls.clone();
        let picked = crate::backup::connect_with_failover(&probe_urls).await.ok();
        let order: Vec<String> = match picked {
            Some(first) => {
                let mut v = vec![first.clone()];
                v.extend(probe_urls.into_iter().filter(|u| *u != first));
                v
            }
            None => probe_urls,
        };
        let mut last_err = "no relay reachable".to_owned();
        for url in order {
            let fut = RelayConnection::connect(&url, &self.device_id, &self.identity);
            match tokio::time::timeout(RELAY_CONNECT_TIMEOUT, fut).await {
                Ok(Ok(conn)) => {
                    *self.conn.lock().await = Some(conn);
                    self.live.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                Ok(Err(e)) => last_err = format!("{e}"),
                Err(_) => last_err = "relay connect timed out".to_owned(),
            }
        }
        self.note_backoff();
        Err(last_err)
    }

    /// Send one signal to a connected peer. Drops a dead connection so
    /// the next call reconnects; the caller decides retry policy.
    pub async fn send_to(&self, to: &str, payload: serde_json::Value) -> Result<(), String> {
        self.ensure_connected().await?;
        let mut slot = self.conn.lock().await;
        let conn = slot
            .as_mut()
            .ok_or_else(|| "relay disconnected".to_owned())?;
        match conn.send_signal("signal", to, payload).await {
            Ok(()) => Ok(()),
            Err(e) => {
                *slot = None;
                drop(slot);
                self.note_backoff();
                self.live.store(false, Ordering::Relaxed);
                Err(format!("relay send: {e}"))
            }
        }
    }

    /// Request/response round trip with one peer: sends a CRDT message
    /// and awaits its reply (matched by sender id). Exactly one waiter
    /// per peer: a second concurrent request to the same peer fails fast
    /// instead of piling up; rounds are sequential per peer by design.
    /// Replies arrive through the serve loop (`serve_once`), which owns
    /// the only reader: a second reader would steal frames, so this
    /// function only sends and waits.
    pub async fn request(&self, peer_id: &str, msg: &CrdtMessage) -> RelayReply {
        if peer_id.is_empty() || peer_id == self.device_id {
            return Err("relay request refused".to_owned());
        }
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().map_err(|_| "relay lock".to_owned())?;
            if pending.contains_key(peer_id) {
                return Err("relay request already in flight".to_owned());
            }
            pending.insert(peer_id.to_owned(), tx);
        }
        let payload = crdt_payload(msg);
        if let Err(e) = self.send_to(peer_id, payload).await {
            self.pending.lock().ok().and_then(|mut p| p.remove(peer_id));
            return Err(e);
        }
        match tokio::time::timeout(RELAY_REQUEST_TIMEOUT, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err("relay reply channel closed".to_owned()),
            Err(_) => {
                self.pending.lock().ok().and_then(|mut p| p.remove(peer_id));
                Err("relay reply timed out".to_owned())
            }
        }
    }

    /// Deliver an inbound reply to its waiter. Returns false when nobody
    /// was waiting (stale/duplicate/unsolicited reply; dropped, never
    /// applied blindly: only `request()` consumptions touch the engine).
    pub fn deliver(&self, peer_id: &str, msg: CrdtMessage) -> bool {
        self.deliver_reply(peer_id, Ok(msg))
    }

    /// Deliver a failure to a waiter (peer offline, revoked mid-flight).
    /// Same single-waiter discipline as `deliver`.
    pub fn deliver_err(&self, peer_id: &str, err: String) -> bool {
        self.deliver_reply(peer_id, Err(err))
    }

    fn deliver_reply(&self, peer_id: &str, reply: RelayReply) -> bool {
        self.pending
            .lock()
            .ok()
            .and_then(|mut p| p.remove(peer_id))
            .map(|tx| tx.send(reply).is_ok())
            .unwrap_or(false)
    }

    /// One serve-loop step: read the next inbound message and dispatch it.
    /// Exactly one task per link may call this (the serve task owns the
    /// only reader; a second reader would steal frames mid-round).
    /// Dispatch, by sender trust (untrusted senders are dropped outright;
    /// the relay routes opaquely, so anything from strangers is untrusted
    /// input by definition):
    /// - `SyncRequest`/`FetchArchiveBody` → `on_request` computes the
    ///   reply (engine-owned); a returned message is sent back.
    /// - `SyncResponse` → completes the matching `request()` waiter via
    ///   `deliver` (the waiter applies it; never applied here, so no
    ///   double-apply). Unsolicited responses are dropped.
    /// - `PushLiveTab` → `on_push` applies it (engine-owned presence).
    /// - `{"error"}` objects with a `to` matching a waiter fail it fast
    ///   (offline peer) instead of hanging to timeout.
    /// - Anything else (unknown shapes, untrusted senders, undecodable
    ///   payloads) is dropped. The relay routes opaquely, so anything it
    ///   forwards from strangers is untrusted input by definition.
    ///
    /// Lock discipline: the socket lock is held across the socket wait,
    /// so the serve loop passes a SHORT quantum; a 10s hold would starve
    /// concurrent senders (the serve task would own the socket while a
    /// round waits to write). The 1s quantum keeps worst-case send
    /// latency near a second; idle spinning costs one wakeup per second.
    pub async fn serve_once<R, P>(
        &self,
        is_trusted: impl Fn(&str) -> bool,
        mut on_request: R,
        mut on_push: P,
    ) -> ServeOutcome
    where
        R: FnMut(&str, CrdtMessage) -> Option<CrdtMessage>,
        P: FnMut(&str, CrdtMessage),
    {
        // One quantum, one timeout layer: the inner read already
        // enforces it, so cancelling here can never strand a half-read
        // frame (a cancelled partial read would desync the stream).
        let msg = match self.recv_quantum(Duration::from_secs(1)).await {
            Ok(v) => v,
            Err(RelayError::Closed) | Err(RelayError::Timeout) => return ServeOutcome::Idle,
            Err(_) => return ServeOutcome::Gone,
        };
        // Shape-match, not schema-parse: signals, confirms, and refusals
        // share this socket, and a refusal is routing information (fail
        // the waiter), never a frame error.
        let is_signal = msg.get("type").and_then(|t| t.as_str()) == Some("signal");
        let from = msg.get("from").and_then(|f| f.as_str()).unwrap_or("");
        let payload = msg
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        // Drained slot id, acked only after the message is actually
        // handled below (applied or answered). Untrusted and dropped
        // mail is never acked; it redelivers until TTL, bounded rot.
        let blob = msg.get("blob").and_then(|b| b.as_str()).map(str::to_owned);
        if is_signal && let Some(crdt) = parse_crdt_payload(&payload) {
            if !is_trusted(from) {
                return ServeOutcome::Handled;
            }
            match crdt {
                CrdtMessage::SyncRequest { .. } | CrdtMessage::FetchArchiveBody { .. } => {
                    if let Some(reply) = on_request(from, crdt) {
                        let _ = self.send_to(from, crdt_payload(&reply)).await;
                        if let Some(id) = &blob {
                            self.ack(id).await;
                        }
                    }
                }
                CrdtMessage::SyncResponse { .. } | CrdtMessage::FetchArchiveBodyResponse { .. } => {
                    if blob.is_some() {
                        // Stored mail, not a reply: nobody requested it, the
                        // slot IS the delivery. Applies locally (idempotent
                        // LWW; a later live copy re-applies harmlessly).
                        on_push(from, crdt);
                        if let Some(id) = &blob {
                            self.ack(id).await;
                        }
                    } else {
                        // Live reply: completes the waiter, which applies it
                        // (never applied here: no double-apply).
                        self.deliver(from, crdt);
                    }
                }
                CrdtMessage::PushLiveTab { .. } => {
                    on_push(from, crdt);
                    if let Some(id) = &blob {
                        self.ack(id).await;
                    }
                }
            }
            ServeOutcome::Handled
        } else if let Some(err) = msg.get("error").and_then(|e| e.as_str()) {
            // Server-side refusals (offline peer, rate limit): fail the
            // matching waiter fast instead of hanging it to timeout.
            // Errors without a recipient name no waiter.
            if let Some(to) = msg.get("to").and_then(|t| t.as_str()) {
                self.deliver_err(to, format!("relay: {err}"));
            }
            ServeOutcome::Handled
        } else {
            // Status confirms, stray shapes: nothing to do.
            ServeOutcome::Handled
        }
    }

    /// Receive one inbound message as untyped JSON (reconnecting first
    /// if needed). The relay multiplexes shapes on one socket: signals,
    /// `{"status"}` confirms, `{"error"}` refusals; so the serve loop
    /// matches on shape here instead of forcing the signal schema (a
    /// typed `recv_signal` would turn every refusal into a frame error
    /// and drop the session it was meant to inform).
    /// A quiet window is `Timeout`, NOT failure: the session, backoff,
    /// and presence all survive it (the server only drops dead peers,
    /// and our pings prove liveness). Only IO/close/frame errors drop
    /// the session so the next call reconnects; callers treat `Closed`
    /// as a clean reconnect trigger, not an error to log.
    /// Lock discipline: the socket lock is held across the socket wait,
    /// so callers must pass a SHORT quantum; a 10s hold would starve
    /// concurrent senders (the serve task would own the socket while a
    /// round waits to write). The serve quantum below keeps holds to
    /// ~1s, so sends interleave freely.
    pub async fn recv_quantum(&self, quantum: Duration) -> Result<serde_json::Value, RelayError> {
        self.ensure_connected()
            .await
            .map_err(|_| RelayError::Closed)?;
        let mut slot = self.conn.lock().await;
        let conn = slot.as_mut().ok_or(RelayError::Closed)?;
        // Single timeout layer, threaded all the way down: wrapping this
        // in a second timeout could cancel a partial frame read and
        // desync the stream (consumed bytes would be lost).
        match conn.recv_json_quantum(quantum).await {
            Ok(v) => Ok(v),
            Err(RelayError::Timeout) => Err(RelayError::Timeout),
            Err(e) => {
                *slot = None;
                drop(slot);
                self.note_backoff();
                self.live.store(false, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    /// Convenience: single receive with the IO timeout as the quantum
    /// (handshakes, tests, and other non-loop readers).
    pub async fn recv(&self) -> Result<serde_json::Value, RelayError> {
        self.recv_quantum(RELAY_IO_TIMEOUT).await
    }

    /// Keepalive ping on the live session (no-op when disconnected:
    /// callers ping on a schedule and ignore failures; the next serve
    /// step reconnects). Server read deadlines make this load-bearing
    /// for presence, not a nicety.
    pub async fn ping(&self) -> Result<(), String> {
        self.ensure_connected().await?;
        let mut slot = self.conn.lock().await;
        let conn = slot
            .as_mut()
            .ok_or_else(|| "relay disconnected".to_owned())?;
        match conn.ping().await {
            Ok(()) => Ok(()),
            Err(e) => {
                *slot = None;
                drop(slot);
                self.note_backoff();
                self.live.store(false, Ordering::Relaxed);
                Err(format!("relay ping: {e}"))
            }
        }
    }

    /// Ack one drained slot after handling it (fire-and-forget: a lost
    /// ack redelivers, and CRDT re-apply is idempotent, so at-least-once
    /// delivery is exactly-once in effect). Never ack unhandled mail.
    pub async fn ack(&self, blob_id: &str) {
        if blob_id.is_empty() || blob_id.len() > 128 {
            return;
        }
        let _ = self
            .send_raw("ack", "", serde_json::json!({"blob": blob_id}))
            .await;
    }
    /// Raw control envelope on the live session (`ack` and future
    /// server-directed types that are never routed).
    pub async fn send_raw(
        &self,
        msg_type: &str,
        to: &str,
        payload: serde_json::Value,
    ) -> Result<(), String> {
        self.ensure_connected().await?;
        let mut slot = self.conn.lock().await;
        let conn = slot
            .as_mut()
            .ok_or_else(|| "relay disconnected".to_owned())?;
        match conn.send_signal(msg_type, to, payload).await {
            Ok(()) => Ok(()),
            Err(e) => {
                *slot = None;
                drop(slot);
                self.note_backoff();
                self.live.store(false, Ordering::Relaxed);
                Err(format!("relay send: {e}"))
            }
        }
    }

    /// Store one opaque payload for an offline peer (TTL blob): connect
    /// throwaway, auth, store, read the single confirm, close. Separate
    /// from the live session on purpose; a store must never disturb
    /// presence (same-id reconnects evict), and the confirm arrives on
    /// the storing socket, not the serve loop. Returns the slot id.
    pub async fn store(&self, to: &str, payload: serde_json::Value) -> Result<String, String> {
        if to.is_empty() || to.len() > 128 {
            return Err("relay store refused".to_owned());
        }
        if self.urls.is_empty() {
            return Err("no relay urls".to_owned());
        }
        let mut last_err = "no relay reachable".to_owned();
        for url in &self.urls {
            let fut = RelayConnection::connect(url, &self.device_id, &self.identity);
            let mut conn = match tokio::time::timeout(RELAY_CONNECT_TIMEOUT, fut).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    last_err = format!("{e}");
                    continue;
                }
                Err(_) => {
                    last_err = "relay connect timed out".to_owned();
                    continue;
                }
            };
            let envelope = serde_json::json!({
                "type": "store",
                "from": self.device_id,
                "to": to,
                "payload": payload,
                "ts": now_secs(),
            });
            if conn.send_json(&envelope).await.is_err() {
                last_err = "relay store write failed".to_owned();
                continue;
            }
            let reply = match tokio::time::timeout(RELAY_IO_TIMEOUT, conn.recv_json()).await {
                Ok(Ok(v)) => v,
                _ => {
                    last_err = "relay store confirm missing".to_owned();
                    continue;
                }
            };
            if reply.get("status").and_then(|s| s.as_str()) == Some("stored")
                && let Some(id) = reply.get("blob").and_then(|b| b.as_str())
            {
                return Ok(id.to_owned());
            }
            last_err = reply
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("relay store refused")
                .chars()
                .take(128)
                .collect();
        }
        Err(last_err)
    }

    /// Drop pending waiters (revoke/shutdown): waiters fail fast instead
    /// of hanging to timeout.
    pub fn drop_pending(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }

    /// Forget this device server-side (`erase_me`) over a fresh throwaway
    /// connection: the live session (if any) is left alone so a failed
    /// erase never kills a working session as a side effect.
    pub async fn erase_me(
        urls: &[String],
        device_id: &str,
        identity: &DeviceIdentity,
    ) -> Result<(), String> {
        let mut last_err = "no relay urls".to_owned();
        for url in urls {
            match tokio::time::timeout(
                RELAY_CONNECT_TIMEOUT,
                RelayConnection::connect(url, device_id, identity),
            )
            .await
            {
                Ok(Ok(mut conn)) => {
                    let _ = conn.send_erase_me().await;
                    return Ok(());
                }
                Ok(Err(e)) => last_err = format!("{e}"),
                Err(_) => last_err = "relay connect timed out".to_owned(),
            }
        }
        Err(last_err)
    }
}

#[derive(Debug, Clone)]
pub struct RelayStatus {
    pub urls: Vec<String>,
    pub connected: bool,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn url_shapes_accepted_and_rejected() {
        let good = parse_relay_url("ws://127.0.0.1:8080/sync").expect("ws url");
        assert!(!good.tls);
        assert_eq!(good.port, 8080);
        assert_eq!(good.path, "/sync");
        let bare = parse_relay_url("ws://example.com").expect("bare ws url");
        assert_eq!((bare.port, bare.path.as_str()), (80, "/sync"));
        let secure = parse_relay_url("wss://relay.example.com:443/x").expect("wss url");
        assert!(secure.tls);
        let v6 = parse_relay_url("ws://[::1]:9000/sync").expect("bracketed v6");
        assert_eq!((v6.host.as_str(), v6.port), ("::1", 9000));
        for bad in [
            "",
            "http://example.com/sync",
            "wss://",
            "ws://user@example.com/",
            "ws://exam ple.com/",
            "ws://example.com:0/",
            "ws://example.com:notaport/",
            "ws://a:b:c/",
            "ws://[::1/",
            &("ws://".to_owned() + &"a".repeat(300)),
            "ws://example.com/\x01",
        ] {
            assert!(parse_relay_url(bad).is_none(), "rejected: {bad:?}");
        }
    }

    #[test]
    fn ws_accept_matches_rfc_vector() {
        // RFC 6455 §1.3 example.
        assert_eq!(
            ws_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn crdt_payload_roundtrips_opaquely() {
        let msg = CrdtMessage::SyncRequest {
            since_vector_clock: [("dev".to_owned(), 42u64)].into_iter().collect(),
            enabled_categories: Default::default(),
        };
        let payload = crdt_payload(&msg);
        assert_eq!(payload.get("kind").and_then(|k| k.as_str()), Some("crdt"));
        let back = parse_crdt_payload(&payload).expect("round trip");
        // No PartialEq on CrdtMessage: re-encoding must be byte-identical.
        assert_eq!(crdt_payload(&back), payload);
        assert!(parse_crdt_payload(&serde_json::json!({"kind": "crdt"})).is_none());
        assert!(parse_crdt_payload(&serde_json::json!({"kind": "crdt", "data": "!!!"})).is_none());
        assert!(parse_crdt_payload(&serde_json::json!({"kind": "nope", "data": "eA=="})).is_none());
    }

    /// Minimal in-test relay implementing the Go server's auth+routing
    /// contract (auth-first, id pinning, From enforcement, To routing,
    /// 404s, erase_me, same-id eviction). Lets the client suite speak the
    /// real protocol without a Go toolchain in CI. Also reused by the
    /// session-level relay tests (same protocol, engine on both ends).
    #[cfg(test)]
    pub(crate) mod harness {
        use super::*;
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::sync::mpsc;

        /// Outbound server→client traffic: JSON signals go out as
        /// single text frames; Raw lets tests inject pings, fragments,
        /// and hostile shapes byte-exactly.
        enum Outbound {
            Json(serde_json::Value),
            Raw(u8, bool, Vec<u8>),
            Pong,
        }

        struct Conn {
            tx: mpsc::UnboundedSender<Outbound>,
            epoch: u64,
        }

        struct BlobSlot {
            id: String,
            from: String,
            payload: serde_json::Value,
        }

        struct State {
            pins: HashMap<String, String>,
            conns: HashMap<String, Conn>,
            epoch: u64,
            blobs: HashMap<String, Vec<BlobSlot>>,
            blob_seq: u64,
        }

        pub struct FakeRelay {
            pub url: String,
            pub bad_accept: bool,
            state: Arc<Mutex<State>>,
        }

        async fn read_frame(stream: &mut (impl AsyncRead + Unpin)) -> Option<(u8, Vec<u8>)> {
            let mut hdr = [0u8; 2];
            stream.read_exact(&mut hdr).await.ok()?;
            let opcode = hdr[0] & 0x0F;
            let mut len = (hdr[1] & 0x7F) as usize;
            if len == 126 {
                let mut ext = [0u8; 2];
                stream.read_exact(&mut ext).await.ok()?;
                len = u16::from_be_bytes(ext) as usize;
            } else if len == 127 {
                let mut ext = [0u8; 8];
                stream.read_exact(&mut ext).await.ok()?;
                len = u64::from_be_bytes(ext) as usize;
                if len > 4 * 1024 * 1024 {
                    return None;
                }
            }
            let mask = if hdr[1] & 0x80 != 0 {
                let mut m = [0u8; 4];
                stream.read_exact(&mut m).await.ok()?;
                m
            } else {
                [0u8; 4]
            };
            let mut payload = vec![0u8; len];
            if len > 0 {
                stream.read_exact(&mut payload).await.ok()?;
            }
            if hdr[1] & 0x80 != 0 {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            Some((opcode, payload))
        }

        async fn write_frame(
            stream: &mut (impl AsyncWrite + Unpin),
            opcode: u8,
            fin: bool,
            data: &[u8],
        ) -> std::io::Result<()> {
            let mut head = vec![(if fin { 0x80 } else { 0x00 }) | opcode, 0x00];
            if data.len() < 126 {
                head[1] |= data.len() as u8;
            } else if data.len() <= 0xFFFF {
                head[1] |= 126;
                head.extend_from_slice(&(data.len() as u16).to_be_bytes());
            } else {
                head[1] |= 127;
                head.extend_from_slice(&(data.len() as u64).to_be_bytes());
            }
            stream.write_all(&head).await?;
            stream.write_all(data).await?;
            stream.flush().await
        }

        fn send_val(tx: &mpsc::UnboundedSender<Outbound>, v: serde_json::Value) {
            let _ = tx.send(Outbound::Json(v));
        }

        async fn handle_conn(
            mut stream: tokio::net::TcpStream,
            state: Arc<Mutex<State>>,
            bad_accept: bool,
        ) {
            // HTTP upgrade.
            let mut head = Vec::new();
            let mut one = [0u8; 1];
            loop {
                if stream.read_exact(&mut one).await.is_err() {
                    return;
                }
                head.push(one[0]);
                if head.len() > 16 * 1024 {
                    return;
                }
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let head_str = String::from_utf8_lossy(&head);
            let key = head_str
                .lines()
                .skip(1)
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.trim().eq_ignore_ascii_case("sec-websocket-key") {
                        Some(v.trim().to_owned())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let accept = if bad_accept {
                "bogus".to_owned()
            } else {
                super::super::ws_accept(&key)
            };
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            if stream.write_all(resp.as_bytes()).await.is_err() {
                return;
            }
            // First frame must be signed auth.
            let (tx, rx) = mpsc::unbounded_channel::<Outbound>();
            let (opcode, raw) = match read_frame(&mut stream).await {
                Some(v) => v,
                None => return,
            };
            if opcode != 0x1 {
                return;
            }
            let auth_msg: serde_json::Value = match serde_json::from_slice(&raw) {
                Ok(v) => v,
                Err(_) => return,
            };
            if auth_msg.get("type").and_then(|t| t.as_str()) != Some("auth") {
                let _ = write_frame(&mut stream, 0x1, true, br#"{"error":"auth required"}"#).await;
                return;
            }
            let device = auth_msg
                .get("from")
                .and_then(|f| f.as_str())
                .unwrap_or("")
                .to_owned();
            let payload: RelayAuthPayload = match serde_json::from_value(
                auth_msg.get("payload").cloned().unwrap_or_default(),
            ) {
                Ok(p) => p,
                Err(_) => return,
            };
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if device.is_empty() || relay_auth::verify_auth(&device, &payload, now).is_none() {
                let _ = write_frame(
                    &mut stream,
                    0x1,
                    true,
                    br#"{"error":"auth signature invalid"}"#,
                )
                .await;
                return;
            }
            {
                let pinned = state.lock().unwrap().pins.get(&device).cloned();
                if let Some(pinned) = pinned
                    && pinned != payload.pubkey
                {
                    let _ = write_frame(
                        &mut stream,
                        0x1,
                        true,
                        br#"{"error":"device id already bound to another key"}"#,
                    )
                    .await;
                    return;
                }
                let mut st = state.lock().unwrap();
                st.pins.insert(device.clone(), payload.pubkey.clone());
                st.epoch += 1;
                let epoch = st.epoch;
                st.conns.insert(
                    device.clone(),
                    Conn {
                        tx: tx.clone(),
                        epoch,
                    },
                );
            }
            let hello = serde_json::json!({"status": "authed"});
            if write_frame(&mut stream, 0x1, true, hello.to_string().as_bytes())
                .await
                .is_err()
            {
                return;
            }
            // Drain stored blobs first (oldest-first), each tagged with
            // its slot id for acking: same order as the Go drain.
            let my_epoch = state.lock().unwrap().epoch;
            let drain: Vec<(String, String, serde_json::Value)> = state
                .lock()
                .unwrap()
                .blobs
                .get(&device)
                .map(|slots| {
                    slots
                        .iter()
                        .map(|s| (s.id.clone(), s.from.clone(), s.payload.clone()))
                        .collect()
                })
                .unwrap_or_default();
            for (id, from, payload) in drain {
                send_val(
                    &tx,
                    serde_json::json!({
                        "type": "signal", "from": from, "to": device,
                        "payload": payload, "ts": 1, "blob": id,
                    }),
                );
            }
            // Split halves: the writer task owns the write half plus the
            // outbound queue; the routing loop below owns the read half.
            // Server-routed messages go out unmasked.
            let (mut rd, mut wr) = stream.into_split();
            let writer = tokio::spawn(async move {
                let mut rx = rx;
                while let Some(msg) = rx.recv().await {
                    let ok = match msg {
                        Outbound::Json(v) => {
                            write_frame(&mut wr, 0x1, true, v.to_string().as_bytes()).await
                        }
                        Outbound::Raw(op, fin, b) => write_frame(&mut wr, op, fin, &b).await,
                        Outbound::Pong => write_frame(&mut wr, 0xA, true, &[]).await,
                    };
                    if ok.is_err() {
                        break;
                    }
                }
            });
            // Route inbound signals by To (server never parses payload).
            // Pings are answered through the outbound queue (the write
            // half lives in the writer task, never here).
            let session = device.clone();
            loop {
                // Evicted by a same-id reconnect (server keeps exactly one
                // session per device): stop reading, drain, and let the
                // socket close so the client reconnects cleanly.
                let current = state
                    .lock()
                    .unwrap()
                    .conns
                    .get(&session)
                    .map(|c| c.epoch)
                    .unwrap_or(u64::MAX);
                if current != my_epoch {
                    break;
                }
                let Some((opcode, raw)) = read_frame(&mut rd).await else {
                    break;
                };
                if opcode == 0x9 {
                    let _ = tx.send(Outbound::Pong);
                    continue;
                }
                if opcode == 0x8 {
                    break;
                }
                if opcode != 0x1 {
                    continue;
                }
                let msg: serde_json::Value = match serde_json::from_slice(&raw) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                if msg.get("from").and_then(|f| f.as_str()) != Some(session.as_str()) {
                    send_val(&tx, serde_json::json!({"error": "from mismatch"}));
                    continue;
                }
                let mtype = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if mtype == "store" {
                    // TTL slot for an offline peer (quota/expiry
                    // live server-side in Go; the harness keeps
                    // unbounded slots; protocol shape is the test
                    // target here, not quota).
                    let to = msg.get("to").and_then(|t| t.as_str()).unwrap_or("");
                    let payload = msg
                        .get("payload")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    if to.is_empty() || payload.is_null() {
                        send_val(&tx, serde_json::json!({"error": "missing recipient"}));
                        continue;
                    }
                    let mut st = state.lock().unwrap();
                    st.blob_seq += 1;
                    let id = format!("{}-{:x}", st.blob_seq, st.blob_seq);
                    st.blobs.entry(to.to_owned()).or_default().push(BlobSlot {
                        id: id.clone(),
                        from: session.clone(),
                        payload,
                    });
                    send_val(
                        &tx,
                        serde_json::json!({"status": "stored", "to": to, "blob": id}),
                    );
                    continue;
                }
                if mtype == "ack" {
                    let id = msg
                        .get("payload")
                        .and_then(|p| p.get("blob"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    let mut st = state.lock().unwrap();
                    if let Some(slots) = st.blobs.get_mut(&session) {
                        slots.retain(|s| s.id != id);
                    }
                    send_val(&tx, serde_json::json!({"status": "acked", "blob": id}));
                    continue;
                }
                if mtype == "erase_me" {
                    {
                        let mut st = state.lock().unwrap();
                        st.conns.remove(&session);
                        st.pins.remove(&session);
                        st.blobs.remove(&session);
                    }
                    send_val(&tx, serde_json::json!({"status": "forgotten"}));
                    break;
                }
                let to = msg.get("to").and_then(|t| t.as_str()).unwrap_or("");
                if to.is_empty() {
                    send_val(&tx, serde_json::json!({"error": "missing recipient"}));
                    continue;
                }
                let target = state.lock().unwrap().conns.get(to).map(|c| c.tx.clone());
                match target {
                    Some(peer) => send_val(&peer, msg),
                    None => send_val(
                        &tx,
                        serde_json::json!({"error": "404 Peer Offline", "to": to}),
                    ),
                }
            }
            // Release the map's sender first (epoch-checked, so a
            // replacement that raced us keeps its slot): the writer is
            // then fed only by the local queue, drains it, and ends.
            // (erase_me already dropped pins + slot in its arm; this is
            // a no-op for it.)
            {
                let mut st = state.lock().unwrap();
                if st.conns.get(&session).is_some_and(|c| c.epoch == my_epoch) {
                    st.conns.remove(&session);
                }
            }
            drop(tx);
            let _ = writer.await;
        }

        impl FakeRelay {
            pub async fn spawn() -> Self {
                Self::spawn_with(false).await
            }

            pub async fn spawn_with(bad_accept: bool) -> Self {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("loopback bind");
                let port = listener.local_addr().expect("port").port();
                let state = Arc::new(Mutex::new(State {
                    pins: HashMap::new(),
                    conns: HashMap::new(),
                    epoch: 0,
                    blobs: HashMap::new(),
                    blob_seq: 0,
                }));
                let accept_state = Arc::clone(&state);
                tokio::spawn(async move {
                    loop {
                        let Ok((stream, _)) = listener.accept().await else {
                            return;
                        };
                        let st = Arc::clone(&accept_state);
                        tokio::spawn(async move {
                            handle_conn(stream, st, bad_accept).await;
                        });
                    }
                });
                Self {
                    url: format!("ws://127.0.0.1:{port}/sync"),
                    bad_accept,
                    state,
                }
            }

            /// Inject a raw server→client frame to a connected device
            /// (pings, fragments, hostile shapes). Test-only backdoor for
            /// proving the client's frame layer, not the protocol.
            pub fn inject(&self, device: &str, opcode: u8, fin: bool, bytes: Vec<u8>) -> bool {
                self.state
                    .lock()
                    .ok()
                    .and_then(|st| st.conns.get(device).map(|c| c.tx.clone()))
                    .map(|tx| tx.send(Outbound::Raw(opcode, fin, bytes)).is_ok())
                    .unwrap_or(false)
            }
        }
    }

    fn test_identity() -> (DeviceIdentity, String) {
        let id = DeviceIdentity::generate().expect("identity");
        let name = id.device_id.to_string();
        (id, name)
    }

    #[tokio::test]
    async fn handshake_rejects_bad_accept() {
        let relay = harness::FakeRelay::spawn_with(true).await;
        assert!(relay.bad_accept);
        let (id, name) = test_identity();
        let err = RelayConnection::connect(&relay.url, &name, &id)
            .await
            .expect_err("bad accept must fail");
        assert_eq!(err, RelayError::Handshake);
    }

    #[tokio::test]
    async fn auth_rejects_wrong_key_takeover() {
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let (b, _) = test_identity();
        let _conn_a = RelayConnection::connect(&relay.url, &name_a, &a)
            .await
            .expect("first auth");
        // Same id, different key: pinned slot refuses.
        let err = RelayConnection::connect(&relay.url, &name_a, &b)
            .await
            .expect_err("takeover refused");
        assert!(
            matches!(err, RelayError::Auth(ref s) if s.contains("already bound")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn spoofed_from_rejected_by_server() {
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let mut conn = RelayConnection::connect(&relay.url, &name_a, &a)
            .await
            .expect("auth");
        // Bypass send_signal stamping: raw envelope with forged From.
        let forged = serde_json::json!({
            "type": "signal", "from": "someone-else", "to": "nobody",
            "payload": {}, "ts": 1,
        });
        conn.send_json(&forged).await.expect("write");
        let reply = conn.recv_json().await.expect("reply");
        assert_eq!(
            reply.get("error").and_then(|e| e.as_str()),
            Some("from mismatch")
        );
    }

    #[tokio::test]
    async fn signal_routes_by_to_and_404s_offline() {
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let (b, name_b) = test_identity();
        let mut conn_a = RelayConnection::connect(&relay.url, &name_a, &a)
            .await
            .expect("auth a");
        let mut conn_b = RelayConnection::connect(&relay.url, &name_b, &b)
            .await
            .expect("auth b");
        conn_a
            .send_signal("signal", &name_b, serde_json::json!({"ping": 1}))
            .await
            .expect("send");
        let got = conn_b.recv_signal().await.expect("b receives");
        assert_eq!(got.msg_type, "signal");
        assert_eq!(got.from, name_a);
        assert_eq!(got.payload.get("ping").and_then(|p| p.as_i64()), Some(1));
        // Offline peer answers 404, session stays alive. Error objects
        // are not signal-shaped, so they arrive via the JSON reader.
        conn_a
            .send_signal("signal", "ghost-device", serde_json::json!({}))
            .await
            .expect("send");
        let reply = conn_a.recv_json().await.expect("404 reply");
        assert_eq!(
            reply.get("error").and_then(|e| e.as_str()),
            Some("404 Peer Offline")
        );
        assert_eq!(
            reply.get("to").and_then(|t| t.as_str()),
            Some("ghost-device")
        );
    }

    #[tokio::test]
    async fn oversize_send_fails_before_io() {
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let mut conn = RelayConnection::connect(&relay.url, &name_a, &a)
            .await
            .expect("auth");
        let big = "x".repeat(RELAY_MAX_MESSAGE_BYTES);
        let err = conn
            .send_signal("signal", "nobody", serde_json::Value::String(big))
            .await
            .expect_err("over cap");
        assert_eq!(err, RelayError::Oversize);
    }

    #[tokio::test]
    async fn ping_and_fragments_do_not_break_stream() {
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let mut conn = RelayConnection::connect(&relay.url, &name_a, &a)
            .await
            .expect("auth");
        // Server ping first: answered with a pong, stream survives.
        assert!(relay.inject(&name_a, 0x9, true, vec![]));
        // Then the same signal in two fragments: reassembled whole.
        let body = serde_json::json!({
            "type": "signal", "from": "peer", "to": name_a,
            "payload": {"n": 7}, "ts": 1,
        })
        .to_string()
        .into_bytes();
        let mid = body.len() / 2;
        assert!(relay.inject(&name_a, 0x1, false, body[..mid].to_vec()));
        assert!(relay.inject(&name_a, 0x0, true, body[mid..].to_vec()));
        let got = conn.recv_signal().await.expect("reassembled");
        assert_eq!(got.msg_type, "signal");
        assert_eq!(got.payload.get("n").and_then(|n| n.as_i64()), Some(7));
    }

    #[tokio::test]
    async fn link_request_response_demuxes_by_peer() {
        use std::sync::Arc;
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let (b, name_b) = test_identity();
        let link_a = Arc::new(RelayLink::new(vec![relay.url.clone()], name_a.clone(), a));
        // Responder B: raw connection answering CRDT requests.
        let mut conn_b = RelayConnection::connect(&relay.url, &name_b, &b)
            .await
            .expect("auth b");
        let responder = tokio::spawn(async move {
            let sig = conn_b.recv_signal().await.expect("request arrives");
            assert_eq!(sig.from, name_a, "request came from A");
            let msg = parse_crdt_payload(&sig.payload).expect("crdt decodes");
            assert!(matches!(msg, CrdtMessage::SyncRequest { .. }));
            let reply = crdt_payload(&CrdtMessage::SyncResponse { deltas: vec![] });
            conn_b
                .send_signal("signal", &sig.from, reply)
                .await
                .expect("reply");
        });
        // The serve loop owns the only reader: run it while requesting.
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
        let reply = link_a.request(&name_b, &req).await.expect("round trip");
        assert!(matches!(reply, CrdtMessage::SyncResponse { .. }));
        responder.await.expect("responder done");
        serve.abort();
    }

    #[tokio::test]
    async fn offline_peer_fails_fast_not_timeout() {
        use std::sync::Arc;
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let link = Arc::new(RelayLink::new(vec![relay.url.clone()], name_a, a));
        let serve_link = Arc::clone(&link);
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
        let started = Instant::now();
        let err = link
            .request("ghost-device", &req)
            .await
            .expect_err("offline");
        assert!(err.contains("404"), "got {err:?}");
        assert!(
            started.elapsed() < RELAY_REQUEST_TIMEOUT,
            "fast fail, not timeout"
        );
        serve.abort();
    }

    #[tokio::test]
    async fn untrusted_sender_never_reaches_waiter() {
        use std::sync::Arc;
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let (b, name_b) = test_identity();
        let link = Arc::new(RelayLink::new(vec![relay.url.clone()], name_a.clone(), a));
        let trust_b = name_b.clone();
        let serve_link = Arc::clone(&link);
        // A waiter for B exists; the injected reply claims to be from
        // stranger C. Trust gate drops it; the waiter stays pending.
        let serve = tokio::spawn(async move {
            serve_link
                .serve_once(|id| id == trust_b, |_, _| None, |_, _| {})
                .await
        });
        let req = CrdtMessage::SyncRequest {
            since_vector_clock: Default::default(),
            enabled_categories: Default::default(),
        };
        let link_clone = Arc::clone(&link);
        let waiter_name = name_b.clone();
        let waiter_req = req.clone();
        let waiter =
            tokio::spawn(async move { link_clone.request(&waiter_name, &waiter_req).await });
        // Let the request register, then inject a hostile reply from C.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let hostile = serde_json::json!({
            "type": "signal", "from": "stranger-c", "to": name_a,
            "payload": crdt_payload(&CrdtMessage::SyncResponse { deltas: vec![] }),
            "ts": 1,
        })
        .to_string()
        .into_bytes();
        assert!(relay.inject(&name_a, 0x1, true, hostile));
        // Waiter still pending: a second request to B fails fast as
        // already-in-flight, proving the hostile reply didn't consume it.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let dup = link.request(&name_b, &req).await.expect_err("slot held");
        assert!(dup.contains("already in flight"), "got {dup:?}");
        serve.abort();
        waiter.abort();
        let _ = b;
    }

    #[tokio::test]
    async fn same_key_reconnect_evicts_old_session() {
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let mut first = RelayConnection::connect(&relay.url, &name_a, &a)
            .await
            .expect("first auth");
        let _second = RelayConnection::connect(&relay.url, &name_a, &a)
            .await
            .expect("reconnect");
        // The evicted session's next read fails: the server keeps exactly
        // one session per device (parity with the Go replace rule).
        let gone = tokio::time::timeout(Duration::from_secs(5), first.recv_json()).await;
        assert!(
            matches!(gone, Ok(Err(_)) | Err(_)),
            "evicted session must not stay readable"
        );
    }

    #[tokio::test]
    async fn store_drain_ack_round_trip() {
        use std::sync::Arc;
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let (b, name_b) = test_identity();
        let link = Arc::new(RelayLink::new(vec![relay.url.clone()], name_a.clone(), a));
        // B offline: store accepts a slot id without any session for B.
        let payload = crdt_payload(&CrdtMessage::SyncResponse { deltas: vec![] });
        let id = link.store(&name_b, payload).await.expect("stored");
        assert!(!id.is_empty());
        // Fresh session drains it tagged; ack removes it for good.
        let mut conn = RelayConnection::connect(&relay.url, &name_b, &b)
            .await
            .expect("auth b");
        let got = conn.recv_signal().await.expect("drained signal");
        assert_eq!(got.blob.as_deref(), Some(id.as_str()));
        let ack = serde_json::json!({
            "type": "ack", "from": name_b, "to": "",
            "payload": {"blob": id}, "ts": super::now_secs(),
        });
        conn.send_json(&ack).await.expect("ack write");
        // Reconnect: nothing further drains (no redelivery after ack).
        let mut conn2 = RelayConnection::connect(&relay.url, &name_b, &b)
            .await
            .expect("re-auth b");
        let nothing = tokio::time::timeout(Duration::from_secs(2), conn2.recv_signal()).await;
        assert!(nothing.is_err(), "acked slot must not redeliver");
    }

    #[tokio::test]
    async fn erase_me_forgets_pin() {
        let relay = harness::FakeRelay::spawn().await;
        let (a, name_a) = test_identity();
        let (b, _) = test_identity();
        let mut conn_a = RelayConnection::connect(&relay.url, &name_a, &a)
            .await
            .expect("auth a");
        conn_a.send_erase_me().await.expect("erase");
        let reply = conn_a.recv_json().await.expect("forgotten");
        assert_eq!(
            reply.get("status").and_then(|s| s.as_str()),
            Some("forgotten")
        );
        // Pin gone: a different key may now claim the id.
        RelayConnection::connect(&relay.url, &name_a, &b)
            .await
            .expect("re-pin after erase");
    }
}
