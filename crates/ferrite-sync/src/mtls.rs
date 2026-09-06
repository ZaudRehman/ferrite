//! Part 10 mTLS TCP via rustls + custom ServerCertVerifier pinned to trusted_devices (5.2).

use std::sync::{Arc, Mutex};

use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector, rustls};
use tracing::debug;

use crate::crypto::DeviceIdentity;
use crate::trusted::TrustedStore;

#[derive(Debug, Clone)]
pub struct MtlsConfig {
    pub device_id: String,
    pub port: u16,
}

pub struct MtlsTransport {
    config: MtlsConfig,
    acceptor: Option<TlsAcceptor>,
    connector: TlsConnector,
    trusted: Arc<Mutex<TrustedStore>>,
}

impl MtlsTransport {
    /// Build mTLS transport bound to `identity`: the served certificate's
    /// private key IS the device identity key, so the TLS handshake
    /// signature provably binds the peer to the pinned Ed25519 key.
    pub fn new(
        config: MtlsConfig,
        trusted: Arc<Mutex<TrustedStore>>,
        identity: &DeviceIdentity,
    ) -> Result<Self, String> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (cert_der, key_der) = identity
            .self_signed_cert_der()
            .map_err(|e| format!("identity cert: {e}"))?;
        let cert_chain = vec![CertificateDer::from(cert_der.clone())];
        let client_cert_chain = vec![CertificateDer::from(cert_der)];
        let key = PrivateKeyDer::Pkcs8(key_der.clone().into());
        let client_key = PrivateKeyDer::Pkcs8(key_der.into());

        // Server verifier pinned to trusted_devices
        let server_verifier = Arc::new(PinnedVerifier {
            trusted: Arc::clone(&trusted),
        });
        let mut server_crypto = rustls::ServerConfig::builder()
            .with_client_cert_verifier(server_verifier)
            .with_single_cert(cert_chain, key)
            .map_err(|e| format!("server cert: {e}"))?;
        server_crypto.alpn_protocols = vec![b"ferrite-sync/1".to_vec()];

        let client_verifier = Arc::new(PinnedVerifier {
            trusted: Arc::clone(&trusted),
        });
        let client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(client_verifier)
            .with_client_auth_cert(client_cert_chain, client_key)
            .map_err(|e| format!("client cert: {e}"))?;

        let acceptor = TlsAcceptor::from(Arc::new(server_crypto));
        let connector = TlsConnector::from(Arc::new(client_crypto));

        Ok(Self {
            config,
            acceptor: Some(acceptor),
            connector,
            trusted,
        })
    }

    pub async fn listen(&self) -> Result<TcpListener, String> {
        let addr = format!("0.0.0.0:{}", self.config.port);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;
        let port = listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(self.config.port);
        debug!(%port, "mTLS TCP listener bound");
        Ok(listener)
    }

    pub async fn accept(
        &self,
        listener: &TcpListener,
    ) -> Result<
        (
            tokio_rustls::server::TlsStream<TcpStream>,
            std::net::SocketAddr,
        ),
        String,
    > {
        let acceptor = self.acceptor.clone().ok_or("no acceptor")?;
        let (stream, addr) = listener
            .accept()
            .await
            .map_err(|e| format!("accept: {e}"))?;
        stream.set_nodelay(true).ok();
        let tls = acceptor
            .accept(stream)
            .await
            .map_err(|e| format!("tls accept: {e}"))?;
        debug!(%addr, "mTLS accepted");
        Ok((tls, addr))
    }

    /// Connect with SNI set to the expected peer `device_id`. The verifier
    /// requires the presented certificate to carry the same device_id in
    /// CN/SAN *and* a pinned, non-revoked public key.
    pub async fn connect(
        &self,
        addr: std::net::SocketAddr,
        server_name: &str,
    ) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("tcp connect: {e}"))?;
        stream.set_nodelay(true).ok();
        let name = ServerName::try_from(server_name.to_owned()).map_err(|e| format!("sni: {e}"))?;
        let tls = self
            .connector
            .connect(name, stream)
            .await
            .map_err(|e| format!("tls connect: {e}"))?;
        debug!(%addr, "mTLS connected");
        Ok(tls)
    }

    pub fn is_trusted(&self, device_id: &str, public_key: &[u8]) -> bool {
        if let Ok(store) = self.trusted.lock() {
            store.is_trusted(device_id, public_key)
        } else {
            false
        }
    }
}

#[derive(Debug)]
pub(crate) struct PinnedVerifier {
    trusted: Arc<Mutex<TrustedStore>>,
}

/// Free constructor so QUIC transport (same crate) shares the verifier.
pub(crate) fn pinned_server_verifier(trusted: Arc<Mutex<TrustedStore>>) -> Arc<PinnedVerifier> {
    Arc::new(PinnedVerifier { trusted })
}

