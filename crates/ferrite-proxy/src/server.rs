//! Local proxy: raw tokio TCP with `httparse`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use rustls::RootCertStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::TlsConnector;

use crate::error::{ProxyError, ProxyResult};
use crate::event::{Classification, FORK_CAP, ForkTx, ForkedPayload, NetworkEvent, TelemetryTx};
use crate::verdict::{DecisionHook, RequestMeta, ResourceType, Verdict, resource_type_for_request};

pub const MAX_HEAD_BYTES: usize = 16 * 1024;
pub const MAX_HEADERS: usize = 100;
pub const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);
pub const CLIENT_HEAD_TIMEOUT: Duration = Duration::from_secs(10);
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(5);
pub const RESPONSE_HEAD_TIMEOUT: Duration = Duration::from_secs(10);
pub const PROBE_TCP_TIMEOUT: Duration = Duration::from_millis(800);
pub const PROBE_TLS_TIMEOUT: Duration = Duration::from_millis(800);
pub const POOLED_IDLE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Default)]
pub struct ProxyConfig {
    pub fixed_port: Option<u16>,
    pub archive: Option<std::sync::Arc<ArchiveProvider>>,
}

impl std::fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("fixed_port", &self.fixed_port)
            .field("archive", &self.archive.is_some())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveSnapshot {
    pub html: Vec<u8>,
    pub headers: String,
    pub timestamp: i64,
}

pub type ArchiveProvider = dyn Fn(&str) -> Option<ArchiveSnapshot> + Send + Sync;

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

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        // Best-effort shutdown signal if `stop()` was not called.
        // Join is skipped here (may block); the proxy thread observes
        // the watch channel and exits on its own.
        let _ = self.stop_tx.send(true);
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
            let workers = std::thread::available_parallelism()
                .map(|n| n.get().clamp(2, 8))
                .unwrap_or(4);
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(workers)
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
    let archive = config.archive.clone();

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
                let archive = archive.clone();
                tokio::spawn(async move {
                    handle_client(stream, hook, telemetry, fork, pool, config.fixed_port, archive).await;
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
    archive: Option<std::sync::Arc<ArchiveProvider>>,
) {
    let _ = client.set_nodelay(true);
    // Keep-alive loop: serve successive requests on one client connection.
    // `pending` holds bytes already read past the previous head (pipelined
    // body bytes); they must be consumed before reading the socket again.
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let (head, rest) = match read_head(&mut client, std::mem::take(&mut pending)).await {
            Ok(Some(v)) => v,
            // Clean EOF.
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(error = %e, "malformed client head: closing");
                return;
            }
        };
        pending = rest;

        if head.method.eq_ignore_ascii_case("CONNECT") {
            if !pending.is_empty() {
                tracing::debug!("discarding pipelined bytes after CONNECT");
            }
            handle_connect(client, &head, &*hook, &telemetry, &pool).await;
            // Tunnel owned the socket either way.
            return;
        }

        let (keep_alive, rest) = handle_forward(
            &mut client,
            &head,
            pending,
            &*hook,
            &telemetry,
            &fork,
            &pool,
            archive.clone(),
        )
        .await;
        pending = rest;
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

fn is_internal_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    h == "ferrite"
        || h == "ferrite.invalid"
        || h.ends_with(".ferrite")
        || h.ends_with(".ferrite.invalid")
}

fn is_upgradable_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if h == "localhost" || h == "127.0.0.1" || h == "::1" {
        return false;
    }
    if h.ends_with(".local") || h.ends_with(".localhost") || h.ends_with(".invalid") {
        return false;
    }
    // RFC1918: 10/8, 192.168/16, 172.16/12. ULA/link-local are never upgraded.
    if h.starts_with("10.") || h.starts_with("192.168.") {
        return false;
    }
    if let Some(rest) = h.strip_prefix("172.")
        && let Some(octet) = rest.split('.').next().and_then(|s| s.parse::<u8>().ok())
        && (16..=31).contains(&octet)
    {
        return false;
    }
    if h.starts_with("fc") || h.starts_with("fd") || h.starts_with("fe80:") {
        return false;
    }
    if h.contains(':') {
        return false;
    }
    true
}

