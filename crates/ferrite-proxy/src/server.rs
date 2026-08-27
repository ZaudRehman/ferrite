//! Local proxy: raw tokio TCP with `httparse`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::error::{ProxyError, ProxyResult};
use crate::event::{Classification, FORK_CAP, ForkTx, ForkedPayload, NetworkEvent, TelemetryTx};
use crate::verdict::{DecisionHook, RequestMeta, ResourceType, Verdict, resource_type_for_request};

pub const MAX_HEAD_BYTES: usize = 16 * 1024;
pub const MAX_HEADERS: usize = 100;
pub const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default)]
pub struct ProxyConfig {
    pub fixed_port: Option<u16>,
}

pub struct ProxyHandle {
    pub port: u16,
    pub telemetry: crossbeam_channel::Receiver<NetworkEvent>,
    pub forks: crossbeam_channel::Receiver<ForkedPayload>,
    pub telemetry_dropped: Arc<AtomicU64>,
    pub fork_dropped: Arc<AtomicU64>,
    stop_tx: watch::Sender<bool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl ProxyHandle {
    pub fn stop(mut self) {
        let _ = self.stop_tx.send(true);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl std::fmt::Debug for ProxyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyHandle")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

pub fn spawn(config: ProxyConfig, hook: Arc<dyn DecisionHook>) -> ProxyResult<ProxyHandle> {
    let addr = SocketAddr::from(([127, 0, 0, 1], config.fixed_port.unwrap_or(0)));
    let listener =
        std::net::TcpListener::bind(addr).map_err(|source| ProxyError::Bind { addr, source })?;
    let port = listener.local_addr().ok().map(|a| a.port()).unwrap_or(0);
    let _ = listener.set_nonblocking(true);

    let (telemetry_tx, telemetry_rx) = TelemetryTx::new();
    let (fork_tx, fork_rx) = ForkTx::new();
    let (stop_tx, stop_rx) = watch::channel(false);

    let telemetry_dropped = Arc::clone(&telemetry_tx.dropped);
    let fork_dropped = Arc::clone(&fork_tx.dropped);

    let join = std::thread::Builder::new()
        .name("ferrite-proxy".to_owned())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("proxy runtime build");
            rt.block_on(run(listener, config, hook, telemetry_tx, fork_tx, stop_rx));
            rt.shutdown_timeout(Duration::from_secs(2));
        })
        .map_err(|source| ProxyError::Runtime(std::io::Error::other(source)))?;

    Ok(ProxyHandle {
        port,
        telemetry: telemetry_rx,
        forks: fork_rx,
        telemetry_dropped,
        fork_dropped,
        stop_tx,
        join: Some(join),
    })
}

async fn run(
    std_listener: std::net::TcpListener,
    config: ProxyConfig,
    hook: Arc<dyn DecisionHook>,
    telemetry: TelemetryTx,
    fork: ForkTx,
    mut stop_rx: watch::Receiver<bool>,
) {
    let listener = match TcpListener::from_std(std_listener) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "proxy listener conversion failed");
            return;
        }
    };

    let pool: Arc<ConnPool> = Arc::new(ConnPool::new());

    tracing::info!(
        port = listener.local_addr().map(|a| a.port()).unwrap_or(0),
        "proxy listening"
    );

    loop {
        tokio::select! {
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    tracing::info!("proxy shutdown requested");
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, _peer) = match accepted {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "accept error; continuing");
                        continue;
                    }
                };
                let hook = Arc::clone(&hook);
                let telemetry = telemetry.clone();
                let fork = fork.clone();
                let pool = Arc::clone(&pool);
                tokio::spawn(async move {
                    handle_client(stream, hook, telemetry, fork, pool, config.fixed_port).await;
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-client state machine
// ---------------------------------------------------------------------------

async fn handle_client(
    mut client: TcpStream,
    hook: Arc<dyn DecisionHook>,
    telemetry: TelemetryTx,
    fork: ForkTx,
    pool: Arc<ConnPool>,
    _fixed_port: Option<u16>,
) {
    let _ = client.set_nodelay(true);
    // Keep-alive loop: serve successive requests on one client connection.
    loop {
        let head = match read_head(&mut client).await {
            Ok(Some(h)) => h,
            // Clean EOF.
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(error = %e, "malformed client head: closing");
                return;
            }
        };

        if head.method.eq_ignore_ascii_case("CONNECT") {
            handle_connect(client, &head, &*hook, &telemetry, &pool).await;
            // Tunnel owned the socket either way.
            return;
        }

        let keep_alive = handle_forward(&mut client, &head, &*hook, &telemetry, &fork, &pool).await;
        if !keep_alive {
            return;
        }
    }
}

/// A parsed request head (request line + header list).
#[derive(Debug)]
struct ReqHead {
    method: String,
    /// CONNECT authority (`host:port`) or absolute-form target URI.
    target: String,
    /// Lower-cased header names with raw values.
    headers: Vec<(String, String)>,
    keep_alive: bool,
}

impl ReqHead {
    fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == lower)
            .map(|(_, v)| v.as_str())
    }

    fn host_of_absolute(&self) -> (String, u16) {
        // absolute-form: scheme://host[:port]/path
        let t = &self.target;
        let after_scheme = t.find("://").map(|i| &t[i + 3..]).unwrap_or(t.as_str());
        let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
        split_host_port(authority, 80)
    }
}

