//! Part 4 end-to-end integration tests (spec §6 validation #1-#5).
//!
//! Spawns a real local upstream HTTP/1.1 server (raw tokio, mirroring the
//! proxy's own stack) and drives the real proxy over real sockets:
//! * absolute-form forward + telemetry + fork (checks #3, #5)
//! * CONNECT tunnel round-trip with zero TLS interference (check #2 semantics)
//! * blocked-domain 403/204 via `BlockListHook`
//! * garbage-byte crash isolation (check #4)
//! * 100-parallel connection smoke (check #1, scaled for CI)

use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use ferrite_proxy::{BlockListHook, ProxyConfig, ProxyHandle, TelemetryTx};

// ---------------------------------------------------------------------------
// Local upstream HTTP/1.1 test server (raw tokio: same stack as the proxy)
// ---------------------------------------------------------------------------

struct Upstream {
    port: u16,
}

impl Upstream {
    fn spawn() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("upstream runtime");
        let (port_tx, port_rx) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || {
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                port_tx.send(listener.local_addr().unwrap().port()).unwrap();
                loop {
                    let Ok((mut sock, _)) = listener.accept().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 4096];
                        // Read until end of head; serve based on path.
                        loop {
                            let n = sock.read(&mut chunk).await.unwrap();
                            if n == 0 {
                                return;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        let head = String::from_utf8_lossy(&buf).into_owned();
                        let path = head
                            .lines()
                            .next()
                            .and_then(|l| l.split(' ').nth(1))
                            .unwrap_or("/")
                            .to_owned();
                        let (status, ctype, body): (&str, &str, &[u8]) = match path.as_str() {
                            "/html" => (
                                "200 OK",
                                "text/html; charset=utf-8",
                                b"<html><body>hello ferrite</body></html>",
                            ),
                            "/pdf" => ("200 OK", "application/pdf", b"%PDF-1.4 fake"),
                            "/big" => ("200 OK", "text/plain", &vec![b'x'; 512 * 1024]),
                            "/notfound" => ("404 Not Found", "text/plain", b"nope"),
                            _ => ("200 OK", "text/plain", b"ok"),
                        };
                        let resp = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                            body.len()
                        );
                        sock.write_all(resp.as_bytes()).await.unwrap();
                        sock.write_all(body).await.unwrap();
                    });
                }
            });
        });
        let port = port_rx.recv().expect("upstream port");
        std::mem::forget(join);
        Self { port }
    }
}

// ---------------------------------------------------------------------------
// Raw client helpers
// ---------------------------------------------------------------------------

