//! The shell's shortcut vocabulary, and the reporter that carries a key press
//! across the webview boundary.
//!
//! ## Why this module exists
//!
//! Windows delivers key messages to the *focus window*, and a wry webview takes
//! that focus at creation (`WebViewAttributes::focused` defaults to true, and
//! wry moves focus into the document again whenever the parent window takes it).
//! The OS focus window is therefore Chromium's widget, not the winit window, so
//! `WindowEvent::KeyboardInput` — the only input `ShellApp::on_local_key` ever
//! had — never fires while a page or the chrome is focused. Measured, not
//! inferred: see `tasks/keyboard-path.md`.
//!
//! So a key press has two possible carriers, and both must reach one decision:
//!
//! * the winit window, when it holds focus (startup, before any webview);
//! * the focused webview's document, reported over the content IPC.
//!
//! This module owns the part both carriers must agree on: which physical keys
//! are bound, with which modifiers, what the webview does with one it matches
//! (`Claim`), and which `Action` it means. `BINDINGS` is the single table; the
//! reporter's matcher is *generated* from it (never hand-written), so the JS and
//! Rust cannot drift, and `ferrite-shell` dispatches on `Action` instead of
//! re-reading modifiers.
//!
//! ## Physical keys, deliberately
//!
//! Bindings are physical (`KeyboardEvent.code` / winit `KeyCode`), not logical:
//! Ctrl+T is the same physical key on a Cyrillic layout, which is what Chrome's
//! own accelerator table does. `Character` matching would make every binding
//! layout-dependent.
//!
//! ## The token
//!
//! A shortcut report is a privileged instruction (open a tab, close a tab,
//! toggle devtools), and the content IPC is default-deny for exactly that reason
//! (`shell.rs:11500`): any page can call `window.ipc.postMessage` with arbitrary
//! JSON. A plain `shortcut` type would therefore hand every visited page the
//! ability to drive the browser. Instead the factory mints a per-webview token
//! (`token_for`), bakes it into the reporter's closure, and the shell verifies it
//! against the sending webview. The token is derived from a per-process CSPRNG
//! secret, so it is unguessable and needs no state to keep in sync; the reporter
//! posts through `window.ipc.postMessage`, which wry installs as a frozen,
//! non-configurable property, so page script cannot learn or wrap it.

use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use uuid::Uuid;
use winit::keyboard::KeyCode;

/// A physical key the shell binds. Names match `KeyboardEvent.code`, so the
/// reporter and the winit adapter compare the same token on every platform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Code {
    KeyA,
    KeyB,
    KeyD,
    KeyE,
    KeyF,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyN,
    KeyO,
    KeyP,
    KeyR,
    KeyT,
    KeyW,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Equal,
    Minus,
    NumpadAdd,
    NumpadSubtract,
    Escape,
    Tab,
    F3,
    F4,
    F5,
    F6,
    F10,
    F11,
    F12,
    ArrowLeft,
    ArrowRight,
}

impl Code {
    /// The `KeyboardEvent.code` value for this key.
    pub fn js_code(self) -> &'static str {
        match self {
            Code::KeyA => "KeyA",
            Code::KeyB => "KeyB",
            Code::KeyD => "KeyD",
            Code::KeyE => "KeyE",
            Code::KeyF => "KeyF",
            Code::KeyH => "KeyH",
            Code::KeyI => "KeyI",
            Code::KeyJ => "KeyJ",
            Code::KeyK => "KeyK",
            Code::KeyL => "KeyL",
            Code::KeyN => "KeyN",
            Code::KeyO => "KeyO",
            Code::KeyP => "KeyP",
            Code::KeyR => "KeyR",
            Code::KeyT => "KeyT",
            Code::KeyW => "KeyW",
            Code::Digit0 => "Digit0",
            Code::Digit1 => "Digit1",
            Code::Digit2 => "Digit2",
            Code::Digit3 => "Digit3",
            Code::Digit4 => "Digit4",
            Code::Digit5 => "Digit5",
            Code::Digit6 => "Digit6",
            Code::Digit7 => "Digit7",
            Code::Digit8 => "Digit8",
            Code::Digit9 => "Digit9",
            Code::Equal => "Equal",
            Code::Minus => "Minus",
            Code::NumpadAdd => "NumpadAdd",
            Code::NumpadSubtract => "NumpadSubtract",
            Code::Escape => "Escape",
            Code::Tab => "Tab",
            Code::F3 => "F3",
            Code::F4 => "F4",
            Code::F5 => "F5",
            Code::F6 => "F6",
            Code::F10 => "F10",
            Code::F11 => "F11",
            Code::F12 => "F12",
            Code::ArrowLeft => "ArrowLeft",
            Code::ArrowRight => "ArrowRight",
        }
    }

    /// Parse the `code` field of a shortcut report. Unknown codes are not
    /// shortcuts here (a newer reporter, or a hand-rolled message).
    pub fn from_js(code: &str) -> Option<Code> {
        Some(match code {
            "KeyA" => Code::KeyA,
            "KeyB" => Code::KeyB,
            "KeyD" => Code::KeyD,
            "KeyE" => Code::KeyE,
            "KeyF" => Code::KeyF,
            "KeyH" => Code::KeyH,
            "KeyI" => Code::KeyI,
            "KeyJ" => Code::KeyJ,
            "KeyK" => Code::KeyK,
            "KeyL" => Code::KeyL,
            "KeyN" => Code::KeyN,
            "KeyO" => Code::KeyO,
            "KeyP" => Code::KeyP,
            "KeyR" => Code::KeyR,
            "KeyT" => Code::KeyT,
            "KeyW" => Code::KeyW,
            "Digit0" => Code::Digit0,
            "Digit1" => Code::Digit1,
            "Digit2" => Code::Digit2,
            "Digit3" => Code::Digit3,
            "Digit4" => Code::Digit4,
            "Digit5" => Code::Digit5,
            "Digit6" => Code::Digit6,
            "Digit7" => Code::Digit7,
            "Digit8" => Code::Digit8,
            "Digit9" => Code::Digit9,
            "Equal" => Code::Equal,
            "Minus" => Code::Minus,
            "NumpadAdd" => Code::NumpadAdd,
            "NumpadSubtract" => Code::NumpadSubtract,
            "Escape" => Code::Escape,
            "Tab" => Code::Tab,
            "F3" => Code::F3,
            "F4" => Code::F4,
            "F5" => Code::F5,
            "F6" => Code::F6,
            "F10" => Code::F10,
            "F11" => Code::F11,
            "F12" => Code::F12,
            "ArrowLeft" => Code::ArrowLeft,
            "ArrowRight" => Code::ArrowRight,
            _ => return None,
        })
    }

    /// Map a winit physical key. `None` for anything unbound, which is the
    /// common case (every key that is not a shell shortcut).
    pub fn from_winit(code: KeyCode) -> Option<Code> {
        Some(match code {
            KeyCode::KeyA => Code::KeyA,
            KeyCode::KeyB => Code::KeyB,
            KeyCode::KeyD => Code::KeyD,
            KeyCode::KeyE => Code::KeyE,
            KeyCode::KeyF => Code::KeyF,
            KeyCode::KeyH => Code::KeyH,
            KeyCode::KeyI => Code::KeyI,
            KeyCode::KeyJ => Code::KeyJ,
            KeyCode::KeyK => Code::KeyK,
            KeyCode::KeyL => Code::KeyL,
            KeyCode::KeyN => Code::KeyN,
            KeyCode::KeyO => Code::KeyO,
            KeyCode::KeyP => Code::KeyP,
            KeyCode::KeyR => Code::KeyR,
            KeyCode::KeyT => Code::KeyT,
            KeyCode::KeyW => Code::KeyW,
            KeyCode::Digit0 => Code::Digit0,
            KeyCode::Digit1 => Code::Digit1,
            KeyCode::Digit2 => Code::Digit2,
            KeyCode::Digit3 => Code::Digit3,
            KeyCode::Digit4 => Code::Digit4,
            KeyCode::Digit5 => Code::Digit5,
            KeyCode::Digit6 => Code::Digit6,
            KeyCode::Digit7 => Code::Digit7,
            KeyCode::Digit8 => Code::Digit8,
            KeyCode::Digit9 => Code::Digit9,
            KeyCode::Equal => Code::Equal,
            KeyCode::Minus => Code::Minus,
            KeyCode::NumpadAdd => Code::NumpadAdd,
            KeyCode::NumpadSubtract => Code::NumpadSubtract,
            KeyCode::Escape => Code::Escape,
            KeyCode::Tab => Code::Tab,
            KeyCode::F3 => Code::F3,
            KeyCode::F4 => Code::F4,
            KeyCode::F5 => Code::F5,
            KeyCode::F6 => Code::F6,
            KeyCode::F10 => Code::F10,
            KeyCode::F11 => Code::F11,
            KeyCode::F12 => Code::F12,
            KeyCode::ArrowLeft => Code::ArrowLeft,
            KeyCode::ArrowRight => Code::ArrowRight,
            _ => return None,
        })
    }
}