fn split_host_port(authority: &str, default_port: u16) -> (String, u16) {
    if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 [::1]:8080
        if let Some(end) = rest.find(']') {
            let host = &rest[..end];
            let port = rest[end + 1..]
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(default_port);
            return (host.to_owned(), port);
        }
    }
    match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_owned(), p.parse().unwrap_or(default_port))
        }
        _ => (authority.to_owned(), default_port),
    }
}

fn is_upgradable_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1" {
        return false;
    }
    if host.starts_with("10.") || host.starts_with("192.168.") || host.starts_with("172.") {
        return false;
    }
    // IPv6 literal or already bracketed
    if host.contains(':') {
        return false;
    }
    true
}

/// Read one request head.
/// Returns `None` on clean EOF with no bytes read.
async fn read_head(stream: &mut TcpStream) -> std::io::Result<Option<ReqHead>> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        if buf.len() > MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "head exceeds 16KB cap",
            ));
        }
        let n = tokio::time::timeout(UPSTREAM_TIMEOUT, stream.read(&mut chunk)).await;
        let n = match n {
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "head read timeout",
                ));
            }
            Ok(Err(e)) => return Err(e),
            Ok(Ok(0)) => {
                if buf.is_empty() {
                    return Ok(None);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "eof mid-head",
                ));
            }
            Ok(Ok(n)) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).rev().take(2048).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut headers);
    let parsed = req
        .parse(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e}")))?;
    if !parsed.is_complete() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "incomplete head",
        ));
    }

    let method = req.method.unwrap_or("").to_owned();
    let target = req.path.unwrap_or("").to_owned();
    let mut hdrs = Vec::with_capacity(req.headers.len());
    let mut keep_alive = true;
    for h in req.headers.iter() {
        let name = h.name.to_ascii_lowercase();
        if name == "connection" {
            let v = String::from_utf8_lossy(h.value).to_ascii_lowercase();
            if v.contains("close") {
                keep_alive = false;
            }
        }
        hdrs.push((name, String::from_utf8_lossy(h.value).into_owned()));
    }
    // HTTP/1.0 defaults to close unless keep-alive requested.
    if buf.starts_with(b"HTTP/1.0") && !hdrs.iter().any(|(k, _)| k == "connection") {
        keep_alive = false;
    }

    Ok(Some(ReqHead {
        method,
        target,
        headers: hdrs,
        keep_alive,
    }))
}