async fn send_recv(port: u16, request: &str) -> String {
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to proxy");
    sock.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    // Read until we have head + content-length body (test responses are small).
    loop {
        let n = sock.read(&mut buf).await.expect("read");
        out.extend_from_slice(&buf[..n]);
        let s = String::from_utf8_lossy(&out);
        if let Some(idx) = s.find("\r\n\r\n") {
            let head = &s[..idx];
            let cl = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if out.len() >= idx + 4 + cl {
                break;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

fn spawn_proxy(hook: Arc<dyn ferrite_proxy::DecisionHook>) -> ProxyHandle {
    ferrite_proxy::spawn(ProxyConfig::default(), hook).expect("proxy spawn")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn forward_html_telemetry_and_fork() {
    let upstream = Upstream::spawn();
    let handle = spawn_proxy(Arc::new(ferrite_proxy::AllowAllHook));

    // Absolute-form GET through the proxy.
    let req = format!(
        "GET http://127.0.0.1:{}/html HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        upstream.port
    );
    let resp = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(send_recv(handle.port, &req));

    assert!(resp.starts_with("HTTP/1.1 200"), "got: {resp}");
    assert!(resp.contains("hello ferrite"), "body proxied");

    // Telemetry: one Allowed event for the document.
    let ev = handle
        .telemetry
        .recv_timeout(Duration::from_secs(2))
        .expect("telemetry event");
    assert_eq!(ev.request_domain, "127.0.0.1");
    assert_eq!(ev.classification, ferrite_proxy::Classification::Allowed);
    assert_eq!(
        ev.bytes_transferred,
        "<html><body>hello ferrite</body></html>".len() as u64
    );

    // Fork: html payload captured (validation #5).
    let fork = handle
        .forks
        .recv_timeout(Duration::from_secs(2))
        .expect("forked payload");
    assert!(fork.content_type.starts_with("text/html"));
    assert!(fork.head.windows(5).any(|w| w == b"hello"));

    handle.stop();
}

#[test]
fn connect_tunnel_round_trip() {
    let upstream = Upstream::spawn();
    let handle = spawn_proxy(Arc::new(ferrite_proxy::AllowAllHook));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let resp = rt.block_on(async move {
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", handle.port))
            .await
            .unwrap();
        // CONNECT to the plaintext upstream: the proxy must tunnel bytes
        // untouched (validation #2 semantics: no MITM, no rewriting).
        let connect = format!(
            "CONNECT 127.0.0.1:{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            upstream.port, upstream.port
        );
        sock.write_all(connect.as_bytes()).await.unwrap();

        // Read the 200 established head.
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut chunk).await.unwrap();
            buf.extend_from_slice(&chunk[..n]);
        }
        let head = String::from_utf8_lossy(&buf).into_owned();
        assert!(
            head.starts_with("HTTP/1.1 200"),
            "tunnel established: {head}"
        );

        // Inside the "tunnel": plain HTTP to upstream (stands in for TLS).
        sock.write_all(b"GET /html HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut out = Vec::new();
        loop {
            let n = sock.read(&mut chunk).await.unwrap();
            out.extend_from_slice(&chunk[..n]);
            let s = String::from_utf8_lossy(&out);
            if s.contains("hello ferrite") {
                break;
            }
        }
        String::from_utf8(out).unwrap_or_default()
    });

    assert!(resp.contains("hello ferrite"), "tunnel relayed body");
    handle.stop();
}

#[test]
fn blocked_domain_403_and_204() {
    let _upstream = Upstream::spawn();
    let handle = spawn_proxy(Arc::new(BlockListHook::new(&["ads.example"])));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // CONNECT to blocked host → 403.
    let r1 = rt.block_on(send_recv(
        handle.port,
        "CONNECT ads.example:443 HTTP/1.1\r\nHost: ads.example:443\r\n\r\n",
    ));
    assert!(r1.starts_with("HTTP/1.1 403"), "CONNECT blocked: {r1}");

    // Absolute-form to blocked host → 204.
    let r2 = rt.block_on(send_recv(
        handle.port,
        "GET http://ads.example/pixel.gif HTTP/1.1\r\nHost: ads.example\r\nConnection: close\r\n\r\n",
    ));
    assert!(r2.starts_with("HTTP/1.1 204"), "HTTP blocked: {r2}");

    // Telemetry recorded both blocks with BlockedTracker.
    let mut blocked = 0;
    while let Ok(ev) = handle.telemetry.recv_timeout(Duration::from_millis(500)) {
        if ev.classification == ferrite_proxy::Classification::BlockedTracker {
            blocked += 1;
        }
    }
    assert_eq!(blocked, 2, "both blocked requests telemetered");
    handle.stop();
}

#[test]
fn garbage_bytes_do_not_crash_proxy() {
    let _upstream = Upstream::spawn();
    let handle = spawn_proxy(Arc::new(ferrite_proxy::AllowAllHook));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        // Garbage burst.
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", handle.port))
            .await
            .unwrap();
        sock.write_all(&[0xFF, 0x00, 0x13, b'\r', b'\n', 0x7F])
            .await
            .unwrap();
        let mut buf = [0u8; 64];
        // Server may respond or close; either is fine: no panic.
        let _ = tokio::time::timeout(Duration::from_secs(2), sock.read(&mut buf)).await;
    });

    // Proxy still serves a valid request afterwards (check #4).
    let req = format!(
        "GET http://127.0.0.1:{}/html HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        _upstream.port
    );
    let resp = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(send_recv(handle.port, &req));
    assert!(resp.contains("hello ferrite"), "proxy survived: {resp}");
    handle.stop();
}

#[test]
fn one_hundred_parallel_forwards() {
    let upstream = Upstream::spawn();
    let handle = spawn_proxy(Arc::new(ferrite_proxy::AllowAllHook));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();

    let up = upstream.port;
    let results: Vec<std::result::Result<(), ()>> = rt.block_on(async move {
        let mut handles: Vec<tokio::task::JoinHandle<std::result::Result<(), ()>>> = Vec::new();
        for _ in 0..100 {
            let port = handle.port;
            handles.push(tokio::spawn(async move {
                let mut sock =
                    tokio::net::TcpStream::connect(("127.0.0.1", port)).await.map_err(|_| ())?;
                let req = format!(
                    "GET http://127.0.0.1:{up}/html HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
                );
                sock.write_all(req.as_bytes()).await.map_err(|_| ())?;
                let mut out = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = sock.read(&mut buf).await.map_err(|_| ())?;
                    out.extend_from_slice(&buf[..n]);
                    if String::from_utf8_lossy(&out).contains("hello ferrite") {
                        return Ok(());
                    }
                }
            }));
        }
        let joined: Vec<std::result::Result<(), ()>> =
            futures_join(handles).await.into_iter().map(|r| r.flatten()).collect();
        joined
    });

    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 100, "all 100 parallel forwards succeeded");
    handle.stop();
}

