//! Command palette registry. Execution reuses [`crate::ipc::ChromeCommand`]
//! verbatim, so palette rows cannot desync from menu and toolbar paths.

use crate::ipc::ChromeCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDef {
    /// Stable id, posted back on run.
    pub id: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    /// Lowercase aliases, not substrings of the label.
    pub keywords: &'static str,
}

/// Every palette action. Menu backed rows reuse menu ids.
pub const COMMANDS: &[CommandDef] = &[
    CommandDef {
        id: "new-tab",
        label: "New Tab",
        hint: "Ctrl+T",
        keywords: "new tab open blank",
    },
    CommandDef {
        id: "find",
        label: "Find in Page",
        hint: "Ctrl+F",
        keywords: "find search page text",
    },
    CommandDef {
        id: "share",
        label: "Share This Page",
        hint: "",
        keywords: "share send link qr",
    },
    CommandDef {
        id: "qr-code",
        label: "Show QR Code",
        hint: "",
        keywords: "qr code scan phone share",
    },
    CommandDef {
        id: "bookmark-page",
        label: "Bookmark This Page",
        hint: "Ctrl+D",
        keywords: "bookmark star save favorite",
    },
    CommandDef {
        id: "bookmark-all",
        label: "Bookmark All Tabs",
        hint: "",
        keywords: "bookmark all tabs save",
    },
    CommandDef {
        id: "history",
        label: "History",
        hint: "",
        keywords: "history past visits",
    },
    CommandDef {
        id: "downloads",
        label: "Downloads",
        hint: "",
        keywords: "downloads files saved",
    },
    CommandDef {
        id: "bookmarks",
        label: "Bookmarks",
        hint: "",
        keywords: "bookmarks library saved",
    },
    CommandDef {
        id: "bookmarks-bar",
        label: "Cycle Bookmarks Bar",
        hint: "",
        keywords: "bookmarks bar show hide",
    },
    CommandDef {
        id: "side-panel",
        label: "Toggle Side Panel",
        hint: "Ctrl+B",
        keywords: "side panel sidebar bookmarks history reading",
    },
    CommandDef {
        id: "vertical-tabs",
        label: "Toggle Vertical Tabs",
        hint: "",
        keywords: "vertical tabs sidebar rail strip layout",
    },
    CommandDef {
        id: "settings",
        label: "Settings",
        hint: "",
        keywords: "settings preferences options",
    },
    CommandDef {
        id: "settings-privacy",
        label: "Privacy Settings",
        hint: "",
        keywords: "privacy adblock gpc tracking https",
    },
    CommandDef {
        id: "passwords",
        label: "Password Manager",
        hint: "",
        keywords: "passwords credentials vault login totp",
    },
    CommandDef {
        id: "archive",
        label: "Archive Search",
        hint: "",
        keywords: "archive snapshots time machine offline",
    },
    CommandDef {
        id: "task-manager",
        label: "Task Manager",
        hint: "Shift+Esc",
        keywords: "task manager processes performance memory",
    },
    CommandDef {
        id: "print",
        label: "Print",
        hint: "Ctrl+P",
        keywords: "print page",
    },
    CommandDef {
        id: "status",
        label: "Toggle Status Bar",
        hint: "",
        keywords: "status bar diagnostics counters",
    },
    CommandDef {
        id: "toggle-mute",
        label: "Mute Active Tab",
        hint: "",
        keywords: "mute silence audio sound tab",
    },
    CommandDef {
        id: "toggle-pin",
        label: "Pin Active Tab",
        hint: "",
        keywords: "pin tab keep",
    },
    CommandDef {
        id: "zoom-in",
        label: "Zoom In",
        hint: "Ctrl++",
        keywords: "zoom in larger text bigger",
    },
    CommandDef {
        id: "zoom-out",
        label: "Zoom Out",
        hint: "Ctrl+-",
        keywords: "zoom out smaller text",
    },
    CommandDef {
        id: "zoom-reset",
        label: "Reset Zoom",
        hint: "Ctrl+0",
        keywords: "zoom reset 100 default",
    },
    CommandDef {
        id: "go-back",
        label: "Go Back",
        hint: "Alt+Left",
        keywords: "back previous page history",
    },
    CommandDef {
        id: "go-forward",
        label: "Go Forward",
        hint: "Alt+Right",
        keywords: "forward next page history",
    },
    CommandDef {
        id: "reload",
        label: "Reload",
        hint: "Ctrl+R",
        keywords: "reload refresh page",
    },
    CommandDef {
        id: "home",
        label: "Home",
        hint: "",
        keywords: "home start page",
    },
    CommandDef {
        id: "sleep-active",
        label: "Sleep Active Tab",
        hint: "",
        keywords: "sleep unload discard inactive tab memory",
    },
    CommandDef {
        id: "welcome",
        label: "Replay Welcome Tour",
        hint: "",
        keywords: "welcome tour setup onboarding first run replay",
    },
];

/// Map a registry id to the executed command. Unknown ids return None.
pub fn palette_command(id: &str) -> Option<ChromeCommand> {
    match id {
        "toggle-mute" => Some(ChromeCommand::ToggleMute { id: None }),
        "toggle-pin" => Some(ChromeCommand::TogglePin { id: None }),
        "zoom-in" => Some(ChromeCommand::ZoomIn),
        "zoom-out" => Some(ChromeCommand::ZoomOut),
        "zoom-reset" => Some(ChromeCommand::ZoomReset),
        "go-back" => Some(ChromeCommand::GoBack { steps: 1 }),
        "go-forward" => Some(ChromeCommand::GoForward { steps: 1 }),
        "reload" => Some(ChromeCommand::Reload),
        "home" => Some(ChromeCommand::Home),
        _ if COMMANDS.iter().any(|c| c.id == id) => Some(ChromeCommand::MenuItem { id: id.into() }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_unique_labels_nonempty() {
        let mut seen = HashSet::new();
        assert!(!COMMANDS.is_empty());
        for c in COMMANDS {
            assert!(!c.id.is_empty() && !c.label.is_empty(), "blank row");
            assert!(seen.insert(c.id), "duplicate id {}", c.id);
            assert_eq!(
                c.keywords,
                c.keywords.to_lowercase(),
                "keywords lowercase {}",
                c.id
            );
        }
    }

    #[test]
    fn every_id_maps_to_a_command() {
        for c in COMMANDS {
            assert!(
                palette_command(c.id).is_some(),
                "registry id with no execution: {}",
                c.id
            );
        }
        assert_eq!(palette_command("nope"), None);
        assert_eq!(palette_command(""), None);
    }

    #[test]
    fn mapped_commands_stay_inside_the_safe_set() {
        // Palette reuses leaf actions only. New variants need review.
        for c in COMMANDS {
            match palette_command(c.id).unwrap() {
                ChromeCommand::MenuItem { .. }
                | ChromeCommand::ToggleMute { .. }
                | ChromeCommand::TogglePin { .. }
                | ChromeCommand::ZoomIn
                | ChromeCommand::ZoomOut
                | ChromeCommand::ZoomReset
                | ChromeCommand::GoBack { .. }
                | ChromeCommand::GoForward { .. }
                | ChromeCommand::Home
                | ChromeCommand::Reload => {}
                other => panic!("palette escapes safe set: {} -> {other:?}", c.id),
            }
        }
    }
}
