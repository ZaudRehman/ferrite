//! mDNS discovery on `_ferrite._tcp.local.` Client isolated networks
//! report honest failures instead of silent absence.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tracing::debug;

#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    pub device_id: String,
    pub device_name: String,
    pub fingerprint: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    pub last_seen_ms: i64,
}

#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    pub device_id: String,
    pub device_name: String,
    pub fingerprint: String,
    pub port: u16,
}

pub struct DiscoveryService {
    config: DiscoveryConfig,
    daemon: ServiceDaemon,
    discovered: Arc<RwLock<HashMap<String, DiscoveredDevice>>>,
}

impl DiscoveryService {
    pub fn new(config: DiscoveryConfig) -> Result<Self, String> {
        let daemon = ServiceDaemon::new().map_err(|e| format!("mdns daemon: {e}"))?;
        Ok(Self {
            config,
            daemon,
            discovered: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub fn broadcast(&self) -> Result<ServiceInfo, String> {
        let host = format!("{}.local.", self.config.device_id);
        let props: HashMap<String, String> = HashMap::from([
            ("device_id".to_string(), self.config.device_id.clone()),
            ("device_name".to_string(), self.config.device_name.clone()),
            ("fingerprint".to_string(), self.config.fingerprint.clone()),
        ]);
        let info = ServiceInfo::new(
            "_ferrite._tcp.local.",
            &self.config.device_id,
            &host,
            "",
            self.config.port,
            props,
        )
        .map_err(|e| format!("service info: {e}"))?
        .enable_addr_auto();
        let out = info.clone();
        self.daemon
            .register(info)
            .map_err(|e| format!("register: {e}"))?;
        debug!(port = self.config.port, device_id = %self.config.device_id, "mDNS broadcast _ferrite._tcp");
        Ok(out)
    }

    /// Continuous resolution browsing for `_ferrite._tcp.local`. Pushes to
    /// the discovered map within 3s.
    pub fn start_browsing(&self) -> Result<(), String> {
        let receiver = self
            .daemon
            .browse("_ferrite._tcp.local.")
            .map_err(|e| format!("browse: {e}"))?;
        let discovered = Arc::clone(&self.discovered);
        let self_id = self.config.device_id.clone();
        std::thread::Builder::new()
            .name("ferrite-discovery".to_owned())
            .spawn(move || {
                let start = std::time::Instant::now();
                while let Ok(event) = receiver.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let fullname = info.get_fullname().to_owned();
                            let device_id = info
                                .get_property_val_str("device_id")
                                .unwrap_or(&fullname)
                                .to_owned();
                            if device_id == self_id {
                                continue;
                            }
                            let device_name = info
                                .get_property_val_str("device_name")
                                .unwrap_or("Ferrite Device")
                                .to_owned();
                            let fingerprint = info
                                .get_property_val_str("fingerprint")
                                .unwrap_or("")
                                .to_owned();
                            let port = info.get_port();
                            let addresses: Vec<IpAddr> = info
                                .get_addresses()
                                .iter()
                                .cloned()
                                .collect();
                            let dev = DiscoveredDevice {
                                device_id: device_id.clone(),
                                device_name,
                                fingerprint,
                                addresses,
                                port,
                                last_seen_ms: now_millis(),
                            };
                            if let Ok(mut map) = discovered.write() {
                                map.insert(fullname.clone(), dev);
                            }
                            debug!(%device_id, port, elapsed_ms = start.elapsed().as_millis(), "discovered");
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            if let Ok(mut map) = discovered.write() {
                                apply_removal(&mut map, &fullname);
                            }
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|e| format!("spawn: {e}"))?;
        Ok(())
    }

    /// Snapshot for UI "Available Devices" list. If empty after window, caller shows limitation message (3.3)
    pub fn snapshot(&self) -> Vec<DiscoveredDevice> {
        self.discovered
            .read()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn is_empty_with_hint(&self, since_ms: i64) -> Option<String> {
        let empty = self.discovered.read().map(|m| m.is_empty()).unwrap_or(true);
        if empty && now_millis() - since_ms > 5000 {
            Some(
                "No devices found: this network may block local discovery (client isolation). Pairing via QR still works if devices are on the same Wi-Fi, otherwise use relay (Part 12)."
                    .to_owned(),
            )
        } else {
            None
        }
    }

    /// Handle network interface changes gracefully (rebroadcast)
    pub fn rebroadcast_on_interface_change(&self) {
        // mdns-sd handles auto; we log for diagnostics
        debug!("network change detected - mdns auto-rebroadcast");
    }
}

/// Removal keyed by mDNS fullname (what `ServiceRemoved` hands us), NOT
/// device_id: comparing device_id != fullname never matched, so ghosts
/// accumulated forever.
fn apply_removal(map: &mut HashMap<String, DiscoveredDevice>, fullname: &str) {
    map.remove(fullname);
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
    #[test]
    fn removal_evicts_by_fullname() {
        let mut map = HashMap::new();
        map.insert(
            "dev-a._ferrite._tcp.local.".to_owned(),
            DiscoveredDevice {
                device_id: "dev-a".to_owned(),
                device_name: "A".to_owned(),
                fingerprint: String::new(),
                addresses: Vec::new(),
                port: 1,
                last_seen_ms: 0,
            },
        );
        // device_id alone must NOT evict (the old bug compared these).
        apply_removal(&mut map, "dev-a");
        assert_eq!(map.len(), 1);
        apply_removal(&mut map, "dev-a._ferrite._tcp.local.");
        assert!(map.is_empty());
    }

    #[test]
    fn config_txt() {
        let cfg = DiscoveryConfig {
            device_id: "test-123".to_owned(),
            device_name: "MacBook Pro".to_owned(),
            fingerprint: "abcd1234".to_owned(),
            port: 8443,
        };
        assert_eq!(cfg.device_id, "test-123");
        // ServiceInfo creation tested in integration (requires network)
    }
}