#[test]
fn chunked_response_is_reframed() {
    // Upstream speaks raw chunked framing; the proxy must re-encode valid
    // chunked framing so the client does not hang.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let up_port = listener.local_addr().unwrap().port();
    let join = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            // Consume request head.
            let mut acc = Vec::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                acc.extend_from_slice(&buf[..n]);
                if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
            )
            .await
            .unwrap();
        });
    });
    let handle = spawn_proxy(Arc::new(ferrite_proxy::AllowAllHook));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let body = rt.block_on(async {
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", handle.port))
            .await
            .unwrap();
        let req = format!(
            "GET http://127.0.0.1:{up_port}/chunked HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        );
        sock.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf))
                .await
                .expect("chunked response must terminate")
                .unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if out.windows(5).any(|w| w == b"world") && out.ends_with(b"0\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(out).unwrap()
    });
    assert!(body.contains("Transfer-Encoding: chunked"), "got: {body}");
    assert!(body.contains("hello"), "chunk payload relayed");
    assert!(body.contains("world"), "second chunk relayed");
    assert!(
        body.ends_with("0\r\n\r\n"),
        "valid terminating chunk: {body}"
    );
    let _ = join.join();
    handle.stop();
}

#[test]
fn chunked_upload_is_forwarded() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let up_port = listener.local_addr().unwrap().port();
    let join = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut acc = Vec::new();
            let mut buf = [0u8; 4096];
            // Read head + chunked body until terminating chunk.
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                acc.extend_from_slice(&buf[..n]);
                if acc.windows(5).any(|w| w == b"0\r\n\r\n") {
                    break;
                }
            }
            let req = String::from_utf8_lossy(&acc).into_owned();
            assert!(req.contains("hello"), "upstream saw chunk payload: {req}");
            let body = b"upload-ok";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\nupload-ok",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });
    });
    let handle = spawn_proxy(Arc::new(ferrite_proxy::AllowAllHook));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let resp = rt.block_on(send_recv(
        handle.port,
        &format!(
            "POST http://127.0.0.1:{up_port}/submit HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n"
        ),
    ));
    assert!(resp.contains("upload-ok"), "chunked upload proxied: {resp}");
    let _ = join.join();
    handle.stop();
}