// ---------------------------------------------------------------------------
// CONNECT: blind tunnel
// ---------------------------------------------------------------------------

async fn handle_connect(
    mut client: TcpStream,
    head: &ReqHead,
    hook: &dyn DecisionHook,
    telemetry: &TelemetryTx,
    pool: &ConnPool,
) {
    let (host, port) = split_host_port(&head.target, 443);
    let mut meta = RequestMeta::new(host.clone(), ResourceType::Document);
    meta.is_secure = true;
    let verdict = hook.decide(&meta);
    let classification = classify(verdict);

    let event = |cls| NetworkEvent {
        timestamp: crate::event::now_millis(),
        origin_domain: host.clone(),
        request_domain: host.clone(),
        resource_type: ResourceType::Document,
        bytes_transferred: 0,
        classification: cls,
    };

    if verdict != Verdict::Allow {
        tracing::info!(%host, ?verdict, "CONNECT denied");
        telemetry.emit(event(classification));
        let _ = client
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await;
        return;
    }
    telemetry.emit(event(classification));

    let upstream = match pool.dial(&host, port).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%host, %port, error = %e, "upstream connect failed");
            let _ = client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await;
            return;
        }
    };

    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .is_err()
    {
        return;
    }

    // Blind tunnel: no TLS inspection.
    // (spec §2.2: no MITM, validation #2).
    let mut upstream = upstream;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
}

// ---------------------------------------------------------------------------
// Forward and tee.
// ---------------------------------------------------------------------------

