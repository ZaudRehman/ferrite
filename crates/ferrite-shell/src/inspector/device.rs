//! Device emulation.

#[derive(Debug, Clone)]
pub struct DevicePreset {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub user_agent: &'static str,
}

pub const PRESETS: &[DevicePreset] = &[
    DevicePreset {
        name: "iPhone 14",
        width: 390,
        height: 844,
        user_agent: "Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Mobile/15E148 Safari/604.1",
    },
    DevicePreset {
        name: "Pixel 7",
        width: 412,
        height: 915,
        user_agent: "Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Mobile Safari/537.36",
    },
    DevicePreset {
        name: "iPad",
        width: 820,
        height: 1180,
        user_agent: "Mozilla/5.0 (iPad; CPU OS 16_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Mobile/15E148 Safari/604.1",
    },
    DevicePreset {
        name: "Desktop",
        width: 1280,
        height: 800,
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Safari/537.36",
    },
];

#[derive(Debug, Default)]
pub struct DeviceState {
    pub enabled: bool,
    pub preset: usize,
}

impl DeviceState {
    pub fn current(&self) -> &DevicePreset {
        &PRESETS[self.preset.min(PRESETS.len() - 1)]
    }

    pub fn ua_override_js(&self) -> String {
        if !self.enabled {
            return String::new();
        }
        format!(
            "Object.defineProperty(window.navigator, 'userAgent', {{get: () => {:?}}});",
            self.current().user_agent
        )
    }
}