#[test]
fn internal_host_requires_exact_match() {
    let handle = spawn_proxy(Arc::new(ferrite_proxy::AllowAllHook));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // Exact internal host serves the local page.
    let internal = rt.block_on(send_recv(
        handle.port,
        "GET http://ferrite/newtab HTTP/1.1\r\nHost: ferrite\r\nConnection: close\r\n\r\n",
    ));
    assert!(
        internal.starts_with("HTTP/1.1 200"),
        "internal served: {internal}"
    );
    // External host merely containing the substring must NOT be hijacked.
    // `.invalid` never resolves, so the proxy must NOT serve internal
    // content, a document-shaped failure 302s to the error page instead.
    let external = rt.block_on(send_recv(
        handle.port,
        "GET http://myferrite.invalid/ HTTP/1.1\r\nHost: myferrite.invalid\r\nConnection: close\r\n\r\n",
    ));
    assert!(
        external.starts_with("HTTP/1.1 302") && external.contains("ferrite://error?"),
        "substring host not hijacked, got: {external}"
    );
    assert!(!external.contains("ferrite://newtab"));
    handle.stop();
}

#[test]
fn archive_fallback_serves_snapshot_on_dial_failure() {
    // Canned provider: stands in for the shell's Archive-backed closure.
    let provider: Arc<ferrite_proxy::ArchiveProvider> = Arc::new(|_url: &str| {
        Some(ferrite_proxy::ArchiveSnapshot {
            html: b"<html><body>archived copy</body></html>".to_vec(),
            headers: "text/html".to_owned(),
            timestamp: 1700000000,
        })
    });
    let handle = ferrite_proxy::spawn(
        ProxyConfig {
            fixed_port: None,
            archive: Some(provider),
            dns: ferrite_proxy::DnsConfig::system(),
        },
        Arc::new(ferrite_proxy::AllowAllHook),
    )
    .expect("proxy spawn");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // Port 1 is closed: dial fails, so the archive snapshot must serve.
    let resp = rt.block_on(send_recv(
        handle.port,
        "GET http://127.0.0.1:1/dead HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    ));
    assert!(resp.starts_with("HTTP/1.1 200"), "snapshot served: {resp}");
    assert!(
        resp.contains("Displaying local snapshot"),
        "archive banner: {resp}"
    );
    assert!(resp.contains("archived copy"), "snapshot body: {resp}");
    handle.stop();
}

#[test]
fn archive_none_still_502s_on_dial_failure() {
    // Without a provider, non-documents keep the empty 502 (only
    // document-shaped failures 302 to the error page).
    let handle = spawn_proxy(Arc::new(ferrite_proxy::AllowAllHook));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let resp = rt.block_on(send_recv(
        handle.port,
        "GET http://127.0.0.1:1/dead HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    ));
    assert!(resp.starts_with("HTTP/1.1 502"), "no fallback: {resp}");
    handle.stop();
}

async fn futures_join<T: Send + 'static>(
    handles: Vec<tokio::task::JoinHandle<T>>,
) -> Vec<std::result::Result<T, ()>> {
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.map_err(|_| ()));
    }
    out
}

/// Read until EOF (server closes on `Connection: close`). Unlike `send_recv`,
/// this tolerates bare closes (empty) without hanging, so regression tests
/// can assert a proper status was sent instead of zero bytes.
async fn send_recv_eof(port: u16, request: &[u8]) -> Vec<u8> {
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to proxy");
    sock.write_all(request).await.unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => out.extend_from_slice(&buf[..n]),
            // RST / reset after a status still counts: return what we have.
            Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

#[test]
fn blocked_document_redirects_to_blocked_page() {
    let handle = spawn_proxy(Arc::new(BlockListHook::new(&["ads.example"])));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // Top-level document navigation must 302 to ferrite://blocked (same shape
    // as malware, no proceed mechanism), never a bare 204.
    let doc = rt.block_on(send_recv_eof(
        handle.port,
        b"GET http://ads.example/ HTTP/1.1\r\nHost: ads.example\r\nSec-Fetch-Dest: document\r\nConnection: close\r\n\r\n",
    ));
    let doc_s = String::from_utf8_lossy(&doc).into_owned();
    assert!(
        doc_s.starts_with("HTTP/1.1 302"),
        "blocked doc must 302, got: {doc_s}"
    );
    assert!(
        doc_s.contains("ferrite://blocked?host=ads.example"),
        "blocked loc shape, got: {doc_s}"
    );
    assert!(!doc_s.contains("proceed"), "no proceed mechanism: {doc_s}");
    // Subresource stays a harmless 204.
    let sub = rt.block_on(send_recv_eof(
        handle.port,
        b"GET http://ads.example/pixel.gif HTTP/1.1\r\nHost: ads.example\r\nSec-Fetch-Dest: image\r\nConnection: close\r\n\r\n",
    ));
    let sub_s = String::from_utf8_lossy(&sub).into_owned();
    assert!(
        sub_s.starts_with("HTTP/1.1 204"),
        "blocked subresource stays 204, got: {sub_s}"
    );
    handle.stop();
}

#[test]
fn document_mid_body_failure_yields_error_status() {
    // Upstream reads the head then drops without a response. The client
    // sends the head first, waits for the RST to land, then sends a small
    // body the proxy fully consumes before its upstream write fails. The
    // proxy must send a status (Document -> 302 to the error page), never a
    // bare close.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("upstream bind");
    let up_port = listener.local_addr().unwrap().port();
    let join = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut acc = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                acc.extend_from_slice(&buf[..n]);
                if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            // Drop without responding: the proxy's body write must fail.
        });
    });
    let handle = spawn_proxy(Arc::new(ferrite_proxy::AllowAllHook));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let resp = rt.block_on(async {
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", handle.port))
            .await
            .unwrap();
        let body_len = 4096;
        let head = format!(
            "POST http://127.0.0.1:{up_port}/ HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Dest: document\r\nContent-Length: {body_len}\r\nConnection: close\r\n\r\n"
        );
        sock.write_all(head.as_bytes()).await.unwrap();
        // Let the upstream RST land before the body is forwarded.
        tokio::time::sleep(Duration::from_millis(300)).await;
        sock.write_all(&vec![b'x'; body_len]).await.unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => out.extend_from_slice(&buf[..n]),
                Ok(Err(_)) | Err(_) => break,
            }
        }
        out
    });
    let resp_s = String::from_utf8_lossy(&resp).into_owned();
    assert!(
        !resp_s.is_empty() && resp_s.starts_with("HTTP/1.1 "),
        "mid-body failure must send a status, not bare close, got {} bytes",
        resp_s.len()
    );
    assert!(
        resp_s.starts_with("HTTP/1.1 302") && resp_s.contains("ferrite://error?"),
        "document mid-body failure 302s to error page, got: {}",
        &resp_s[..resp_s.len().min(300)]
    );
    let _ = join.join();
    handle.stop();
}