/// Forward one request; returns whether the client connection may keep-alive.
async fn handle_forward(
    client: &mut TcpStream,
    head: &ReqHead,
    hook: &dyn DecisionHook,
    telemetry: &TelemetryTx,
    fork: &ForkTx,
    pool: &ConnPool,
) -> bool {
    let (host, port) = head.host_of_absolute();
    let (path, _query_kept_in_path) = match head.target.find("://") {
        Some(i) => {
            let after = &head.target[i + 3..];
            match after.find('/') {
                Some(s) => (&after[s..], ()),
                None => ("/", ()),
            }
        }
        None => (head.target.as_str(), ()),
    };
    let sec_fetch_dest = head.header("sec-fetch-dest");
    let resource = resource_type_for_request(sec_fetch_dest, path);
    let is_secure = head.target.starts_with("https://") || port == 443;
    let referrer = head
        .header("referer")
        .or_else(|| head.header("referrer"))
        .map(|s| s.to_owned());
    let mut meta = RequestMeta::new(host.clone(), resource);
    meta.is_secure = is_secure;
    meta.referrer = referrer;
    let verdict = hook.decide(&meta);
    let classification = classify(verdict);

    if verdict != Verdict::Allow {
        tracing::info!(%host, ?verdict, "request blocked");
        telemetry.emit(NetworkEvent {
            timestamp: crate::event::now_millis(),
            origin_domain: host.clone(),
            request_domain: host.clone(),
            resource_type: resource,
            bytes_transferred: 0,
            classification,
        });
        // Malware: 302, others: 204.
        if verdict == Verdict::BlockMalware {
            let loc = format!("ferrite://security-warning?host={host}");
            let resp = format!(
                "HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = client.write_all(resp.as_bytes()).await;
            return false;
        }
        // Blocked: 204.
        let _ = client
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await;
        return head.keep_alive;
    }

    // ---- Forward origin-form over the pooled upstream ----
    // Try HTTPS upgrade.
    let should_upgrade =
        !is_secure && hook.should_upgrade_https() && port == 80 && is_upgradable_host(&host);
    let (mut upstream, actual_port) = if should_upgrade {
        match pool.dial(&host, 443).await {
            Ok(s) => {
                tracing::debug!(%host, "https-upgrade: dialed 443, forwarding origin-form");
                (s, 443)
            }
            Err(_) => match pool.pop_or_dial(&host, port).await {
                Ok(s) => (s, port),
                Err(e) => {
                    tracing::warn!(%host, error = %e, "upstream dial failed (after https-upgrade fallback)");
                    let _ = client
                        .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                        .await;
                    return head.keep_alive;
                }
            },
        }
    } else {
        match pool.pop_or_dial(&host, port).await {
            Ok(s) => (s, port),
            Err(e) => {
                tracing::warn!(%host, error = %e, "upstream dial failed");
                let _ = client
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                    .await;
                return head.keep_alive;
            }
        }
    };

    let with_gpc = hook.should_send_gpc();
    let req_head = build_origin_form_head(head, path, &host, actual_port, with_gpc);
    if let Err(e) = upstream.write_all(&req_head).await {
        tracing::warn!(%host, error = %e, "upstream write failed");
        let _ = client
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
            .await;
        return head.keep_alive;
    }

    // Request body (if any): content-length framed.
    let request_body_len = head
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(cl) = request_body_len
        && relay_exact(client, &mut upstream, cl).await.is_err()
    {
        return false;
    }

    // ---- Read response head ----
    let (resp_head, mut head_bytes, leftover) = match read_response_head(&mut upstream).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%host, error = %e, "upstream response failed");
            let _ = client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await;
            return head.keep_alive;
        }
    };

    let content_type = resp_head
        .header("content-type")
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_forkable =
        content_type.starts_with("text/html") || content_type.starts_with("application/pdf");
    let status_ok = resp_head.status >= 200;
    let mut fork_buf: Vec<u8> = Vec::new();
    let mut body_bytes: u64 = 0;

    // Strip Set-Cookie for trackers.
    if hook.should_strip_cookies(&host) {
        let stripped = strip_set_cookie(&head_bytes);
        if stripped.len() != head_bytes.len() {
            head_bytes = stripped;
        }
    }

    // Relay head to client verbatim.
    if client.write_all(&head_bytes).await.is_err() {
        return false;
    }

    // Relay leftover body bytes.
    if !leftover.is_empty() {
        if is_forkable && fork_buf.len() < FORK_CAP {
            let take = leftover.len().min(FORK_CAP - fork_buf.len());
            fork_buf.extend_from_slice(&leftover[..take]);
        }
        if client.write_all(&leftover).await.is_err() {
            return false;
        }
        body_bytes += leftover.len() as u64;
    }

    let no_body = resp_head.status == 204
        || resp_head.status == 304
        || head.method.eq_ignore_ascii_case("HEAD");

    let upstream_keep_alive = resp_head.keep_alive;
    let relay_result = if no_body {
        Ok(())
    } else if let Some(cl) = resp_head
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
    {
        relay_tee_exact(
            &mut upstream,
            client,
            cl.saturating_sub(leftover.len() as u64),
            is_forkable,
            &mut fork_buf,
            &mut body_bytes,
        )
        .await
    } else if resp_head.is_chunked {
        relay_tee_chunked(
            &mut upstream,
            client,
            is_forkable,
            &mut fork_buf,
            &mut body_bytes,
        )
        .await
    } else {
        relay_tee_eof(
            &mut upstream,
            client,
            is_forkable,
            &mut fork_buf,
            &mut body_bytes,
        )
        .await
    };

    if !fork_buf.is_empty() && is_forkable && status_ok {
        fork.emit(ForkedPayload {
            url: head.target.clone(),
            content_type: content_type.clone(),
            head: std::mem::take(&mut fork_buf),
        });
    }

    telemetry.emit(NetworkEvent {
        timestamp: crate::event::now_millis(),
        origin_domain: host.clone(),
        request_domain: host.clone(),
        resource_type: resource,
        bytes_transferred: body_bytes,
        classification,
    });

    if relay_result.is_err() {
        return false;
    }

    // Re-pool upstream only when both sides keep alive and framing was exact.
    if upstream_keep_alive && head.keep_alive {
        pool.put(&host, upstream);
    }

    head.keep_alive
}

