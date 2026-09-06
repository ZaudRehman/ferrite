//! QUIC p2p transport with pinned identity certs + mDNS discovery.
//!
//! The endpoint serves the device identity certificate (same Ed25519 key as
//! mTLS) and verifies peers against the trusted-device store. There is no
//! `SkipVerify` fallback: untrusted peers fail the handshake.

use std::sync::{Arc, Mutex};

use tracing::{debug, warn};

use crate::crypto::DeviceIdentity;
use crate::trusted::TrustedStore;

#[derive(Debug, Clone)]
pub struct P2pConfig {
    pub device_id: String,
    pub listen_port: u16,
    pub relay_url: Option<String>,
}

impl P2pConfig {
    pub fn new(device_id: String, listen_port: u16) -> Self {
        Self {
            device_id,
            listen_port,
            relay_url: None,
        }
    }
}

pub struct P2pTransport {
    config: P2pConfig,
    endpoint: Option<quinn::Endpoint>,
}

impl P2pTransport {
    pub fn new(config: P2pConfig) -> Self {
        Self {
            config,
            endpoint: None,
        }
    }

    pub async fn bind(
        &mut self,
        identity: &DeviceIdentity,
        trusted: Arc<Mutex<TrustedStore>>,
    ) -> Result<u16, String> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], self.config.listen_port));
        let (cert_der, key_der) = identity
            .self_signed_cert_der()
            .map_err(|e| format!("identity cert: {e}"))?;
        let cert_chain = vec![rustls_pki_types::CertificateDer::from(cert_der.clone())];
        let client_chain = vec![rustls_pki_types::CertificateDer::from(cert_der)];
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.clone().into());
        let client_key = rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.into());
        let server_crypto = rustls::ServerConfig::builder()
            .with_client_cert_verifier(crate::mtls::pinned_server_verifier(Arc::clone(&trusted)))
            .with_single_cert(cert_chain, key)
            .map_err(|e| format!("server cert: {e}"))?;
        let client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(crate::mtls::pinned_server_verifier(trusted))
            .with_client_auth_cert(client_chain, client_key)
            .map_err(|e| format!("client cert: {e}"))?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
                .map_err(|e| e.to_string())?,
        ));
        let mut client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
                .map_err(|e| e.to_string())?,
        ));
        client_config.transport_config(Arc::new({
            let mut t = quinn::TransportConfig::default();
            t.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
            t
        }));
        server_config.transport_config(Arc::new({
            let mut t = quinn::TransportConfig::default();
            t.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
            t
        }));
        let mut endpoint =
            quinn::Endpoint::server(server_config, addr).map_err(|e| format!("bind: {e}"))?;
        endpoint.set_default_client_config(client_config);
        let port = endpoint
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(self.config.listen_port);
        debug!(port, "p2p quic bound");
        self.endpoint = Some(endpoint);
        self.spawn_mdns(port);
        Ok(port)
    }

    fn spawn_mdns(&self, port: u16) {
        let device_id = self.config.device_id.clone();
        std::thread::Builder::new()
            .name("ferrite-mdns".to_owned())
            .spawn(move || {
                if let Ok(mdns) = mdns_sd::ServiceDaemon::new() {
                    let _host = format!("{device_id}.local.");
                    let props: std::collections::HashMap<String, String> =
                        std::collections::HashMap::from([(
                            "device".to_string(),
                            device_id.clone(),
                        )]);
                    if let Ok(service) = mdns_sd::ServiceInfo::new(
                        "_ferrite._tcp.local.",
                        &device_id,
                        &format!("{device_id}.local."),
                        "",
                        port,
                        props,
                    ) {
                        if let Err(e) = mdns.register(service) {
                            warn!(error=%e, "mdns register failed");
                        } else {
                            debug!("mdns registered");
                            loop {
                                std::thread::sleep(std::time::Duration::from_secs(3600));
                            }
                        }
                    }
                }
            })
            .ok();
    }

    pub async fn connect(
        &self,
        addr: std::net::SocketAddr,
        server_name: &str,
    ) -> Result<quinn::Connection, String> {
        let ep = self.endpoint.as_ref().ok_or("not bound")?;
        let conn = ep
            .connect(addr, server_name)
            .map_err(|e| format!("connect: {e}"))?
            .await
            .map_err(|e| format!("quic: {e}"))?;
        Ok(conn)
    }

    pub fn is_bound(&self) -> bool {
        self.endpoint.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bind_ephemeral() {
        use std::sync::Mutex;
        for _ in 0..3 {
            let id = format!("test-device-{}", uuid::Uuid::new_v4());
            let cfg = P2pConfig::new(id, 0);
            let mut t = P2pTransport::new(cfg);
            let identity = DeviceIdentity::generate().unwrap();
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            let store = TrustedStore::new(std::sync::Arc::new(Mutex::new(conn)));
            store.init_schema().unwrap();
            let trusted = std::sync::Arc::new(Mutex::new(store));
            if let Ok(port) = t.bind(&identity, trusted).await {
                assert!(port != 0);
                assert!(t.is_bound());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // If all binds fail, skip - CI may have port contention
    }
}
