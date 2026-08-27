//! Typed errors for the network interception engine.

use thiserror::Error;

/// All fallible outcomes from the proxy layer.
pub type ProxyResult<T> = Result<T, ProxyError>;

#[derive(Debug, Error)]
pub enum ProxyError {
    /// The listener could not bind its local address.
    #[error("failed to bind proxy listener on {addr}: {source}")]
    Bind {
        addr: std::net::SocketAddr,
        #[source]
        source: std::io::Error,
    },

    /// The internal tokio runtime failed to start.
    #[error("failed to start proxy async runtime: {0}")]
    Runtime(#[source] std::io::Error),

    /// Upstream connection failed or timed out (surfaced inside tunnel/forward
    /// paths as HTTP 502 to the client; never panics the accept loop).
    #[error("upstream connect to {host}:{port} failed: {source}")]
    UpstreamConnect {
        host: String,
        port: u16,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_error_includes_addr() {
        let err = ProxyError::Bind {
            addr: "127.0.0.1:1".parse().unwrap(),
            source: std::io::Error::new(std::io::ErrorKind::AddrInUse, "busy"),
        };
        assert!(err.to_string().contains("127.0.0.1:1"));
    }

    #[test]
    fn upstream_error_names_host() {
        let err = ProxyError::UpstreamConnect {
            host: "example.test".to_owned(),
            port: 443,
            source: std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"),
        };
        let s = err.to_string();
        assert!(s.contains("example.test") && s.contains("443"));
    }
}