fn classify(v: Verdict) -> Classification {
    match v {
        Verdict::Allow => Classification::Allowed,
        Verdict::BlockAd => Classification::BlockedAd,
        Verdict::BlockTracker => Classification::BlockedTracker,
        Verdict::BlockMalware => Classification::BlockedMalware,
        Verdict::BlockMixedContent => Classification::BlockedMixedContent,
    }
}

/// Rebuild the request head in origin-form for the upstream.
fn build_origin_form_head(
    head: &ReqHead,
    path: &str,
    host: &str,
    port: u16,
    with_gpc: bool,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(head.method.as_bytes());
    out.push(b' ');
    out.extend_from_slice(path.as_bytes());
    out.extend_from_slice(b" HTTP/1.1\r\n");
    let host_hdr: String = if port == 80 {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    };
    out.extend_from_slice(format!("Host: {host_hdr}\r\n").as_bytes());
    for (k, v) in &head.headers {
        if k == "host" || k == "connection" || k == "proxy-connection" {
            continue;
        }
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if with_gpc {
        out.extend_from_slice(b"Sec-GPC: 1\r\n");
    }
    if head.header("content-length").is_none() && head.header("transfer-encoding").is_none() {
        out.extend_from_slice(b"Content-Length: 0\r\n");
    }
    let conn = if head.keep_alive {
        "keep-alive"
    } else {
        "close"
    };
    out.extend_from_slice(format!("Connection: {conn}\r\n\r\n").as_bytes());
    out
}

/// Read an upstream response head; returns parsed head + raw bytes relayed.
async fn read_response_head(
    stream: &mut TcpStream,
) -> std::io::Result<(RespHead, Vec<u8>, Vec<u8>)> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        if buf.len() > MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "response head exceeds cap",
            ));
        }
        let n = tokio::time::timeout(UPSTREAM_TIMEOUT, stream.read(&mut chunk)).await;
        let n = match n {
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "response head timeout",
                ));
            }
            Ok(Err(e)) => return Err(e),
            Ok(Ok(0)) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "eof mid response head",
                ));
            }
            Ok(Ok(n)) => n,
        };
        let start = buf.len();
        buf.extend_from_slice(&chunk[..n]);
        if buf[start..].windows(4).any(|w| w == b"\r\n\r\n")
            || buf.windows(4).any(|w| w == b"\r\n\r\n")
        {
            break;
        }
    }

    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("terminator found above")
        + 4;
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut resp = httparse::Response::new(&mut headers);
    let parsed = resp
        .parse(&buf[..split])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e}")))?;
    if !parsed.is_complete() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "incomplete response head",
        ));
    }

    let status = resp.code.unwrap_or(0);
    let mut keep_alive = true;
    let mut is_chunked = false;
    for h in resp.headers.iter() {
        let name = h.name.to_ascii_lowercase();
        match name.as_str() {
            "connection" => {
                let v = String::from_utf8_lossy(h.value).to_ascii_lowercase();
                if v.contains("close") {
                    keep_alive = false;
                }
            }
            "transfer-encoding" => {
                let v = String::from_utf8_lossy(h.value).to_ascii_lowercase();
                if v.contains("chunked") {
                    is_chunked = true;
                }
            }
            _ => {}
        }
    }

    let head = RespHead {
        status,
        keep_alive,
        is_chunked,
        raw_headers: resp
            .headers
            .iter()
            .map(|h| {
                (
                    h.name.to_ascii_lowercase(),
                    String::from_utf8_lossy(h.value).into_owned(),
                )
            })
            .collect(),
    };
    let leftover = buf[split..].to_vec();
    buf.truncate(split);
    Ok((head, buf, leftover))
}

struct RespHead {
    status: u16,
    keep_alive: bool,
    is_chunked: bool,
    #[allow(dead_code)]
    raw_headers: Vec<(String, String)>,
}