async fn probe_https(host: &str) -> bool {
    let host_owned = host.to_owned();
    let tcp = match tokio::time::timeout(
        PROBE_TCP_TIMEOUT,
        TcpStream::connect((host_owned.clone(), 443)),
    )
    .await
    {
        Ok(Ok(s)) => s,
        _ => return false,
    };
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = std::sync::Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let connector = TlsConnector::from(config);
    let server_name = match host_owned.try_into() {
        Ok(n) => n,
        Err(_) => return false,
    };
    match tokio::time::timeout(PROBE_TLS_TIMEOUT, connector.connect(server_name, tcp)).await {
        Ok(Ok(mut tls)) => {
            let _ = tokio::time::timeout(
                Duration::from_millis(300),
                tls.write_all(b"GET / HTTP/1.0\r\nHost: ".as_ref()),
            )
            .await;
            true
        }
        _ => false,
    }
}

async fn try_serve_archive(
    client: &mut TcpStream,
    archive: &Option<std::sync::Arc<ArchiveProvider>>,
    url: &str,
) -> bool {
    let Some(provider) = archive else {
        return false;
    };
    let Some(snap) = provider(url) else {
        return false;
    };
    let banner = format!(
        "<div style=\"background:#1C1B1B;color:#E5E2E1;padding:8px;text-align:center;font-family:system-ui\">Connection Error: Displaying local snapshot from {}</div>",
        snap.timestamp
    );
    let body = format!("{}{}", banner, String::from_utf8_lossy(&snap.html));
    let body_bytes = body.as_bytes();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body_bytes.len()
    );
    let _ = client.write_all(resp.as_bytes()).await;
    let _ = client.write_all(body_bytes).await;
    true
}

/// Read one request head.
/// `prefix` holds bytes already read from the socket (pipelined).
/// Returns `None` on clean EOF with no bytes read, plus any bytes read
/// past the head terminator (request body prefix / pipelined bytes).
async fn read_head(
    stream: &mut TcpStream,
    prefix: Vec<u8>,
) -> std::io::Result<Option<(ReqHead, Vec<u8>)>> {
    let mut buf: Vec<u8> = prefix;
    if buf.windows(4).rev().take(2048).any(|w| w == b"\r\n\r\n") {
        // Entire head already buffered; fall through to parse.
    } else {
        let mut chunk = [0u8; 2048];
        loop {
            if buf.len() > MAX_HEAD_BYTES + 16 * 1024 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "head exceeds cap",
                ));
            }
            let n = tokio::time::timeout(CLIENT_HEAD_TIMEOUT, stream.read(&mut chunk)).await;
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
            if buf.windows(4).rev().take(8192).any(|w| w == b"\r\n\r\n") {
                break;
            }
            if buf.len() > MAX_HEAD_BYTES + 16 * 1024 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "head exceeds cap",
                ));
            }
        }
    }

    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "incomplete head"))?
        + 4;
    if split > MAX_HEAD_BYTES + 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "head exceeds 16KB cap",
        ));
    }

    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut headers);
    let parsed = req
        .parse(&buf[..split])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e}")))?;
    if !parsed.is_complete() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "incomplete head",
        ));
    }

    let method = req.method.unwrap_or("").to_owned();
    let target = req.path.unwrap_or("").to_owned();
    let version = req.version.unwrap_or(1);
    let mut hdrs = Vec::with_capacity(req.headers.len());
    let mut keep_alive = version != 0;
    for h in req.headers.iter() {
        let name = h.name.to_ascii_lowercase();
        if name == "connection" {
            let v = String::from_utf8_lossy(h.value).to_ascii_lowercase();
            if v.contains("close") {
                keep_alive = false;
            } else if version == 0 && v.contains("keep-alive") {
                keep_alive = true;
            }
        }
        hdrs.push((name, String::from_utf8_lossy(h.value).into_owned()));
    }

    let rest = buf[split..].to_vec();
    Ok(Some((
        ReqHead {
            method,
            target,
            headers: hdrs,
            keep_alive,
        },
        rest,
    )))
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
    if is_internal_host(&host) {
        let _ = client
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    }
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

    // Dial with one retry on transient failure: pooled sockets may have
    // gone stale between validation and use.
    let upstream = match pool.pop_or_dial(&host, port).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%host, %port, error = %e, "upstream connect failed");
            let _ = client
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
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

