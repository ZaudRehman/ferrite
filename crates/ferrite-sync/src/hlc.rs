//! Hybrid logical clock. Wall plus logical, no NTP reliance.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct Hlc {
    inner: Arc<Mutex<HlcInner>>,
    device_id: String,
}

#[derive(Debug, Clone)]
struct HlcInner {
    last_physical: u64,
    last_logical: u64,
}

impl Hlc {
    pub fn new(device_id: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HlcInner {
                last_physical: now_millis(),
                last_logical: 0,
            })),
            device_id,
        }
    }

    /// Next timestamp (millis shifted plus logical). Stays monotonic under
    /// saturation by carrying into the physical part.
    pub fn next(&self) -> u64 {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let physical = now_millis();
        if physical > inner.last_physical {
            inner.last_physical = physical;
            inner.last_logical = 0;
        } else if inner.last_logical < 0xFFFF {
            inner.last_logical += 1;
        } else {
            // Saturated: carry into physical. Self corrects on the next tick.
            inner.last_physical += 1;
            inner.last_logical = 0;
        }
        (inner.last_physical << 16) | (inner.last_logical & 0xFFFF)
    }

    /// Observe a remote timestamp for causality. Same saturation rule.
    pub fn observe(&self, remote: u64) {
        let remote_phys = remote >> 16;
        let remote_log = remote & 0xFFFF;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let base_phys = now_millis().max(inner.last_physical).max(remote_phys);
        let mut phys = base_phys;
        let mut log = if base_phys == inner.last_physical && base_phys == remote_phys {
            inner.last_logical.max(remote_log) + 1
        } else if base_phys == remote_phys {
            remote_log + 1
        } else if base_phys == inner.last_physical {
            inner.last_logical + 1
        } else {
            0
        };
        if log > 0xFFFF {
            // Saturated within one millisecond: carry.
            phys = base_phys + 1;
            log = 0;
        }
        inner.last_physical = phys;
        inner.last_logical = log;
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monotonic() {
        let h = Hlc::new("dev-a".to_owned());
        let a = h.next();
        let b = h.next();
        assert!(b > a);
    }
    #[test]
    fn observe_increases() {
        let h = Hlc::new("dev-a".to_owned());
        let r = (now_millis() + 1000) << 16;
        h.observe(r);
        let n = h.next();
        assert!(n > r);
    }

    #[test]
    fn burst_stays_monotonic_past_logical_capacity() {
        // 70k stamps in a tight loop: logical part saturates at 65535 and
        // must carry (never wrap backwards).
        let h = Hlc::new("dev-burst".to_owned());
        let mut prev = h.next();
        for _ in 0..70_000 {
            let cur = h.next();
            assert!(cur > prev, "HLC went backwards: {cur} <= {prev}");
            prev = cur;
        }
    }
}
