//! Typed errors for the network interception engine.

use thiserror::Error;

/// Proxy layer result.
pub type ProxyResult<T> = Result<T, ProxyError>;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("failed to bind proxy listener on {addr}: {source}")]
    Bind {
        addr: std::net::SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to start proxy async runtime: {0}")]
    Runtime(#[source] std::io::Error),
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
}