#[allow(clippy::too_many_arguments)]
async fn handle_forward(
    client: &mut TcpStream,
    head: &ReqHead,
    body_prefix: Vec<u8>,
    hook: &dyn DecisionHook,
    telemetry: &TelemetryTx,
    fork: &ForkTx,
    pool: &ConnPool,
    archive: Option<std::sync::Arc<ArchiveProvider>>,
) -> (bool, Vec<u8>) {
    let (host, port) = head.host_of_absolute();
    // Internal pages: only exact ferrite hosts. Never match external URLs
    // that merely contain the substring in path/query.
    if is_internal_host(&host) {
        let target_lower = head.target.to_ascii_lowercase();
        let body: Vec<u8> = if target_lower.contains("newtab") {
            br#"<!DOCTYPE html><!-- ferrite://newtab --><html><head><meta charset="utf-8"><title>New Tab - Ferrite</title><style>html,body{margin:0;height:100%;background:#020203;color:#E5E2E1;font-family:Inter,system-ui,sans-serif;display:flex;align-items:center;justify-content:center;flex-direction:column} .wrap{max-width:640px;margin:0 auto;padding:48px 24px;display:flex;flex-direction:column;align-items:center;justify-content:center;min-height:100vh} .logo{font-size:28px;font-weight:800;letter-spacing:-0.02em;color:#E5E5E5;margin:0 0 8px} .sub{color:#9CA3AF;font-size:13px;margin:0 0 24px;letter-spacing:0.08em;text-transform:uppercase;font-family:Geist Pixel Square,monospace} .search{width:100%;max-width:560px} .search input{width:100%;padding:16px;font-size:15px;border:0.5px solid #2A2A2A;border-radius:0;background:#131313;color:#E5E2E1;outline:none;font-family:JetBrains Mono,monospace} .hint{margin-top:8px;color:#9CA3AF;font-size:11px;font-family:Geist Pixel Square,monospace} .shortcuts{display:grid;grid-template-columns:repeat(4,80px);gap:16px;margin-top:32px} .tile{width:80px;height:80px;background:#131313;border:0.5px solid #2A2A2A;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:6px;cursor:pointer} .tile:hover{border-color:#4F46E5}</style></head><body><div class="wrap"><p class="logo">Ferrite</p><p class="sub">Sovereign Browser</p><div class="search"><input type="text" id="q" placeholder="Search with DuckDuckGo or enter URL - press Enter" autocomplete="off" spellcheck="false"><p class="hint">Press Enter to search or navigate. Shortcuts below are instant.</p></div><div class="shortcuts"><div class="tile" onclick="go('https://duckduckgo.com')"><strong>DDG</strong><span>DuckDuckGo</span></div><div class="tile" onclick="go('https://google.com')"><strong>G</strong><span>Google</span></div><div class="tile" onclick="go('https://github.com')"><strong>GH</strong><span>GitHub</span></div><div class="tile" onclick="go('https://news.ycombinator.com')"><strong>HN</strong><span>Hacker News</span></div></div></div><script>const el=document.getElementById('q');function go(u){try{window.ipc.postMessage(JSON.stringify({type:'navigate',url:u}))}catch(e){location.href=u}}el.addEventListener('keydown',e=>{if(e.key==='Enter'){const v=e.target.value.trim();if(v){try{window.ipc.postMessage(JSON.stringify({type:'search',query:v}))}catch(e){location.href='https://duckduckgo.com/?q='+encodeURIComponent(v)}}}});document.addEventListener('keydown',e=>{if((e.ctrlKey||e.metaKey)&&e.key.toLowerCase()==='k'){e.preventDefault();el.focus();el.select()}});</script></body></html>"#.to_vec()
        } else if target_lower.contains("settings") {
            br#"<html><body><h1>Privacy Settings</h1><p>Adblock, GPC, Real-time, HTTPS-upgrade</p></body></html>"#.to_vec()
        } else if target_lower.contains("search") {
            b"<html><body>ferrite search handled via egui</body></html>".to_vec()
        } else {
            format!("<h1>404 Ferrite - {}</h1>", host).into_bytes()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = client.write_all(resp.as_bytes()).await;
        let _ = client.write_all(&body).await;
        return (false, Vec::new());
    }
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
    // Scheme-only: `http://host:443` is NOT secure and must not evade
    // mixed-content blocking.
    let is_secure = head.target.starts_with("https://");
    let referrer = head
        .header("referer")
        .or_else(|| head.header("referrer"))
        .map(|s| s.to_owned());
    let mut meta = RequestMeta::new(host.clone(), resource).with_url(head.target.clone());
    meta.is_secure = is_secure;
    meta.referrer = referrer.clone();
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
            return (false, Vec::new());
        }
        // Blocked: 204.
        let _ = client
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await;
        // A blocked request has no body to consume; any pipelined prefix
        // belongs to the next request. Draining a declared body avoids
        // desync when the client already sent it.
        let rest = drain_request_body_prefix(client, body_prefix, head).await;
        return (head.keep_alive, rest);
    }

    // ---- Forward origin-form over the pooled upstream ----
    // Try HTTPS upgrade.
    let should_upgrade =
        !is_secure && hook.should_upgrade_https() && port == 80 && is_upgradable_host(&host);
    let (mut upstream, actual_port) = if should_upgrade && probe_https(&host).await {
        match pool.dial(&host, 443).await {
            Ok(s) => {
                tracing::debug!(%host, "https-upgrade: TLS probe succeeded, forwarding to 443");
                (s, 443)
            }
            Err(_) => match pool.pop_or_dial(&host, port).await {
                Ok(s) => (s, port),
                Err(e) => {
                    tracing::warn!(%host, error = %e, "upstream dial failed (after https-upgrade fallback)");
                    if try_serve_archive(client, &archive, &head.target).await {
                        return (false, Vec::new());
                    }
                    let _ = client
                        .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                        .await;
                    return (head.keep_alive, Vec::new());
                }
            },
        }
    } else {
        match pool.pop_or_dial(&host, port).await {
            Ok(s) => (s, port),
            Err(e) => {
                tracing::warn!(%host, error = %e, "upstream dial failed");
                if try_serve_archive(client, &archive, &head.target).await {
                    return (false, Vec::new());
                }
                let _ = client
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                    .await;
                return (head.keep_alive, Vec::new());
            }
        }
    };

    let with_gpc = hook.should_send_gpc();
    let req_head = build_origin_form_head(head, path, &host, actual_port, with_gpc);
    if let Err(e) = upstream.write_all(&req_head).await {
        tracing::warn!(%host, error = %e, "upstream write failed");
        if try_serve_archive(client, &archive, &head.target).await {
            return (false, Vec::new());
        }
        let _ = client
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
            .await;
        return (head.keep_alive, Vec::new());
    }

    // Request body: Content-Length framed or chunked, consuming any bytes
    // already buffered past the head. Remainder (pipelined next request)
    // is returned for the keep-alive loop.
    let request_body_len = head
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok());
    let request_chunked = head
        .header("transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    let body_rest = if request_chunked {
        match forward_chunked_request(client, &mut upstream, body_prefix).await {
            Ok(rest) => rest,
            Err(_) => return (false, Vec::new()),
        }
    } else if let Some(cl) = request_body_len {
        match forward_exact_request(client, &mut upstream, body_prefix, cl).await {
            Ok(rest) => rest,
            Err(_) => return (false, Vec::new()),
        }
    } else {
        body_prefix
    };

    // ---- Read response head ----
    let (resp_head, mut head_bytes, leftover) = match read_response_head(&mut upstream).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%host, error = %e, "upstream response failed");
            if try_serve_archive(client, &archive, &head.target).await {
                return (false, Vec::new());
            }
            let _ = client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await;
            return (head.keep_alive, body_rest);
        }
    };

    if (resp_head.status == 404 || resp_head.status == 500)
        && try_serve_archive(client, &archive, &head.target).await
    {
        telemetry.emit(NetworkEvent {
            timestamp: crate::event::now_millis(),
            origin_domain: host.clone(),
            request_domain: host.clone(),
            resource_type: resource,
            bytes_transferred: 0,
            classification: Classification::Allowed,
        });
        return (false, Vec::new());
    }

    let content_type = resp_head
        .header("content-type")
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_forkable =
        content_type.starts_with("text/html") || content_type.starts_with("application/pdf");
    // Only 2xx payloads are forked: error pages must never masquerade
    // as complete archived content.
    let status_ok = (200..300).contains(&resp_head.status);
    let mut fork_buf: Vec<u8> = Vec::new();
    let mut body_bytes: u64 = 0;

    if hook.should_strip_cookies(&host) {
        let stripped = strip_set_cookie(&head_bytes);
        if stripped.len() != head_bytes.len() {
            head_bytes = stripped;
        }
        let head_lower = String::from_utf8_lossy(&head_bytes).to_ascii_lowercase();
        let mut extra = Vec::new();
        if !head_lower.contains("permissions-policy:") {
            extra.extend_from_slice(
                b"Permissions-Policy: camera=(), microphone=(), geolocation=()\r\n",
            );
        }
        if !head_lower.contains("referrer-policy:") {
            extra.extend_from_slice(b"Referrer-Policy: strict-origin-when-cross-origin\r\n");
        }
        if !extra.is_empty()
            && let Some(pos) = head_bytes.windows(2).position(|w| w == b"\r\n")
        {
            let mut new_head = Vec::with_capacity(head_bytes.len() + extra.len());
            new_head.extend_from_slice(&head_bytes[..pos + 2]);
            new_head.extend_from_slice(&extra);
            new_head.extend_from_slice(&head_bytes[pos + 2..]);
            head_bytes = new_head;
        }
    }
    if let Some(csp) = {
        let mut csp_meta = RequestMeta::new(host.clone(), resource).with_url(head.target.clone());
        csp_meta.is_secure = is_secure;
        csp_meta.referrer = referrer.clone();
        hook.csp_directives(&csp_meta)
    } {
        // Never emit a second CSP when the origin already sent one:
        // intersecting policies break sites and overstate protection.
        let head_lower = String::from_utf8_lossy(&head_bytes).to_ascii_lowercase();
        if !head_lower.contains("content-security-policy:") {
            let csp_header = format!("Content-Security-Policy: {}\r\n", csp);
            if let Some(pos) = head_bytes.windows(2).position(|w| w == b"\r\n") {
                let mut new_head = Vec::with_capacity(head_bytes.len() + csp_header.len());
                new_head.extend_from_slice(&head_bytes[..pos + 2]);
                new_head.extend_from_slice(csp_header.as_bytes());
                new_head.extend_from_slice(&head_bytes[pos + 2..]);
                head_bytes = new_head;
            }
        }
    }

    // Relay head to client verbatim.
    if client.write_all(&head_bytes).await.is_err() {
        return (false, Vec::new());
    }

    let no_body = resp_head.status == 204
        || resp_head.status == 304
        || head.method.eq_ignore_ascii_case("HEAD");

    // Leftover bytes are body bytes already read past the head terminator.
    // Content-Length framing: forward raw and subtract from remaining.
    // Chunked framing: feed through the decoder (may end mid-chunk), so
    // keep as a prefix buffer instead of forwarding raw here.
    let leftover_len = leftover.len() as u64;
    let chunked_prefix: Vec<u8> = if resp_head.is_chunked && !no_body {
        leftover
    } else {
        if !leftover.is_empty() {
            if is_forkable && fork_buf.len() < FORK_CAP {
                let take = leftover.len().min(FORK_CAP - fork_buf.len());
                fork_buf.extend_from_slice(&leftover[..take]);
            }
            if client.write_all(&leftover).await.is_err() {
                return (false, Vec::new());
            }
            body_bytes += leftover_len;
        }
        Vec::new()
    };

    let upstream_keep_alive = resp_head.keep_alive;
    // EOF-delimited responses close the connection by definition, so the
    // socket must never be re-pooled.
    let is_eof_body =
        !no_body && resp_head.header("content-length").is_none() && !resp_head.is_chunked;
    let relay_result = if no_body {
        Ok(())
    } else if let Some(cl) = resp_head
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
    {
        relay_tee_exact(
            &mut upstream,
            client,
            cl.saturating_sub(leftover_len),
            is_forkable,
            &mut fork_buf,
            &mut body_bytes,
        )
        .await
    } else if resp_head.is_chunked {
        relay_tee_chunked_with_prefix(
            &mut upstream,
            client,
            chunked_prefix,
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
        return (false, Vec::new());
    }

    // Re-pool upstream only when both sides keep alive, framing was exact,
    // and the body was not EOF-delimited (that socket is closed).
    if upstream_keep_alive && head.keep_alive && !is_eof_body {
        pool.put(&host, actual_port, upstream);
    }

    (head.keep_alive, body_rest)
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
    let mut has_gpc = false;
    for (k, v) in &head.headers {
        if k == "host"
            || k == "connection"
            || k == "proxy-connection"
            || k == "proxy-authorization"
            || k == "proxy-authenticate"
            || k == "keep-alive"
        {
            continue;
        }
        if k == "sec-gpc" {
            has_gpc = true;
        }
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if with_gpc && !has_gpc {
        out.extend_from_slice(b"sec-gpc: 1\r\n");
    }
    // Do not synthesize Content-Length: absence of CL/TE already means
    // zero-length body per RFC 9112 §6.1. Injecting `Content-Length: 0`
    // on GET/HEAD desyncs strict origins.
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
        let n = tokio::time::timeout(RESPONSE_HEAD_TIMEOUT, stream.read(&mut chunk)).await;
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
    let version = resp.version.unwrap_or(1);
    // HTTP/1.0 defaults to close; HTTP/1.1 defaults to keep-alive.
    let mut keep_alive = version != 0;
    let mut is_chunked = false;
    for h in resp.headers.iter() {
        let name = h.name.to_ascii_lowercase();
        match name.as_str() {
            "connection" => {
                let v = String::from_utf8_lossy(h.value).to_ascii_lowercase();
                if v.contains("close") {
                    keep_alive = false;
                } else if version == 0 && v.contains("keep-alive") {
                    keep_alive = true;
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
    // A response without Content-Length and without chunked framing is
    // EOF-delimited and therefore never reusable.
    if resp
        .headers
        .iter()
        .all(|h| !h.name.eq_ignore_ascii_case("content-length"))
        && !is_chunked
        && status != 204
        && status != 304
    {
        keep_alive = false;
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

/// Forward exactly `len` request bytes consuming `prefix` first.
/// Returns bytes beyond the body (pipelined next request).
async fn forward_exact_request(
    src: &mut TcpStream,
    dst: &mut TcpStream,
    prefix: Vec<u8>,
    len: u64,
) -> std::io::Result<Vec<u8>> {
    let take = (prefix.len() as u64).min(len) as usize;
    if take > 0 {
        dst.write_all(&prefix[..take]).await?;
    }
    let remaining = len - take as u64;
    if remaining > 0 {
        relay_exact(src, dst, remaining).await?;
        return Ok(Vec::new());
    }
    Ok(prefix[take..].to_vec())
}

/// Drain (without forwarding) a request body for blocked requests so the
/// next pipelined request stays aligned. Returns trailing bytes.
async fn drain_request_body_prefix(
    src: &mut TcpStream,
    prefix: Vec<u8>,
    head: &ReqHead,
) -> Vec<u8> {
    let chunked = head
        .header("transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    if chunked {
        let mut drain = BufferedUpstream {
            buf: prefix,
            pos: 0,
        };
        loop {
            let Ok(line) = drain.read_line(src).await else {
                return Vec::new();
            };
            let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
                .unwrap_or(usize::MAX);
            if size == usize::MAX {
                return Vec::new();
            }
            if size == 0 {
                loop {
                    let Ok(l) = drain.read_line(src).await else {
                        return Vec::new();
                    };
                    if l.is_empty() {
                        return drain.remainder();
                    }
                }
            }
            let mut left = size as u64;
            let mut tmp = [0u8; 8192];
            while left > 0 {
                let want = (tmp.len() as u64).min(left) as usize;
                let mut slice = &mut tmp[..want];
                // Drain via buffered read.
                let mut got = 0;
                while !slice.is_empty() {
                    match drain.fill(src, slice).await {
                        Ok(0) => return Vec::new(),
                        Ok(n) => {
                            got += n;
                            slice = &mut slice[n..];
                        }
                        Err(_) => return Vec::new(),
                    }
                }
                left -= got as u64;
            }
            let mut crlf = [0u8; 2];
            if drain.read_exact(src, &mut crlf).await.is_err() {
                return Vec::new();
            }
        }
    }
    if let Some(cl) = head
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
    {
        if prefix.len() as u64 >= cl {
            return prefix[cl as usize..].to_vec();
        }
        let mut left = cl - prefix.len() as u64;
        let mut tmp = [0u8; 8192];
        while left > 0 {
            let want = (tmp.len() as u64).min(left) as usize;
            match src.read(&mut tmp[..want]).await {
                Ok(0) => return Vec::new(),
                Ok(n) => left -= n as u64,
                Err(_) => return Vec::new(),
            }
        }
    }
    prefix
}

/// Forward a chunked request body consuming `prefix` first.
/// Returns bytes after the terminating chunk (pipelined next request).
async fn forward_chunked_request(
    src: &mut TcpStream,
    dst: &mut TcpStream,
    prefix: Vec<u8>,
) -> std::io::Result<Vec<u8>> {
    let mut buf = BufferedUpstream {
        buf: prefix,
        pos: 0,
    };
    loop {
        let size_line = buf.read_line(src).await?;
        dst.write_all(size_line.as_bytes()).await?;
        dst.write_all(b"\r\n").await?;
        let size_str = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk size"))?;
        if size == 0 {
            loop {
                let l = buf.read_line(src).await?;
                dst.write_all(l.as_bytes()).await?;
                dst.write_all(b"\r\n").await?;
                if l.is_empty() {
                    return Ok(buf.remainder());
                }
            }
        }
        let mut left = size as u64;
        let mut tmp = [0u8; 16 * 1024];
        while left > 0 {
            let want = (tmp.len() as u64).min(left) as usize;
            let n = buf.fill(src, &mut tmp[..want]).await?;
            dst.write_all(&tmp[..n]).await?;
            left -= n as u64;
        }
        let mut crlf = [0u8; 2];
        buf.read_exact(src, &mut crlf).await?;
        if crlf != *b"\r\n" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bad chunk terminator",
            ));
        }
        dst.write_all(b"\r\n").await?;
    }
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

/// Buffered chunked relay: `prefix` holds body bytes already read past the
/// response head (may end mid-chunk). All reads go through the prefix first.
async fn relay_tee_chunked_with_prefix(
    src: &mut TcpStream,
    dst: &mut TcpStream,
    prefix: Vec<u8>,
    tee: bool,
    fork_buf: &mut Vec<u8>,
    counted: &mut u64,
) -> std::io::Result<()> {
    let mut buffered = BufferedUpstream {
        buf: prefix,
        pos: 0,
    };
    loop {
        let size_line = buffered.read_line(src).await?;
        let size_str = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk size"))?;
        if size == 0 {
            // Consume trailers until blank line, then terminate framing.
            loop {
                let l = buffered.read_line(src).await?;
                if l.is_empty() {
                    dst.write_all(b"0\r\n\r\n").await?;
                    return Ok(());
                }
            }
        }
        // Forward the chunk header before the payload.
        dst.write_all(format!("{size:x}\r\n").as_bytes()).await?;
        let mut remaining = size;
        let mut buf = vec![0u8; size.clamp(1, 16 * 1024)];
        while remaining > 0 {
            let want = buf.len().min(remaining);
            let n = buffered.read_exact(src, &mut buf[..want]).await?;
            if tee && fork_buf.len() < FORK_CAP {
                let take = n.min(FORK_CAP - fork_buf.len());
                fork_buf.extend_from_slice(&buf[..take]);
            }
            dst.write_all(&buf[..n]).await?;
            *counted += n as u64;
            remaining -= n;
        }
        // Chunk-terminating CRLF both consumed and forwarded.
        let mut crlf = [0u8; 2];
        buffered.read_exact(src, &mut crlf).await?;
        if crlf != *b"\r\n" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bad chunk terminator",
            ));
        }
        dst.write_all(b"\r\n").await?;
    }
}

/// Bytes already read from upstream that must be consumed before the socket.
struct BufferedUpstream {
    buf: Vec<u8>,
    pos: usize,
}

impl BufferedUpstream {
    fn remainder(self) -> Vec<u8> {
        self.buf[self.pos..].to_vec()
    }

    async fn fill(&mut self, src: &mut TcpStream, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos < self.buf.len() {
            let n = (self.buf.len() - self.pos).min(out.len());
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        let n = src.read(out).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof mid chunked body",
            ));
        }
        Ok(n)
    }

    async fn read_exact(
        &mut self,
        src: &mut TcpStream,
        mut out: &mut [u8],
    ) -> std::io::Result<usize> {
        let mut total = 0;
        while !out.is_empty() {
            let n = self.fill(src, out).await?;
            total += n;
            out = &mut out[n..];
        }
        Ok(total)
    }

    async fn read_line(&mut self, src: &mut TcpStream) -> std::io::Result<String> {
        let mut line = Vec::with_capacity(64);
        let mut one = [0u8; 1];
        loop {
            self.read_exact(src, &mut one).await?;
            if one[0] == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            line.push(one[0]);
            if line.len() > 512 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "line too long",
                ));
            }
        }
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