/// What a binding requires of one modifier: held, not held, or don't care.
/// "Don't care" is a real state, not laziness: `F3` is find-next whatever else
/// is down, and forcing a value there would silently drop Ctrl+F3.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Req {
    On,
    Off,
    Any,
}

impl Req {
    fn accepts(self, held: bool) -> bool {
        match self {
            Req::On => held,
            Req::Off => !held,
            Req::Any => true,
        }
    }
}

/// What the webview does with a key it matches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Claim {
    /// Swallow it (`preventDefault` + `stopImmediatePropagation`) and forward.
    /// Every modifier combination belongs here: a page must not also act on
    /// Ctrl+T, and the shell's version is the browser's version.
    Always,
    /// Swallow it only when the event target is not a site-owned surface, and
    /// only then forward it. A site-owned surface is a text-entry field, a
    /// focused canvas/video, a `role=application` region, or a page whose main
    /// surface is a large canvas. Inside one the binding is **inert**: neither
    /// swallowed nor forwarded. This is the one place a shell binding may not
    /// steal a site's own keyboard grammar: Figma keeps `f`, Ctrl+B, Ctrl+K and
    /// Ctrl+Shift+O on its canvas and in text editing; a plain page still gets
    /// Ferrite's link hints, panel and palette.
    OnlyOutsideSiteSurface,
    /// Forward, never swallow. Escape is deliberately here: browsers also give
    /// Escape to the page, and swallowing it would break page dialogs. The
    /// shell's own Escape work (close an overlay, stop a load) is additive.
    Never,
}

/// One shell action. The shell executes these; it never re-derives them from
/// modifiers, which is what keeps the table authoritative.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    ToggleTabSearch,
    OpenDevtools,
    OpenPrivateTab,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    OpenTaskManager,
    OpenHistory,
    OpenDownloads,
    OpenBookmarks,
    OpenMenu,
    /// Close an open overlay, else stop a loading tab. One action because the
    /// executor's state check is the same one the old arm order encoded.
    DismissOrStop,
    ToggleFullscreen,
    FindNextOrOpen,
    FindPrevIfOpen,
    CycleThemePack,
    TogglePanel,
    CycleBarMode,
    OpenFind,
    LinkHints,
    FocusOmnibox,
    FocusContent,
    Print,
    NewTab,
    ReopenTab,
    CycleTabNext,
    CycleTabPrev,
    /// 1-9: jump to that tab, or the last tab for 9.
    SwitchTab(u8),
    CloseTab,
    ToggleStar,
    ReloadOrStop,
    HardReload,
    Back,
    Forward,
}

/// One binding: a physical key, the modifier pattern that selects it, what the
/// webview does with it, and the action it means.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub code: Code,
    pub primary: Req,
    pub shift: Req,
    pub alt: Req,
    pub claim: Claim,
    pub action: Action,
}

/// `primary` is the platform's main accelerator modifier: Ctrl everywhere, and
/// additionally Cmd on macOS (the key that means "browser command" to a user).
/// On Windows the extra `super` check is the Windows key, which the OS claims
/// for its own chords long before a webview sees it, so this stays uniform
/// rather than growing per-platform branches.
const fn b(
    code: Code,
    primary: Req,
    shift: Req,
    alt: Req,
    claim: Claim,
    action: Action,
) -> Binding {
    Binding {
        code,
        primary,
        shift,
        alt,
        claim,
        action,
    }
}