impl PinnedVerifier {
    /// Fail-closed peer check: extract (pubkey, device_id) from the leaf
    /// cert, require a matching non-revoked trusted entry, and — when an
    /// expected name is supplied (client side SNI) — require the cert's
    /// device identity to equal it. Empty trust stores REJECT.
    fn check_peer(
        &self,
        end: &CertificateDer<'_>,
        expected_name: Option<&str>,
    ) -> Result<(), rustls::Error> {
        let (peer_key, peer_device) = extract_identity(end.as_ref()).ok_or_else(|| {
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)
        })?;
        let trusted = self
            .trusted
            .lock()
            .map_err(|_| rustls::Error::General("lock".into()))?;
        let matched = trusted
            .all()
            .iter()
            .any(|d| !d.is_revoked && d.public_key == peer_key && d.device_id == peer_device);
        if !matched {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ));
        }
        if let Some(name) = expected_name {
            // SNI must equal the cert's device identity (case-insensitive).
            // `connect()` callers pass the expected peer device_id.
            let want = name.trim().trim_end_matches('.').to_ascii_lowercase();
            let got = peer_device
                .trim()
                .trim_end_matches('.')
                .to_ascii_lowercase();
            // Bare UUIDs compare directly; DNS SNI may carry `.local.` suffix.
            let got_bare = got.trim_end_matches(".local").trim_end_matches('.');
            if want != got && want != got_bare && !want.starts_with(&format!("{got}.")) {
                return Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::NotValidForName,
                ));
            }
        }
        Ok(())
    }
}

/// Verify a TLS handshake signature against the Ed25519 key in `cert`.
/// rustls passes the fully-constructed verify message as `msg`, so plain
/// ed25519 verification over `(msg, signature)` with the cert's SPKI key is
/// complete: presenting a trusted certificate WITHOUT its private key fails.
/// Only the ED25519 scheme is accepted (our certs are Ed25519-only).
fn verify_handshake_ed25519(
    msg: &[u8],
    cert: &CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    if dss.scheme != rustls::SignatureScheme::ED25519 {
        return Err(rustls::Error::PeerMisbehaved(
            rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme,
        ));
    }
    let spki = spki_bytes(cert.as_ref())
        .ok_or_else(|| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    let vk_bytes: &[u8; 32] = spki
        .as_slice()
        .try_into()
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    let vk = VerifyingKey::from_bytes(vk_bytes)
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    let sig_bytes: &[u8; 64] = dss
        .signature()
        .try_into()
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    vk.verify(msg, &Signature::from_bytes(sig_bytes))
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadSignature))?;
    Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
}

/// Raw Ed25519 (32B) subject public key from a DER certificate.
fn spki_bytes(cert_der: &[u8]) -> Option<Vec<u8>> {
    use x509_parser::prelude::*;
    let (_, cert) = X509Certificate::from_der(cert_der).ok()?;
    let spki = &cert.tbs_certificate.subject_pki;
    // Ed25519 SPKI payload is the 32B key (BIT STRING unwrapped by x509-parser).
    if spki.subject_public_key.data.len() == 32 {
        Some(spki.subject_public_key.data.to_vec())
    } else {
        None
    }
}

impl rustls::client::danger::ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        _inter: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let sni = match name {
            ServerName::DnsName(n) => n.as_ref().to_owned(),
            _ => {
                return Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::NotValidForName,
                ));
            }
        };
        self.check_peer(end, Some(&sni))?;
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // Real verification: presenting a trusted cert WITHOUT its private
        // key must fail. (The old code returned Ok unconditionally.)
        verify_handshake_ed25519(msg, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        verify_handshake_ed25519(msg, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        // Pinned Ed25519 identity only.
        vec![rustls::SignatureScheme::ED25519]
    }
}