impl RespHead {
    fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.raw_headers
            .iter()
            .find(|(k, _)| *k == lower)
            .map(|(_, v)| v.as_str())
    }
}

// ---------------------------------------------------------------------------
// Body relays
// ---------------------------------------------------------------------------

async fn relay_exact(
    src: &mut TcpStream,
    dst: &mut TcpStream,
    mut remaining: u64,
) -> std::io::Result<()> {
    let mut buf = [0u8; 16 * 1024];
    while remaining > 0 {
        let want = buf.len().min(remaining as usize);
        let n = src.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof mid request body",
            ));
        }
        dst.write_all(&buf[..n]).await?;
        remaining -= n as u64;
    }
    Ok(())
}

async fn relay_tee_exact(
    src: &mut TcpStream,
    dst: &mut TcpStream,
    mut remaining: u64,
    tee: bool,
    fork_buf: &mut Vec<u8>,
    counted: &mut u64,
) -> std::io::Result<()> {
    let mut buf = [0u8; 16 * 1024];
    while remaining > 0 {
        let want = buf.len().min(remaining as usize);
        let n = src.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof mid body",
            ));
        }
        if tee && fork_buf.len() < FORK_CAP {
            let take = n.min(FORK_CAP - fork_buf.len());
            fork_buf.extend_from_slice(&buf[..take]);
        }
        dst.write_all(&buf[..n]).await?;
        *counted += n as u64;
        remaining -= n as u64;
    }
    Ok(())
}

async fn relay_tee_eof(
    src: &mut TcpStream,
    dst: &mut TcpStream,
    tee: bool,
    fork_buf: &mut Vec<u8>,
    counted: &mut u64,
) -> std::io::Result<()> {
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = src.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        if tee && fork_buf.len() < FORK_CAP {
            let take = n.min(FORK_CAP - fork_buf.len());
            fork_buf.extend_from_slice(&buf[..take]);
        }
        dst.write_all(&buf[..n]).await?;
        *counted += n as u64;
    }
}

/// Minimal chunked-transfer decoder/relayer. Reads size-line + data + CRLF
/// per chunk until the 0-chunk (trailers consumed and dropped).
async fn relay_tee_chunked(
    src: &mut TcpStream,
    dst: &mut TcpStream,
    tee: bool,
    fork_buf: &mut Vec<u8>,
    counted: &mut u64,
) -> std::io::Result<()> {
    loop {
        let size_line = read_line(src).await?;
        let size_str = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk size"))?;
        if size == 0 {
            // Consume trailers until blank line.
            loop {
                let l = read_line(src).await?;
                if l.is_empty() {
                    return Ok(());
                }
            }
        }
        let mut remaining = size;
        let mut buf = vec![0u8; size.min(16 * 1024)];
        while remaining > 0 {
            let want = buf.len().min(remaining);
            let n = src.read(&mut buf[..want]).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "eof mid chunk",
                ));
            }
            if tee && fork_buf.len() < FORK_CAP {
                let take = n.min(FORK_CAP - fork_buf.len());
                fork_buf.extend_from_slice(&buf[..take]);
            }
            dst.write_all(&buf[..n]).await?;
            *counted += n as u64;
            remaining -= n;
        }
        // Chunk-terminating CRLF.
        let mut crlf = [0u8; 2];
        src.read_exact(&mut crlf).await?;
    }
}

fn strip_set_cookie(head: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(head);
    let mut out = String::new();
    for line in text.split_inclusive("\r\n") {
        if line.to_ascii_lowercase().starts_with("set-cookie:") {
            continue;
        }
        out.push_str(line);
    }
    out.into_bytes()
}

