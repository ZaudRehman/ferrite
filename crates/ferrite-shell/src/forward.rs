//! Second-instance URL forwarding over loopback.
//!
//! The live instance holds the single-instance file lock and serves one
//! line-based protocol on `127.0.0.1` (ephemeral port): a second launch that
//! fails the lock reads the endpoint file, proves knowledge of the per-boot
//! token, and hands over its URLs instead of dying with "already running".
//! Incognito and normal modes use separate endpoint files and never forward
//! across the boundary. Stale endpoint files (crash) simply refuse to
//! connect, and the second instance falls back to the refuse dialog.

use std::io::{BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One request line past this is dropped (URLs themselves cap at 2048).
pub const MAX_LINE_BYTES: usize = 4096;
/// Queued handovers per process; beyond this the server answers `err` so a
/// chatty peer cannot grow memory without bound.
pub const MAX_QUEUED: usize = 8;
/// Second-instance connect + round-trip budget (loopback, no reason to wait).
const FORWARD_TIMEOUT: Duration = Duration::from_millis(500);
/// Per-connection read budget on the server side.
const SERVER_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Inbox for handed-over URLs, drained by the shell tick.
pub type ForwardQueue = Arc<Mutex<Vec<String>>>;

pub fn endpoint_path(incognito: bool, profile_id: &str) -> PathBuf {
    // Incognito is one global window (ephemeral, no stores to clash), and
    // Default keeps legacy names. Named profiles scope their own endpoint
    // so a second launch hands URLs to the same profile, never a stranger.
    if incognito || profile_id.eq_ignore_ascii_case(crate::profiles::DEFAULT_ID) {
        let name = if incognito {
            "ferrite-forward-incognito.json"
        } else {
            "ferrite-forward.json"
        };
        crate::profiles::data_root().join(name)
    } else {
        crate::profiles::data_root().join(format!(
            "ferrite-forward-{}.json",
            crate::profiles::slug(profile_id)
        ))
    }
}

/// URLs eligible for handover: web pages and internal pages only, bounded,
/// single-line. Everything else stays a local refusal (no `javascript:`,
/// no `file:`, no control characters that could smuggle a second line).
pub fn is_forwardable_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || url.len() > 2048 || url.chars().any(|c| c.is_control()) {
        return false;
    }
    let lower = url.to_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("ferrite://")
}

/// Wire format: `v1 <token> open <url>\n`. Tokens are 64 lowercase hex chars.
pub fn encode_request(token: &str, url: &str) -> String {
    format!("v1 {token} open {url}\n")
}

/// Parse + validate one request line. Returns the URL when the shape, token
/// format, verb, and URL all check out (token *equality* is the caller's job:
/// it compares against the live secret, never echoed here).
pub fn parse_request(line: &str) -> Option<String> {
    let line = line
        .strip_suffix('\n')
        .unwrap_or(line)
        .trim_end_matches('\r');
    let (head, url) = line.split_once(" open ")?;
    let mut parts = head.splitn(2, ' ');
    if parts.next()? != "v1" {
        return None;
    }
    let token = parts.next()?;
    if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if !is_forwardable_url(url) {
        return None;
    }
    Some(url.trim().to_owned())
}

/// Constant-time token comparison (length + content, no early exit).
/// Empty secrets never compare equal.
pub fn token_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Fresh per-boot endpoint secret (32 CSPRNG bytes, hex). Held in memory by
/// the server and mirrored into the endpoint file for second instances.
pub fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Serve handovers for this process: bind loopback, publish the endpoint
/// file, and push accepted URLs into `queue` (waking the loop per accept).
/// Best-effort: any failure disables forwarding and leaves the browser
/// fully usable (second instances get the refuse dialog instead).
pub fn start_forward_server(
    wake: winit::event_loop::EventLoopProxy<super::shell::ShellWake>,
    queue: &ForwardQueue,
    incognito: bool,
    profile_id: &str,
) {
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            tracing::debug!(error = %e, "forward server bind failed: no URL handover this session");
            return;
        }
    };
    let port = match listener.local_addr().map(|a| a.port()) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "forward server port unreadable");
            return;
        }
    };
    let token = generate_token();
    let doc = serde_json::json!({ "port": port, "token": token });
    let path = endpoint_path(incognito, profile_id);
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    if std::fs::write(&path, serde_json::to_string(&doc).unwrap_or_default()).is_err() {
        tracing::debug!("forward endpoint unwritable: no URL handover this session");
        return;
    }
    let queue = Arc::clone(queue);
    if std::thread::Builder::new()
        .name("ferrite-forward".into())
        .spawn(move || serve(listener, token, queue, wake))
        .is_err()
    {
        tracing::debug!("forward server spawn failed");
    }
}

fn serve(
    listener: std::net::TcpListener,
    token: String,
    queue: ForwardQueue,
    wake: winit::event_loop::EventLoopProxy<super::shell::ShellWake>,
) {
    for stream in listener.incoming().filter_map(|s| s.ok()) {
        let _ = stream.set_read_timeout(Some(SERVER_READ_TIMEOUT));
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        // Shape first (cheap), secret second: malformed lines never reach
        // the comparison, and the comparison never echoes which half failed.
        let accepted = read_line_capped(&mut reader, &mut line)
            .ok()
            .and_then(|()| parse_request(&line))
            .filter(|_| {
                let presented = line.split_whitespace().nth(1).unwrap_or_default();
                token_eq(presented, &token)
            });
        let reply = match accepted {
            Some(url) => {
                let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                if q.len() >= MAX_QUEUED {
                    &b"err\n"[..]
                } else {
                    q.push(url);
                    let _ = wake.send_event(super::shell::ShellWake::Forward);
                    &b"ok\n"[..]
                }
            }
            None => &b"err\n"[..],
        };
        let _ = (&stream).write_all(reply);
    }
}