impl rustls::server::danger::ClientCertVerifier for PinnedVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        end: &CertificateDer<'_>,
        _inter: &[CertificateDer<'_>],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        // No SNI on the server side: bind to the trusted entry's device_id.
        self.check_peer(end, None)?;
        Ok(rustls::server::danger::ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        verify_handshake_ed25519(msg, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        verify_handshake_ed25519(msg, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![rustls::SignatureScheme::ED25519]
    }
}

/// Extract (Ed25519 pubkey, device_id) bound in the leaf certificate:
/// pubkey from custom extension OID 1.3.6.1.4.1.54392.1, device_id from
/// CN (fallback: first SAN dNSName).
fn extract_identity(cert_der: &[u8]) -> Option<(Vec<u8>, String)> {
    use x509_parser::prelude::*;
    let (_, cert) = X509Certificate::from_der(cert_der).ok()?;
    let mut key: Option<Vec<u8>> = None;
    for ext in cert.extensions() {
        if ext.oid.to_string() == "1.3.6.1.4.1.54392.1" {
            // rcgen encodes content as OCTET STRING; handle both raw 32B and 0x04 0x20 header
            let v = ext.value;
            key = Some(if v.len() == 32 {
                v.to_vec()
            } else if v.len() >= 34 && v[0] == 0x04 && v[1] == 0x20 {
                v[2..34].to_vec()
            } else {
                v.to_vec()
            });
        }
    }
    let key = key?;
    if key.len() != 32 {
        return None;
    }
    let device = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(|s| s.to_owned())
        .or_else(|| {
            cert.subject_alternative_name()
                .ok()
                .flatten()
                .and_then(|san| {
                    san.value.general_names.iter().find_map(|n| match n {
                        GeneralName::DNSName(d) => Some(d.to_string()),
                        _ => None,
                    })
                })
        })?;
    Some((key, device))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    async fn bind_pair() -> (
        MtlsTransport,
        tokio::net::TcpListener,
        MtlsTransport,
        String,
    ) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let conn = Connection::open_in_memory().unwrap();
        let store = crate::trusted::TrustedStore::new(Arc::new(Mutex::new(conn)));
        store.init_schema().unwrap();
        let trusted = Arc::new(Mutex::new(store));
        let server_id = crate::crypto::DeviceIdentity::generate().unwrap();
        let client_id = crate::crypto::DeviceIdentity::generate().unwrap();
        // Mutual pairing: each side trusts the other's (device_id, pubkey).
        for (id, name) in [(&server_id, "server"), (&client_id, "client")] {
            trusted
                .lock()
                .unwrap()
                .add_trusted(&crate::trusted::TrustedDevice {
                    device_id: id.device_id.to_string(),
                    device_name: name.to_owned(),
                    public_key: id.public_key_bytes(),
                    paired_via_device_id: None,
                    first_paired_at: 0,
                    last_seen_at: None,
                    last_synced_at: None,
                    hlc_timestamp: 0,
                    is_revoked: false,
                })
                .unwrap();
        }
        let server_name = server_id.device_id.to_string();
        let client_name = client_id.device_id.to_string();
        let server = MtlsTransport::new(
            MtlsConfig {
                device_id: server_name.clone(),
                port: 0,
            },
            Arc::clone(&trusted),
            &server_id,
        )
        .unwrap();
        let listener = server.listen().await.unwrap();
        let client = MtlsTransport::new(
            MtlsConfig {
                device_id: client_name,
                port: 0,
            },
            trusted,
            &client_id,
        )
        .unwrap();
        (server, listener, client, server_name)
    }

    #[tokio::test]
    async fn mtls_paired_devices_connect() {
        let (server, listener, client, server_name) = bind_pair().await;
        let port = listener.local_addr().unwrap().port();
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let accept_fut = server.accept(&listener);
        // SNI carries the expected peer device_id.
        let connect_fut = client.connect(addr, &server_name);
        let (accept_res, connect_res) = tokio::join!(accept_fut, connect_fut);
        assert!(accept_res.is_ok(), "accept failed: {:?}", accept_res.err());
        assert!(
            connect_res.is_ok(),
            "connect failed: {:?}",
            connect_res.err()
        );
    }

    #[tokio::test]
    async fn mtls_rejects_unknown() {
        // Attacker identity never paired: both directions must fail.
        let (server, listener, _client, server_name) = bind_pair().await;
        let port = listener.local_addr().unwrap().port();
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));

        // Attacker client with its own keypair, empty trust view of server.
        let conn = Connection::open_in_memory().unwrap();
        let store = crate::trusted::TrustedStore::new(Arc::new(Mutex::new(conn)));
        store.init_schema().unwrap();
        let attacker_trusted = Arc::new(Mutex::new(store));
        let attacker_id = crate::crypto::DeviceIdentity::generate().unwrap();
        let attacker = MtlsTransport::new(
            MtlsConfig {
                device_id: attacker_id.device_id.to_string(),
                port: 0,
            },
            attacker_trusted,
            &attacker_id,
        )
        .unwrap();
        let accept_fut = server.accept(&listener);
        let connect_fut = attacker.connect(addr, &server_name);
        let (accept_res, connect_res) = tokio::join!(accept_fut, connect_fut);
        // At least the handshake must fail on one side (fail-closed); both
        // failing is the expected outcome.
        assert!(
            accept_res.is_err() || connect_res.is_err(),
            "attacker must not establish mTLS"
        );
    }

    #[tokio::test]
    async fn mtls_rejects_wrong_sni() {
        // Paired client presenting the wrong expected name must fail:
        // SNI/device binding enforced.
        let (server, listener, client, _) = bind_pair().await;
        let port = listener.local_addr().unwrap().port();
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let accept_fut = server.accept(&listener);
        let connect_fut = client.connect(addr, "someone-else");
        let (accept_res, connect_res) = tokio::join!(accept_fut, connect_fut);
        assert!(
            accept_res.is_err() || connect_res.is_err(),
            "wrong SNI must not establish mTLS"
        );
    }
}