async fn read_line(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut line = Vec::with_capacity(16);
    let mut b = [0u8; 1];
    loop {
        let n = stream.read(&mut b).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof mid line",
            ));
        }
        if b[0] == b'\n' {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
        line.push(b[0]);
        if line.len() > 128 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Connection pool: LRU of warm sockets.
// request relay and re-pooled only when framing was exact + keep-alive held.
// ---------------------------------------------------------------------------

const POOL_MAX_HOSTS: usize = 64;
const POOL_PER_HOST: usize = 2;

#[derive(Default)]
struct Lru {
    map: std::collections::HashMap<String, Vec<TcpStream>>,
    order: std::collections::VecDeque<String>,
}

impl Lru {
    fn touch(&mut self, host: &str) {
        self.order.retain(|h| h != host);
        self.order.push_back(host.to_owned());
        while self.order.len() > POOL_MAX_HOSTS {
            if let Some(evict) = self.order.pop_front() {
                self.map.remove(&evict);
            }
        }
    }
}

/// LRU pool of warm upstream TCP sockets.
#[derive(Default)]
pub struct ConnPool {
    inner: std::sync::Mutex<Lru>,
}

impl std::fmt::Debug for ConnPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnPool").finish_non_exhaustive()
    }
}

impl ConnPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pop a live socket for `host` or dial a fresh one (with timeout).
    pub async fn pop_or_dial(&self, host: &str, port: u16) -> std::io::Result<TcpStream> {
        if let Some(s) = self.try_pop(host) {
            return Ok(s);
        }
        self.dial(host, port).await
    }

    fn try_pop(&self, host: &str) -> Option<TcpStream> {
        let mut lru = self.inner.lock().ok()?;
        let s = lru.map.get_mut(host)?.pop()?;
        // Liveness probe before trusting a pooled socket.
        s.peer_addr().ok()?;
        Some(s)
    }

    /// Return a healthy keep-alive socket to the pool.
    pub fn put(&self, host: &str, s: TcpStream) {
        let Ok(mut lru) = self.inner.lock() else {
            return;
        };
        let slot = lru.map.entry(host.to_owned()).or_default();
        if slot.len() < POOL_PER_HOST {
            slot.push(s);
        }
        lru.touch(host);
    }

    /// Dial upstream.
    pub async fn dial(&self, host: &str, port: u16) -> std::io::Result<TcpStream> {
        match tokio::time::timeout(UPSTREAM_TIMEOUT, TcpStream::connect((host, port))).await {
            Ok(Ok(s)) => {
                let _ = s.set_nodelay(true);
                Ok(s)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("upstream connect timed out: {host}:{port}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_host_port_v4_and_v6() {
        assert_eq!(
            split_host_port("example.com:443", 80),
            ("example.com".to_owned(), 443)
        );
        assert_eq!(
            split_host_port("example.com", 80),
            ("example.com".to_owned(), 80)
        );
        assert_eq!(split_host_port("[::1]:8080", 80), ("::1".to_owned(), 8080));
        assert_eq!(split_host_port("[::1]", 443), ("::1".to_owned(), 443));
    }

    #[test]
    fn origin_form_head_rewrites_and_strips() {
        let head = ReqHead {
            method: "GET".to_owned(),
            target: "http://example.com/a/b?x=1".to_owned(),
            headers: vec![
                ("host".to_owned(), "example.com".to_owned()),
                ("accept".to_owned(), "*/*".to_owned()),
                ("connection".to_owned(), "keep-alive".to_owned()),
            ],
            keep_alive: true,
        };
        let bytes = build_origin_form_head(&head, "/a/b?x=1", "example.com", 80, false);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("GET /a/b?x=1 HTTP/1.1\r\n"));
        assert!(s.contains("Host: example.com\r\n"));
        assert!(!s.contains("proxy-connection"));
        assert!(s.contains("accept: */*\r\n"));
        assert!(s.ends_with("Connection: keep-alive\r\n\r\n"));
        // GPC header when enabled
        let bytes_gpc = build_origin_form_head(&head, "/a/b?x=1", "example.com", 80, true);
        let s_gpc = String::from_utf8(bytes_gpc).unwrap();
        assert!(s_gpc.contains("Sec-GPC: 1\r\n"));
    }
}