/// Every shell shortcut, **in match order**: the first binding whose key and
/// modifier pattern fit wins, exactly like the ordered arms this replaced.
/// Order matters where patterns overlap — Shift+Escape before Escape,
/// Ctrl+Shift+B before Ctrl+B — so new bindings go in with that in mind.
pub const BINDINGS: &[Binding] = &[
    // Ctrl+Shift+<key>. `alt` is Off, not Any: Ctrl+Alt is AltGr on several
    // layouts, and claiming AltGr+Shift+A inside a text field to open a
    // command palette would be a typing bug, not a feature.
    b(
        Code::KeyA,
        Req::On,
        Req::On,
        Req::Off,
        Claim::Always,
        Action::ToggleTabSearch,
    ),
    b(
        Code::KeyI,
        Req::On,
        Req::On,
        Req::Off,
        Claim::Always,
        Action::OpenDevtools,
    ),
    b(
        Code::KeyN,
        Req::On,
        Req::On,
        Req::Off,
        Claim::Always,
        Action::OpenPrivateTab,
    ),
    b(
        Code::KeyB,
        Req::On,
        Req::On,
        Req::Off,
        Claim::Always,
        Action::CycleBarMode,
    ),
    // Ctrl+Shift+F: link hints. A bare letter is not a browser shortcut; Chrome
    // leaves `f` to Figma's frame tool, YouTube's fullscreen and Gmail's
    // forward. Claim 2 keeps the chord inert on canvas/editable surfaces too.
    b(
        Code::KeyF,
        Req::On,
        Req::On,
        Req::Off,
        Claim::OnlyOutsideSiteSurface,
        Action::LinkHints,
    ),
    // Chrome's bookmarks manager. Claim 2 lets a focused design canvas keep
    // Figma's Ctrl+Shift+O (outline stroke); ordinary pages get the manager.
    b(
        Code::KeyO,
        Req::On,
        Req::On,
        Req::Off,
        Claim::OnlyOutsideSiteSurface,
        Action::OpenBookmarks,
    ),
    b(
        Code::KeyR,
        Req::On,
        Req::On,
        Req::Off,
        Claim::Always,
        Action::HardReload,
    ),
    b(
        Code::KeyT,
        Req::On,
        Req::On,
        Req::Off,
        Claim::Always,
        Action::ReopenTab,
    ),
    b(
        Code::Tab,
        Req::On,
        Req::On,
        Req::Off,
        Claim::Always,
        Action::CycleTabPrev,
    ),
    // Ctrl+<key>.
    b(
        Code::KeyB,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::OnlyOutsideSiteSurface,
        Action::TogglePanel,
    ),
    b(
        Code::KeyF,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::OpenFind,
    ),
    // Chrome-owned pages. Figma web cannot keep Ctrl+H/Ctrl+J either; those
    // are browser commands in Chrome, not site shortcuts.
    b(
        Code::KeyH,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::OpenHistory,
    ),
    b(
        Code::KeyJ,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::OpenDownloads,
    ),
    b(
        Code::KeyL,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::FocusOmnibox,
    ),
    b(
        Code::KeyP,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::Print,
    ),
    b(
        Code::KeyT,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::NewTab,
    ),
    b(
        Code::KeyK,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::OnlyOutsideSiteSurface,
        Action::ToggleTabSearch,
    ),
    b(
        Code::KeyW,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::CloseTab,
    ),
    b(
        Code::KeyD,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::ToggleStar,
    ),
    b(
        Code::KeyR,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::ReloadOrStop,
    ),
    b(
        Code::Tab,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::CycleTabNext,
    ),
    // Zoom: Shift is Any so both Ctrl+= and Ctrl+Shift+= (the "+" a user
    // actually reaches for) zoom in, as they do in Chrome.
    b(
        Code::Equal,
        Req::On,
        Req::Any,
        Req::Off,
        Claim::Always,
        Action::ZoomIn,
    ),
    b(
        Code::Minus,
        Req::On,
        Req::Any,
        Req::Off,
        Claim::Always,
        Action::ZoomOut,
    ),
    // The numeric keypad reports stable `KeyboardEvent.code` values
    // (`NumpadAdd`/`NumpadSubtract`, UI Events), and WebView2 handled
    // Ctrl+Numpad+/- before Task 24 turned its accelerators off. They are
    // bound here so disabling the engine's accelerators did not silently
    // remove the keypad zoom a desktop keyboard has.
    b(
        Code::NumpadAdd,
        Req::On,
        Req::Any,
        Req::Off,
        Claim::Always,
        Action::ZoomIn,
    ),
    b(
        Code::NumpadSubtract,
        Req::On,
        Req::Any,
        Req::Off,
        Claim::Always,
        Action::ZoomOut,
    ),
    b(
        Code::Digit0,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::ZoomReset,
    ),
    b(
        Code::Digit1,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::SwitchTab(1),
    ),
    b(
        Code::Digit2,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::SwitchTab(2),
    ),
    b(
        Code::Digit3,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::SwitchTab(3),
    ),
    b(
        Code::Digit4,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::SwitchTab(4),
    ),
    b(
        Code::Digit5,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::SwitchTab(5),
    ),
    b(
        Code::Digit6,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::SwitchTab(6),
    ),
    b(
        Code::Digit7,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::SwitchTab(7),
    ),
    b(
        Code::Digit8,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::SwitchTab(8),
    ),
    b(
        Code::Digit9,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::SwitchTab(9),
    ),
    // Modifier-only chords and function keys. Escape alone must not be
    // swallowed; Shift+Escape (task manager) may be.
    b(
        Code::Escape,
        Req::Off,
        Req::On,
        Req::Off,
        Claim::Always,
        Action::OpenTaskManager,
    ),
    b(
        Code::Escape,
        Req::Off,
        Req::Off,
        Req::Off,
        Claim::Never,
        Action::DismissOrStop,
    ),
    b(
        Code::F11,
        Req::Off,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::ToggleFullscreen,
    ),
    b(
        Code::F12,
        Req::Off,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::OpenDevtools,
    ),
    b(
        Code::F4,
        Req::Off,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::CycleThemePack,
    ),
    // Ctrl+F5 is Chrome's hard reload. It must sit before the bare F5 row:
    // F5's primary is Off, so the patterns do not overlap, but keeping the
    // chord adjacent makes the pair obvious.
    b(
        Code::F5,
        Req::On,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::HardReload,
    ),
    // F5 is the reload key users reach for: WebView2 used to handle it until
    // Task 24 disabled engine accelerators, so it must be in the table or it
    // is dead. Ctrl+R stays the table's other reload chord.
    b(
        Code::F5,
        Req::Off,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::ReloadOrStop,
    ),
    // F6 is Chrome's chrome-region entry point. It lands on the address bar;
    // full tab-strip/bookmarks cycling is a separate focus-owner problem (the
    // shell cannot observe which webview owns the OS focus).
    b(
        Code::F6,
        Req::Off,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::FocusOmnibox,
    ),
    // Shift+F6 is the return leg of the two-stop chrome cycle: address bar to
    // page. Full tab-strip/bookmarks cycling needs a focus-owner signal the
    // shell does not have; the pair is honest and reversible.
    b(
        Code::F6,
        Req::Off,
        Req::On,
        Req::Off,
        Claim::Always,
        Action::FocusContent,
    ),
    b(
        Code::F3,
        Req::Off,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::FindNextOrOpen,
    ),
    b(
        Code::F3,
        Req::Off,
        Req::On,
        Req::Off,
        Claim::Always,
        Action::FindPrevIfOpen,
    ),
    b(
        Code::ArrowLeft,
        Req::Off,
        Req::Any,
        Req::On,
        Claim::Always,
        Action::Back,
    ),
    b(
        Code::ArrowRight,
        Req::Off,
        Req::Any,
        Req::On,
        Claim::Always,
        Action::Forward,
    ),
    // Chrome's menu chords: Alt+F, Alt+E and F10 open the same menu. The
    // shell anchors it under the top-right menu button. F10 must stay bare:
    // Alt+F4 belongs to the OS, not the menu.
    b(
        Code::KeyF,
        Req::Off,
        Req::Off,
        Req::On,
        Claim::Always,
        Action::OpenMenu,
    ),
    b(
        Code::KeyE,
        Req::Off,
        Req::Off,
        Req::On,
        Claim::Always,
        Action::OpenMenu,
    ),
    b(
        Code::F10,
        Req::Off,
        Req::Off,
        Req::Off,
        Claim::Always,
        Action::OpenMenu,
    ),
    // Alt+D is Chrome's address-bar focus. Figma desktop uses Alt+D for align
    // right, but Chrome already owns it on the web; matching Chrome is the
    // browser-compatible behavior.
    b(
        Code::KeyD,
        Req::Off,
        Req::Off,
        Req::On,
        Claim::Always,
        Action::FocusOmnibox,
    ),
];