// ---------------------------------------------------------------------------
// Connection pool: LRU of warm sockets keyed by host:port.
// request relay and re-pooled only when framing was exact + keep-alive held.
// ---------------------------------------------------------------------------

const POOL_MAX_HOSTS: usize = 64;
const POOL_PER_HOST: usize = 2;

struct PooledConn {
    stream: TcpStream,
    idle_since: std::time::Instant,
}

fn pool_key(host: &str, port: u16) -> String {
    format!("{}:{port}", host.to_ascii_lowercase())
}

#[derive(Default)]
struct Lru {
    map: std::collections::HashMap<String, Vec<PooledConn>>,
    order: std::collections::VecDeque<String>,
}

impl Lru {
    fn touch(&mut self, key: &str) {
        self.order.retain(|h| h != key);
        self.order.push_back(key.to_owned());
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

    /// Pop a live socket for `host:port` or dial a fresh one (with timeout).
    pub async fn pop_or_dial(&self, host: &str, port: u16) -> std::io::Result<TcpStream> {
        if let Some(s) = self.try_pop(host, port) {
            return Ok(s);
        }
        self.dial(host, port).await
    }

    fn try_pop(&self, host: &str, port: u16) -> Option<TcpStream> {
        let key = pool_key(host, port);
        // Pop under lock, validate outside so syscalls never run under mutex.
        let conn = {
            let mut lru = self.inner.lock().ok()?;
            lru.map.get_mut(&key)?.pop()
        }?;
        if conn.idle_since.elapsed() > POOLED_IDLE_TTL {
            return None;
        }
        if conn.stream.peer_addr().is_err() {
            return None;
        }
        if conn.stream.take_error().ok().flatten().is_some() {
            return None;
        }
        let _ = conn.stream.set_nodelay(true);
        Some(conn.stream)
    }

    /// Return a healthy keep-alive socket to the pool.
    pub fn put(&self, host: &str, port: u16, s: TcpStream) {
        let key = pool_key(host, port);
        let Ok(mut lru) = self.inner.lock() else {
            return;
        };
        let slot = lru.map.entry(key.clone()).or_default();
        if slot.len() < POOL_PER_HOST {
            slot.push(PooledConn {
                stream: s,
                idle_since: std::time::Instant::now(),
            });
        }
        lru.touch(&key);
    }

    /// Dial upstream.
    pub async fn dial(&self, host: &str, port: u16) -> std::io::Result<TcpStream> {
        match tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect((host, port))).await {
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
        // No synthesized Content-Length on bodyless GET (RFC 9112 §6.1).
        assert!(!s.to_ascii_lowercase().contains("content-length:"));
        // GPC header when enabled
        let bytes_gpc = build_origin_form_head(&head, "/a/b?x=1", "example.com", 80, true);
        let s_gpc = String::from_utf8(bytes_gpc).unwrap();
        assert!(s_gpc.to_ascii_lowercase().contains("sec-gpc: 1\r\n"));
    }

    #[test]
    fn origin_form_strips_proxy_auth_and_dedupes_gpc() {
        let head = ReqHead {
            method: "POST".to_owned(),
            target: "http://example.com/submit".to_owned(),
            headers: vec![
                ("host".to_owned(), "example.com".to_owned()),
                (
                    "proxy-authorization".to_owned(),
                    "Basic c2VjcmV0".to_owned(),
                ),
                ("proxy-authenticate".to_owned(), "Basic realm=x".to_owned()),
                ("sec-gpc".to_owned(), "1".to_owned()),
                ("content-length".to_owned(), "4".to_owned()),
            ],
            keep_alive: true,
        };
        let bytes = build_origin_form_head(&head, "/submit", "example.com", 80, true);
        let s = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
        assert!(!s.contains("proxy-authorization"));
        assert!(!s.contains("proxy-authenticate"));
        // Existing GPC is preserved, not duplicated.
        assert_eq!(s.matches("sec-gpc: 1").count(), 1);
        // Explicit Content-Length from the client is preserved.
        assert!(s.contains("content-length: 4"));
    }

    #[test]
    fn pool_key_isolates_ports_and_cases() {
        assert_eq!(pool_key("Example.COM", 80), "example.com:80");
        assert_ne!(pool_key("example.com", 80), pool_key("example.com", 8080));
        assert_ne!(pool_key("example.com", 80), pool_key("example.com", 443));
    }

    #[test]
    fn internal_host_exact_match() {
        assert!(is_internal_host("ferrite"));
        assert!(is_internal_host("FERRITE"));
        assert!(is_internal_host("ferrite.invalid"));
        assert!(is_internal_host("sub.ferrite"));
        assert!(!is_internal_host("myferrite.com"));
        assert!(!is_internal_host("ferrite.evil.com"));
        assert!(!is_internal_host("notferrite.example"));
        assert!(!is_internal_host("example.com"));
    }

    #[test]
    fn upgradable_host_scopes_rfc1918_correctly() {
        assert!(!is_upgradable_host("10.1.2.3"));
        assert!(!is_upgradable_host("192.168.1.1"));
        assert!(!is_upgradable_host("172.16.0.1"));
        assert!(!is_upgradable_host("172.31.255.255"));
        assert!(is_upgradable_host("172.15.0.1"));
        assert!(is_upgradable_host("172.32.0.1"));
        assert!(is_upgradable_host("example.com"));
        assert!(!is_upgradable_host("localhost"));
        assert!(!is_upgradable_host("fc00::1"));
    }
}