#[test]
fn downgrade_counter_and_failure_telemetry() {
    // Unresolvable but upgradable host: probe fails -> plaintext fallback
    // (downgrade counts) -> dial fails -> Allowed telemetry with no failure
    // kind plus a document 302 to the error page.
    let handle = spawn_proxy(Arc::new(ferrite_proxy::AllowAllHook));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let resp = rt.block_on(send_recv_eof(
        handle.port,
        b"GET http://downgrade-probe.test/ HTTP/1.1\r\nHost: downgrade-probe.test\r\nSec-Fetch-Dest: document\r\nConnection: close\r\n\r\n",
    ));
    let resp_s = String::from_utf8_lossy(&resp).into_owned();
    assert!(
        resp_s.starts_with("HTTP/1.1 302") && resp_s.contains("ferrite://error?"),
        "dial failure doc 302s, got: {resp_s}"
    );
    assert_eq!(
        handle.downgraded.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "one plaintext fallback after should_upgrade"
    );
    let ev = handle
        .telemetry
        .recv_timeout(Duration::from_secs(5))
        .expect("failure telemetry emitted");
    assert_eq!(ev.request_domain, "downgrade-probe.test");
    assert_eq!(ev.classification, ferrite_proxy::Classification::Allowed);
    assert_eq!(ev.bytes_transferred, 0);
    assert_eq!(ev.failure, None);
    handle.stop();
}

// Silence unused import if TelemetryTx only used in type position above.
#[allow(dead_code)]
fn _keep_import(_t: &TelemetryTx) {}