/// Resolve a key press to its action. First match wins.
pub fn lookup(code: Code, primary: bool, shift: bool, alt: bool) -> Option<Action> {
    find(code, primary, shift, alt).map(|b| b.action)
}

/// The binding a key press matches, claim mode included.
pub fn find(code: Code, primary: bool, shift: bool, alt: bool) -> Option<&'static Binding> {
    BINDINGS.iter().find(|b| {
        b.code == code && b.primary.accepts(primary) && b.shift.accepts(shift) && b.alt.accepts(alt)
    })
}

/// The claim mode for a key press, if it is a shell shortcut at all.
pub fn claim_for(code: Code, primary: bool, shift: bool, alt: bool) -> Option<Claim> {
    find(code, primary, shift, alt).map(|b| b.claim)
}

/// Resolve a shortcut report's fields (as posted by the reporter). `None` when
/// the combination is not a shell binding, which is the honest answer for a
/// hand-rolled message.
pub fn action_for_report(code: &str, primary: bool, shift: bool, alt: bool) -> Option<Action> {
    let code = Code::from_js(code)?;
    lookup(code, primary, shift, alt)
}

// ── The reporter ──

/// The script injected into every webview that reports shell shortcuts.
///
/// Generated from `BINDINGS`, with the claim mode inlined as a number (1 claim,
/// 2 claim-unless-site-surface, 3 forward-only) so the matcher code is a direct
/// transcription of the table rather than a second list to keep in sync.
/// Dispatch: `1` swallows and forwards, `2` does exactly that outside a
/// site-owned surface and nothing at all inside one, `3` forwards without
/// swallowing.
/// `token` is this webview's capability (see `token_for`).
pub fn reporter_js(token: &str) -> String {
    let mut matcher = String::new();
    for binding in BINDINGS {
        let mut cond = vec![format!("e.code==='{}'", binding.code.js_code())];
        match binding.primary {
            Req::On => cond.push("(e.ctrlKey||e.metaKey)".to_string()),
            Req::Off => cond.push("!(e.ctrlKey||e.metaKey)".to_string()),
            Req::Any => {}
        }
        match binding.shift {
            Req::On => cond.push("e.shiftKey".to_string()),
            Req::Off => cond.push("!e.shiftKey".to_string()),
            Req::Any => {}
        }
        match binding.alt {
            Req::On => cond.push("e.altKey".to_string()),
            Req::Off => cond.push("!e.altKey".to_string()),
            Req::Any => {}
        }
        let claim = match binding.claim {
            Claim::Always => 1,
            Claim::OnlyOutsideSiteSurface => 2,
            Claim::Never => 3,
        };
        matcher.push_str(&format!("  if({})return {};\n", cond.join("&&"), claim));
    }
    format!(
        r#"// Ferrite shell shortcuts: report a bound key to the shell so it acts even
// though the OS delivers the key to this document, not the host window.
(function(){{
  if(window.__ferriteShortcuts)return;
  window.__ferriteShortcuts=1;
  // Every frame, not just the top one: a key event never crosses a document
  // boundary, so the focused frame is the only place the press appears. The
  // old top-frame guard made every shortcut dead while focus sat in an embed.
  var token='{token}';
  // Capture the sink and JSON at install, before page script runs. wry's
  // `window.ipc` wrapper resolves `window.chrome.webview.postMessage` at call
  // time, so a page that replaces it after load could read the token out of
  // our own report and post forged shortcuts. A captured native method cannot
  // be replaced from script.
  var stringify=JSON.stringify;
  var post=null;
  try{{
    var wv=window.chrome&&window.chrome.webview;
    if(wv&&wv.postMessage)post=wv.postMessage.bind(wv);
    else if(window.ipc&&window.ipc.postMessage)post=window.ipc.postMessage.bind(window.ipc);
  }}catch(x){{}}
  function textEntry(el){{
    for(var n=el;n;n=(n.parentElement||(n.getRootNode&&n.getRootNode().host))){{
      if(n.nodeType!==1)continue;
      if(n.tagName==='TEXTAREA'||n.tagName==='SELECT')return true;
      if(n.tagName==='INPUT'){{
        // Only text-entry controls count. A focused checkbox, button or
        // slider is not a place the user can type, so a site-surface claim
        // must not treat it as one.
        var t=(n.getAttribute('type')||'text').toLowerCase();
        return t!=='button'&&t!=='submit'&&t!=='reset'&&t!=='checkbox'&&
          t!=='radio'&&t!=='range'&&t!=='file'&&t!=='color'&&t!=='image';
      }}
      if(n.isContentEditable)return true;
    }}
    return false;
  }}
  function siteSurface(el){{
    if(textEntry(el))return true;
    for(var n=el;n;n=(n.parentElement||(n.getRootNode&&n.getRootNode().host))){{
      if(n.nodeType!==1)continue;
      // Figma and other design tools put the design on a canvas and give the
      // keyboard to it. A focused canvas/video, or a region that explicitly
      // declares itself an application, owns its keys even when the focused
      // node is a child wrapper rather than the canvas itself.
      if(n.tagName==='CANVAS'||n.tagName==='VIDEO')return true;
      if(n.getAttribute&&n.getAttribute('role')==='application')return true;
    }}
    // A canvas that covers most of the viewport is an application surface even
    // when the user last clicked a non-focusable wrapper and focus sits on
    // body. This is the Figma web case; a small decorative canvas does not
    // qualify.
    try{{
      var vw=window.innerWidth||0,vh=window.innerHeight||0;
      if(vw>0&&vh>0){{
        var canvases=document.querySelectorAll('canvas');
        for(var i=0;i<canvases.length;i++){{
          var c=canvases[i];
          var cs=window.getComputedStyle?window.getComputedStyle(c):null;
          // A hidden or click-through canvas is a decorative backdrop, not
          // the surface the user is typing into. Counting it would make a
          // full-screen animation steal Ctrl+B/K/Shift+O from the page.
          if(cs&&(cs.pointerEvents==='none'||cs.visibility==='hidden'||cs.display==='none'))continue;
          var r=c.getBoundingClientRect();
          if(r.width*r.height>=vw*vh*0.5)return true;
        }}
      }}
    }}catch(x){{}}
    return false;
  }}
  function claimOf(e){{
{matcher}    return 0;
  }}
  window.addEventListener('keydown',function(e){{
    // Fail closed on anything a page could fake: a synthetic KeyboardEvent has
    // isTrusted false, and a composition commit (Enter on a CJK IME) is the
    // IME's key, not the user's shortcut.
    if(e.isTrusted!==true)return;
    if(e.isComposing)return;
    var c=claimOf(e);
    if(c===0)return;
    // Claim 2 is inert inside a site-owned surface: the keystroke belongs to
    // the page (Figma's frame tool, YouTube's fullscreen, a text field), so the
    // binding neither swallows it nor lets the shell act on it.
    // `composedPath()[0]` is the real target inside a shadow root; at window
    // level `e.target` is retargeted to the host, which hid the field.
    var target=(e.composedPath&&e.composedPath()[0])||e.target;
    if(c===2&&siteSurface(target))return;
    // Claim 3 (Escape) is forwarded without being swallowed: browsers give
    // Escape to the page too, and the shell's handling is additive.
    if(c!==3){{
      e.preventDefault();
      e.stopImmediatePropagation();
    }}
    try{{
      if(post)post(stringify({{
        type:'shortcut',token:token,code:e.code,
        primary:!!(e.ctrlKey||e.metaKey),shift:!!e.shiftKey,alt:!!e.altKey,
        repeat:!!e.repeat
      }}));
    }}catch(x){{}}
  }},true);
}})();
"#
    )
}