fn read_line_capped(
    reader: &mut BufReader<&std::net::TcpStream>,
    out: &mut String,
) -> std::io::Result<()> {
    let mut total = 0usize;
    loop {
        let mut byte = [0u8; 1];
        match reader.read_exact(&mut byte) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        total += 1;
        if total > MAX_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "forward line exceeds cap",
            ));
        }
        out.push(byte[0] as char);
        if byte[0] == b'\n' {
            break;
        }
    }
    Ok(())
}

/// Second-instance path: hand `urls` to the live window. Returns true only
/// when every URL was accepted (`ok` per line). Any failure means "fall back
/// to the refuse dialog": stale endpoint file, connect timeout, bad reply.
pub fn try_forward_to_live(incognito: bool, profile_id: &str, urls: &[String]) -> bool {
    if urls.is_empty() {
        return false;
    }
    let path = endpoint_path(incognito, profile_id);
    let doc: serde_json::Value = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
    {
        Some(v) => v,
        None => return false,
    };
    let port = doc.get("port").and_then(|p| p.as_u64()).unwrap_or(0) as u16;
    let token = doc.get("token").and_then(|t| t.as_str()).unwrap_or("");
    if port == 0 || token.len() != 64 {
        return false;
    }
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let stream = match std::net::TcpStream::connect_timeout(&addr, FORWARD_TIMEOUT) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let _ = stream.set_read_timeout(Some(FORWARD_TIMEOUT));
    let _ = stream.set_write_timeout(Some(FORWARD_TIMEOUT));
    let mut reader = BufReader::new(&stream);
    for url in urls {
        if !is_forwardable_url(url) {
            return false;
        }
        if (&stream)
            .write_all(encode_request(token, url.trim()).as_bytes())
            .is_err()
        {
            return false;
        }
        let mut reply = String::new();
        if read_line_capped(&mut reader, &mut reply).is_err() || reply.trim() != "ok" {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn round_trip_accepts_well_formed() {
        let line = encode_request(TOKEN, "https://example.com/a?b=1");
        assert_eq!(
            parse_request(&line).as_deref(),
            Some("https://example.com/a?b=1")
        );
    }

    #[test]
    fn malformed_lines_rejected() {
        assert!(parse_request("v1 short open https://example.com\n").is_none());
        assert!(parse_request("v2 TOKEN open https://example.com\n").is_none());
        assert!(parse_request(&format!("v1 {TOKEN} run https://example.com\n")).is_none());
        assert!(parse_request(&format!("v1 {TOKEN} open javascript:alert(1)\n")).is_none());
        assert!(parse_request(&format!("v1 {TOKEN} open file:///etc/passwd\n")).is_none());
        assert!(parse_request(&format!("v1 {TOKEN} open https://ok.test\ninject\n")).is_none());
        assert!(parse_request("garbage\n").is_none());
        assert!(parse_request("").is_none());
    }

    #[test]
    fn forwardable_gate() {
        assert!(is_forwardable_url("https://example.com"));
        assert!(is_forwardable_url("http://a.test/x"));
        assert!(is_forwardable_url("ferrite://newtab"));
        assert!(!is_forwardable_url("javascript:alert(1)"));
        assert!(!is_forwardable_url("data:text/html,x"));
        assert!(!is_forwardable_url("file:///x"));
        assert!(!is_forwardable_url(""));
        assert!(!is_forwardable_url(
            &("https://x.test/".to_owned() + &"y".repeat(3000))
        ));
        assert!(!is_forwardable_url("https://x.test/a\nb"));
    }

    #[test]
    fn token_compare_is_exact() {
        assert!(token_eq(TOKEN, TOKEN));
        assert!(!token_eq(TOKEN, &TOKEN.to_uppercase()));
        assert!(!token_eq(TOKEN, "short"));
        assert!(!token_eq("", ""));
    }

    #[test]
    fn generate_token_shape() {
        let t = generate_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, generate_token(), "CSPRNG must not repeat");
    }

    #[test]
    fn endpoint_scopes_named_profiles() {
        // Default + incognito keep legacy bare names (compat); named
        // profiles scope their own endpoint so handovers never cross.
        let def = endpoint_path(false, "Default")
            .file_name()
            .unwrap()
            .to_owned();
        assert_eq!(def, "ferrite-forward.json");
        let inc = endpoint_path(true, "Work").file_name().unwrap().to_owned();
        assert_eq!(inc, "ferrite-forward-incognito.json");
        let scoped = endpoint_path(false, "Work").file_name().unwrap().to_owned();
        assert_eq!(scoped, "ferrite-forward-Work.json");
        let spaced = endpoint_path(false, "My Work")
            .file_name()
            .unwrap()
            .to_owned();
        assert_eq!(spaced, "ferrite-forward-My_Work.json");
    }
}