// ── The capability token ──

fn secret() -> &'static [u8; 32] {
    static SECRET: OnceLock<[u8; 32]> = OnceLock::new();
    SECRET.get_or_init(|| {
        // Two v4 UUIDs are 32 CSPRNG bytes: a page cannot learn the secret, so
        // it cannot compute another webview's token, and nothing has to be
        // stored per webview (overlays are created and destroyed constantly;
        // a registry would be a leak to bound).
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        bytes
    })
}

/// This webview's token: unguessable, stable for the process, and derived from
/// the id the webview's IPC messages carry.
pub fn token_for(tab_id: Uuid) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret());
    hasher.update(tab_id.as_bytes());
    hex(&hasher.finalize())
}

/// Constant-time check of a report's token against the sending webview's.
pub fn verify_token(tab_id: Uuid, provided: Option<&str>) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    let expected = token_for(tab_id);
    if provided.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, z) in provided.bytes().zip(expected.bytes()) {
        diff |= a ^ z;
    }
    diff == 0
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0xf) as u32, 16).unwrap_or('0'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed(code: Code, primary: bool, shift: bool, alt: bool) -> Option<Action> {
        lookup(code, primary, shift, alt)
    }

    #[test]
    fn every_action_is_reachable_from_the_table() {
        // The shell's `handle_shortcut` match is exhaustive, so a new binding
        // cannot arrive without a body. This is the other direction: an action
        // nothing binds would be dead code the compiler cannot see. Bump this
        // number only with a binding to match.
        let mut seen: Vec<Action> = Vec::new();
        for binding in BINDINGS {
            // The nine tab digits are one action; count the variant once.
            let canonical = match binding.action {
                Action::SwitchTab(_) => Action::SwitchTab(0),
                other => other,
            };
            if !seen.contains(&canonical) {
                seen.push(canonical);
            }
        }
        assert_eq!(seen.len(), 34, "actions reachable from the table: {seen:?}");
    }

    #[test]
    fn every_binding_resolves_by_its_own_pattern() {
        for binding in BINDINGS {
            for primary in [false, true] {
                for shift in [false, true] {
                    for alt in [false, true] {
                        if !binding.primary.accepts(primary)
                            || !binding.shift.accepts(shift)
                            || !binding.alt.accepts(alt)
                        {
                            continue;
                        }
                        // An earlier overlapping binding may legitimately win
                        // (Shift+Escape before Escape): only assert that the
                        // *action* is reachable, not that this row is the
                        // winner for every combination it accepts.
                        let resolved = lookup(binding.code, primary, shift, alt);
                        assert!(
                            resolved.is_some(),
                            "{binding:?} accepts primary={primary} shift={shift} alt={alt} but resolves to nothing"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn shadowed_rows_are_deliberate_and_documented() {
        // The deliberate overlaps in the table. If a future row shadows another
        // by accident, one of these lookups changes and the test says so.
        assert_eq!(
            pressed(Code::Escape, false, true, false),
            Some(Action::OpenTaskManager),
            "Shift+Escape must win over the bare Escape binding"
        );
        assert_eq!(
            pressed(Code::Escape, false, false, false),
            Some(Action::DismissOrStop)
        );
        assert_eq!(
            pressed(Code::KeyB, true, true, false),
            Some(Action::CycleBarMode),
            "Ctrl+Shift+B must win over Ctrl+B"
        );
        assert_eq!(
            pressed(Code::KeyB, true, false, false),
            Some(Action::TogglePanel)
        );
        assert_eq!(
            pressed(Code::Tab, true, true, false),
            Some(Action::CycleTabPrev),
            "Ctrl+Shift+Tab must win over Ctrl+Tab"
        );
        assert_eq!(
            pressed(Code::Tab, true, false, false),
            Some(Action::CycleTabNext)
        );
        assert_eq!(
            pressed(Code::F3, false, true, false),
            Some(Action::FindPrevIfOpen)
        );
        assert_eq!(
            pressed(Code::F3, false, false, false),
            Some(Action::FindNextOrOpen)
        );
        assert_eq!(
            pressed(Code::F6, false, false, false),
            Some(Action::FocusOmnibox)
        );
        assert_eq!(
            pressed(Code::F6, false, true, false),
            Some(Action::FocusContent)
        );
        assert_eq!(
            pressed(Code::KeyF, true, false, false),
            Some(Action::OpenFind)
        );
        assert_eq!(
            pressed(Code::KeyF, true, true, false),
            Some(Action::LinkHints)
        );
        assert_eq!(
            pressed(Code::KeyR, true, false, false),
            Some(Action::ReloadOrStop)
        );
        assert_eq!(
            pressed(Code::KeyR, true, true, false),
            Some(Action::HardReload)
        );
    }

    #[test]
    fn bare_letters_are_not_shortcuts() {
        // The regression that motivated Ctrl-gating: typing must never open or
        // close a tab. Link hints moved off bare `f` because Figma's frame
        // tool, YouTube's fullscreen and Gmail's forward all own that key in a
        // real page; the browser must not take it.
        for code in [
            Code::KeyA,
            Code::KeyB,
            Code::KeyD,
            Code::KeyE,
            Code::KeyF,
            Code::KeyH,
            Code::KeyI,
            Code::KeyJ,
            Code::KeyK,
            Code::KeyL,
            Code::KeyN,
            Code::KeyO,
            Code::KeyP,
            Code::KeyR,
            Code::KeyT,
            Code::KeyW,
        ] {
            assert_eq!(pressed(code, false, false, false), None, "{code:?} bare");
            assert_eq!(pressed(code, false, true, false), None, "{code:?} shift");
        }
        assert_eq!(
            pressed(Code::KeyF, true, true, false),
            Some(Action::LinkHints),
            "Ctrl+Shift+F is link hints"
        );
        assert_eq!(
            claim_for(Code::KeyF, true, true, false),
            Some(Claim::OnlyOutsideSiteSurface)
        );
    }

    #[test]
    fn altgr_style_chords_are_not_claimed() {
        // Ctrl+Alt+Shift+A is AltGr+Shift+A on several layouts.
        assert_eq!(pressed(Code::KeyA, true, true, true), None);
        assert_eq!(pressed(Code::KeyL, true, false, true), None);
        // Alt+Arrow is the chord that does keep Alt, and it excludes the rest.
        assert_eq!(
            pressed(Code::ArrowLeft, false, false, true),
            Some(Action::Back)
        );
        assert_eq!(pressed(Code::ArrowLeft, true, false, true), None);
    }

    #[test]
    fn escape_is_forwarded_but_never_swallowed() {
        assert_eq!(
            claim_for(Code::Escape, false, false, false),
            Some(Claim::Never)
        );
        assert_eq!(
            claim_for(Code::Escape, false, true, false),
            Some(Claim::Always)
        );
    }

    #[test]
    fn ctrl_chords_are_claimed_so_the_page_cannot_act_twice() {
        // Chrome-owned commands: the page must not also act on them.
        for code in [
            Code::KeyL,
            Code::KeyT,
            Code::KeyW,
            Code::KeyF,
            Code::KeyH,
            Code::KeyJ,
            Code::KeyR,
            Code::KeyP,
            Code::KeyD,
            Code::Tab,
        ] {
            assert_eq!(
                claim_for(code, true, false, false),
                Some(Claim::Always),
                "{code:?}"
            );
        }
        // Ferrite commands that a design canvas may also own. On an ordinary
        // page they are claimed; on a canvas/editable surface they are inert.
        for code in [Code::KeyB, Code::KeyK] {
            assert_eq!(
                claim_for(code, true, false, false),
                Some(Claim::OnlyOutsideSiteSurface),
                "{code:?}"
            );
        }
        assert_eq!(
            claim_for(Code::KeyO, true, true, false),
            Some(Claim::OnlyOutsideSiteSurface),
            "Ctrl+Shift+O: Figma outline stroke, otherwise bookmarks"
        );
        assert_eq!(
            claim_for(Code::KeyR, true, true, false),
            Some(Claim::Always),
            "Ctrl+Shift+R is Chrome's hard reload"
        );
    }

    #[test]
    fn function_keys_are_bare_and_do_not_steal_system_chords() {
        // F4/F6/F10/F11/F12 are bare function keys. `Req::Any` here used to
        // make Alt+F4 cycle the theme (and Ctrl+F11 toggle fullscreen) instead
        // of leaving the system/OS chord alone; F3 with Ctrl is not find-next
        // either. The reporter must not claim any of them with a modifier
        // (Shift+F3 is the one legitimate F3 variant; Ctrl+F5 is hard reload).
        for code in [Code::F4, Code::F10, Code::F11, Code::F12] {
            for (primary, shift, alt) in [
                (true, false, false),
                (false, true, false),
                (false, false, true),
                (true, false, true),
            ] {
                assert_eq!(
                    claim_for(code, primary, shift, alt),
                    None,
                    "{code:?} must be bare (primary={primary} shift={shift} alt={alt})"
                );
            }
        }
        for (primary, alt) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                claim_for(Code::F3, primary, false, alt),
                None,
                "F3 must be bare or Shift+F3 (primary={primary} alt={alt})"
            );
        }
        for (primary, alt) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                claim_for(Code::F6, primary, false, alt),
                None,
                "F6 must be bare or Shift+F6 (primary={primary} alt={alt})"
            );
        }
        assert_eq!(
            claim_for(Code::F3, false, true, false),
            Some(Claim::Always),
            "Shift+F3 is find-previous"
        );
        assert_eq!(
            pressed(Code::F4, false, false, true),
            None,
            "Alt+F4 belongs to the OS, not the theme cycler"
        );
        assert_eq!(
            pressed(Code::F11, false, false, false),
            Some(Action::ToggleFullscreen)
        );
        assert_eq!(
            pressed(Code::F12, false, false, false),
            Some(Action::OpenDevtools)
        );
        assert_eq!(
            pressed(Code::F4, false, false, false),
            Some(Action::CycleThemePack)
        );
        assert_eq!(
            pressed(Code::F6, false, false, false),
            Some(Action::FocusOmnibox)
        );
        assert_eq!(
            pressed(Code::F6, false, true, false),
            Some(Action::FocusContent),
            "Shift+F6 returns to the page"
        );
        assert_eq!(
            pressed(Code::F10, false, false, false),
            Some(Action::OpenMenu)
        );
        assert_eq!(
            pressed(Code::F3, false, false, false),
            Some(Action::FindNextOrOpen)
        );
    }

    #[test]
    fn f5_reloads_and_ctrl_f5_hard_reloads() {
        // WebView2 handled F5 natively before Task 24 disabled engine
        // accelerators. Without this row the key is visible to the page and
        // does nothing, which is not a browser. Ctrl+F5 is Chrome's hard
        // reload, so it must not fall through to the page either.
        assert_eq!(
            pressed(Code::F5, false, false, false),
            Some(Action::ReloadOrStop)
        );
        assert_eq!(
            claim_for(Code::F5, false, false, false),
            Some(Claim::Always)
        );
        assert_eq!(
            pressed(Code::F5, true, false, false),
            Some(Action::HardReload)
        );
        assert_eq!(claim_for(Code::F5, true, false, false), Some(Claim::Always));
        assert_eq!(pressed(Code::F5, false, true, false), None);
    }

    #[test]
    fn chrome_parity_chords_are_wired() {
        // The browser commands a Chrome user reaches for, with the action each
        // must produce. This is the regression net for the missing-shortcut
        // audit: a row can be added to the table but silently point at the
        // wrong action, and this catches it.
        assert_eq!(
            pressed(Code::KeyH, true, false, false),
            Some(Action::OpenHistory)
        );
        assert_eq!(
            pressed(Code::KeyJ, true, false, false),
            Some(Action::OpenDownloads)
        );
        assert_eq!(
            pressed(Code::KeyO, true, true, false),
            Some(Action::OpenBookmarks)
        );
        assert_eq!(
            pressed(Code::KeyR, true, true, false),
            Some(Action::HardReload)
        );
        assert_eq!(
            pressed(Code::KeyD, false, false, true),
            Some(Action::FocusOmnibox)
        );
        assert_eq!(
            pressed(Code::F6, false, false, false),
            Some(Action::FocusOmnibox)
        );
        assert_eq!(
            pressed(Code::F6, false, true, false),
            Some(Action::FocusContent)
        );
        assert_eq!(
            pressed(Code::KeyF, false, false, true),
            Some(Action::OpenMenu)
        );
        assert_eq!(
            pressed(Code::KeyE, false, false, true),
            Some(Action::OpenMenu)
        );
        assert_eq!(
            pressed(Code::F10, false, false, false),
            Some(Action::OpenMenu)
        );
    }

    #[test]
    fn zoom_accepts_the_shifted_plus_and_minus() {
        // Ctrl+'+' is Ctrl+Shift+Equal on every layout that has a plus, and it
        // is the chord users press. The old arm required !shift and so the
        // documented "Ctrl+±" zoom-in was unreachable.
        assert_eq!(
            pressed(Code::Equal, true, true, false),
            Some(Action::ZoomIn)
        );
        assert_eq!(
            pressed(Code::Equal, true, false, false),
            Some(Action::ZoomIn)
        );
        assert_eq!(
            pressed(Code::Minus, true, true, false),
            Some(Action::ZoomOut)
        );
        // The numeric keypad has stable `code` values and was handled by
        // WebView2 before Task 24. Disabling engine accelerators must not
        // orphan it.
        assert_eq!(
            pressed(Code::NumpadAdd, true, false, false),
            Some(Action::ZoomIn)
        );
        assert_eq!(
            pressed(Code::NumpadAdd, true, true, false),
            Some(Action::ZoomIn)
        );
        assert_eq!(
            pressed(Code::NumpadSubtract, true, false, false),
            Some(Action::ZoomOut)
        );
        assert_eq!(pressed(Code::NumpadAdd, false, false, false), None);
        assert_eq!(
            pressed(Code::Digit0, true, false, false),
            Some(Action::ZoomReset)
        );
        assert_eq!(pressed(Code::Digit0, true, true, false), None);
    }

    #[test]
    fn tab_digits_cover_one_through_nine() {
        for (index, code) in [
            Code::Digit1,
            Code::Digit2,
            Code::Digit3,
            Code::Digit4,
            Code::Digit5,
            Code::Digit6,
            Code::Digit7,
            Code::Digit8,
            Code::Digit9,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                pressed(code, true, false, false),
                Some(Action::SwitchTab(index as u8 + 1))
            );
        }
    }

    #[test]
    fn js_codes_round_trip() {
        for binding in BINDINGS {
            assert_eq!(
                Code::from_js(binding.code.js_code()),
                Some(binding.code),
                "{binding:?}"
            );
            assert_eq!(
                Code::from_winit(winit_code(binding.code)),
                Some(binding.code),
                "{binding:?}"
            );
        }
        assert_eq!(Code::from_js("KeyZ"), None);
        assert_eq!(Code::from_js(""), None);
        assert_eq!(Code::from_winit(KeyCode::KeyZ), None);
    }

    /// The winit key each table row must map to. Spelled out separately so a
    /// typo in `Code::from_winit` cannot hide behind the same table.
    fn winit_code(code: Code) -> KeyCode {
        match code {
            Code::KeyA => KeyCode::KeyA,
            Code::KeyB => KeyCode::KeyB,
            Code::KeyD => KeyCode::KeyD,
            Code::KeyE => KeyCode::KeyE,
            Code::KeyF => KeyCode::KeyF,
            Code::KeyH => KeyCode::KeyH,
            Code::KeyI => KeyCode::KeyI,
            Code::KeyJ => KeyCode::KeyJ,
            Code::KeyK => KeyCode::KeyK,
            Code::KeyL => KeyCode::KeyL,
            Code::KeyN => KeyCode::KeyN,
            Code::KeyO => KeyCode::KeyO,
            Code::KeyP => KeyCode::KeyP,
            Code::KeyR => KeyCode::KeyR,
            Code::KeyT => KeyCode::KeyT,
            Code::KeyW => KeyCode::KeyW,
            Code::Digit0 => KeyCode::Digit0,
            Code::Digit1 => KeyCode::Digit1,
            Code::Digit2 => KeyCode::Digit2,
            Code::Digit3 => KeyCode::Digit3,
            Code::Digit4 => KeyCode::Digit4,
            Code::Digit5 => KeyCode::Digit5,
            Code::Digit6 => KeyCode::Digit6,
            Code::Digit7 => KeyCode::Digit7,
            Code::Digit8 => KeyCode::Digit8,
            Code::Digit9 => KeyCode::Digit9,
            Code::Equal => KeyCode::Equal,
            Code::Minus => KeyCode::Minus,
            Code::NumpadAdd => KeyCode::NumpadAdd,
            Code::NumpadSubtract => KeyCode::NumpadSubtract,
            Code::Escape => KeyCode::Escape,
            Code::Tab => KeyCode::Tab,
            Code::F3 => KeyCode::F3,
            Code::F4 => KeyCode::F4,
            Code::F5 => KeyCode::F5,
            Code::F6 => KeyCode::F6,
            Code::F10 => KeyCode::F10,
            Code::F11 => KeyCode::F11,
            Code::F12 => KeyCode::F12,
            Code::ArrowLeft => KeyCode::ArrowLeft,
            Code::ArrowRight => KeyCode::ArrowRight,
        }
    }

    #[test]
    fn the_reporter_is_generated_from_the_table() {
        let js = reporter_js("deadbeef");
        // Every binding appears, with its own claim code and no others.
        let rows = js.lines().filter(|l| l.contains("e.code===")).count();
        assert_eq!(rows, BINDINGS.len(), "generated rows must equal table rows");
        for binding in BINDINGS {
            let expect = format!("e.code==='{}'", binding.code.js_code());
            assert!(js.contains(&expect), "missing {binding:?}");
        }
        let claim_lines = |n: u8| {
            let needle = format!("return {n};");
            js.lines().filter(|l| l.ends_with(&needle)).count()
        };
        assert_eq!(
            claim_lines(1),
            BINDINGS.iter().filter(|b| b.claim == Claim::Always).count()
        );
        assert_eq!(
            claim_lines(2),
            BINDINGS
                .iter()
                .filter(|b| b.claim == Claim::OnlyOutsideSiteSurface)
                .count()
        );
        assert_eq!(
            claim_lines(3),
            BINDINGS.iter().filter(|b| b.claim == Claim::Never).count()
        );
    }

    #[test]
    fn the_reporter_carries_the_token_and_the_guards() {
        let js = reporter_js("abc123");
        assert!(js.contains("var token='abc123';"));
        // Fail closed on synthetic events, IME commits, and a second injection.
        assert!(js.contains("if(e.isTrusted!==true)return;"));
        assert!(js.contains("if(e.isComposing)return;"));
        assert!(js.contains("if(window.__ferriteShortcuts)return;"));
        // Every frame: a key event never crosses a document boundary, so the
        // focused subframe is the only reporter for a key pressed there. The
        // old top-frame guard made shortcuts dead inside an embed.
        assert!(
            !js.contains("window.top!==window.self"),
            "the reporter must run in subframes too"
        );
        // The sink and JSON are captured at install, before page script. wry's
        // window.ipc resolves chrome.webview.postMessage at call time, so a
        // page that wraps it later could otherwise read this token from the
        // reporter's own report and forge shortcuts.
        assert!(js.contains("var stringify=JSON.stringify;"));
        assert!(js.contains("var post=null;"));
        assert!(js.contains("post=wv.postMessage.bind(wv);"));
        assert!(js.contains("post(stringify({"));
        // The site-surface check must look through a shadow boundary: at
        // window level e.target is retargeted to the host, which hid the real
        // input.
        assert!(js.contains("e.composedPath&&e.composedPath()[0]"));
        // The check exists and gates exactly the claim-unless-site-surface
        // path; text-entry inputs, canvases, videos, application regions and
        // a large page canvas are site-owned.
        assert!(js.contains("n.tagName==='TEXTAREA'"));
        assert!(js.contains("n.tagName==='SELECT'"));
        assert!(js.contains("n.tagName==='INPUT'"));
        assert!(js.contains("t!=='checkbox'"));
        assert!(js.contains("n.isContentEditable"));
        assert!(js.contains("n.tagName==='CANVAS'"));
        assert!(js.contains("n.tagName==='VIDEO'"));
        assert!(js.contains("getAttribute('role')==='application'"));
        assert!(js.contains("querySelectorAll('canvas')"));
        assert!(js.contains("cs.pointerEvents==='none'"));
        assert!(js.contains("cs.visibility==='hidden'"));
        assert!(js.contains("cs.display==='none'"));
        assert!(js.contains("r.width*r.height>=vw*vh*0.5"));
        // Claim 1 swallows, claim 3 does not, and claim 2 is inert on a
        // site-owned surface.
        assert!(js.contains("if(c===2&&siteSurface(target))return;"));
        assert!(js.contains("if(c!==3){"));
        // Capture phase, so a page's own handler cannot run first.
        assert!(js.contains("},true);"));
        // The report shape the shell parses.
        assert!(js.contains("type:'shortcut'"));
        assert!(js.contains("token:token"));
        assert!(js.contains("code:e.code"));
        assert!(js.contains("repeat:!!e.repeat"));
    }

    #[test]
    fn site_owned_keys_are_inert_in_the_reporter() {
        // Claim 2 must return *before* the report is built: the page owns the
        // keystroke, so the shell must not receive it (otherwise Ctrl+B in
        // Figma's text editor toggles Ferrite's panel). Order matters, so this
        // asserts the guard precedes the post rather than trusting the branch
        // shape.
        let js = reporter_js("abc123");
        let guard = js
            .find("if(c===2&&siteSurface(target))return;")
            .expect("claim 2 must be gated on the site-surface check");
        let post = js
            .find("post(stringify(")
            .expect("the reporter must post its report");
        assert!(guard < post, "the site-surface guard must precede the post");
        // The exact set of claim-2 rows is deliberate. If another binding
        // moves into claim 2, this test is the place that says so.
        let claim_2: Vec<(Code, Req, Req, Req, Action)> = BINDINGS
            .iter()
            .filter(|b| b.claim == Claim::OnlyOutsideSiteSurface)
            .map(|b| (b.code, b.primary, b.shift, b.alt, b.action))
            .collect();
        assert_eq!(
            claim_2,
            vec![
                (Code::KeyF, Req::On, Req::On, Req::Off, Action::LinkHints),
                (
                    Code::KeyO,
                    Req::On,
                    Req::On,
                    Req::Off,
                    Action::OpenBookmarks
                ),
                (Code::KeyB, Req::On, Req::Off, Req::Off, Action::TogglePanel),
                (
                    Code::KeyK,
                    Req::On,
                    Req::Off,
                    Req::Off,
                    Action::ToggleTabSearch
                ),
            ]
        );
    }

    #[test]
    fn the_reporter_never_edits_settings_or_browser_state_inline() {
        // The reporter decides *claim*; the shell decides *action*. No action
        // name may leak into the script, or the two lists have started to fork.
        let js = reporter_js("abc123");
        for forbidden in [
            "ToggleTabSearch",
            "FocusOmnibox",
            "OpenDevtools",
            "NewTab",
            "CloseTab",
            "LinkHints",
            "OpenHistory",
            "OpenDownloads",
            "OpenBookmarks",
            "OpenMenu",
            "HardReload",
        ] {
            assert!(
                !js.contains(forbidden),
                "{forbidden} leaked into the reporter"
            );
        }
    }

    #[test]
    fn a_report_with_the_wrong_token_is_rejected() {
        let id = Uuid::new_v4();
        let good = token_for(id);
        assert!(verify_token(id, Some(&good)));
        assert!(!verify_token(id, None));
        assert!(!verify_token(id, Some("")));
        assert!(!verify_token(id, Some(&good[..good.len() - 1])));
        assert!(!verify_token(id, Some(&format!("{good}0"))));
        // Another webview's token is not this one's (that is the whole point:
        // a page can read its own token only by being its own reporter).
        let mut other = good.into_bytes();
        other[0] = if other[0] == b'a' { b'b' } else { b'a' };
        assert!(!verify_token(id, Some(&String::from_utf8(other).unwrap())));
    }

    #[test]
    fn tokens_are_per_webview_and_stable_in_process() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_eq!(token_for(a), token_for(a));
        assert_ne!(token_for(a), token_for(b));
        assert_eq!(token_for(a).len(), 64);
    }

    #[test]
    fn reports_resolve_to_actions_and_junk_does_not() {
        assert_eq!(
            action_for_report("KeyL", true, false, false),
            Some(Action::FocusOmnibox)
        );
        assert_eq!(
            action_for_report("KeyT", true, true, false),
            Some(Action::ReopenTab)
        );
        assert_eq!(action_for_report("KeyZ", true, false, false), None);
        assert_eq!(action_for_report("KeyL", false, false, false), None);
        assert_eq!(action_for_report("", true, false, false), None);
        assert_eq!(action_for_report("KeyL", true, false, true), None);
    }
}
