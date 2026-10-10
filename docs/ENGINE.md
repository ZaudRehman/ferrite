# Ferrite Engine Layer

Three layers, three different owners. Name them, because feasibility
questions in this project keep hinging on *which* layer a capability
lives in, not just which platform.

- **Layer 1: the engine.** Blink (WebView2/Windows), WebKit (WKWebView/macOS),
  WebKit (WebKitGTK/Linux). Maintained by Google, Apple, and the WebKitGTK
  project. Ferrite never touches this code.
- **Layer 2: `wry` 0.56 (`winit` 0.30).** The Rust mediation crate with its
  **own capability surface, version history, and gaps**, distinct from what
  the engine can do. A feature can exist in Layer 1 yet be unreachable
  through the pinned Layer 2 version (this is exactly what happened with
  cookie access: every engine exposes cookies; the question was whether
  *wry* surfaced them).
- **Layer 3: Ferrite's Rust code.** IPC patterns, trust boundaries, and
  every feature above the webview.

Rule: on every wry/SDK bump, re-check this file's matrix before assuming
parity. Gaps are rediscovered through design conversations or production
bugs when this file goes stale.

## Pinned versions (Cargo.lock)

| Crate | Version | Notes |
|---|---|---|
| wry | 0.56.1 | `devtools` feature on (engine inspector, no CDP port) |
| webview2-com / -sys | 0.38.2 | Windows only: dialogs, loader queries |
| windows | 0.61.3 | Windows only: ShareUI, COM, ShellExecute |
| objc2 stack | 0.6.x / app-kit+foundation 0.3.2 | macOS only: share sheet, openURL |
| winit | 0.30.13 | All platforms |
| gtk | 0.18.2 | Linux only: Wayland build path (compile-covered, behaviorally untested) |

## Capability matrix

Status: **confirmed** (verified in-tree or against vendored sources) or
**unverified** (assumed from vendor docs, confirm on the target before
promising it). Linux cells default to unverified; that is the honest
standing position for WebKitGTK in this project, not a gap in this table.

| Feature | Windows (WebView2) | macOS (WKWebView) | Linux (WebKitGTK) |
|---|---|---|---|
| Video PiP (`requestPictureInPicture`) | confirmed (Chromium) | confirmed (`allowsPictureInPictureMediaPlayback`, default on) | unverified |
| Document PiP | confirmed (Chromium-only) | not available | not available |
| `GetAvailableCoreWebView2BrowserVersionString` (launch gate) | confirmed (webview2-com-sys 0.38.2) | n/a | n/a |
| Native share sheet | confirmed (`IDataTransferManagerInterop`, unpackaged Win32) | confirmed (`NSSharingServicePicker`) | none exists (fallback page) |
| Script-dialog interception | confirmed (`ScriptDialogOpening`) | not exposed by wry | not exposed by wry |
| Custom `ferrite://` protocol | confirmed | confirmed | confirmed |
| Engine inspector | confirmed (WebView2 DevTools) | confirmed (WebKit inspector) | confirmed |
| Cookie access (jar) | confirmed via wry 0.56 surface | confirmed via wry 0.56 surface | confirmed via wry 0.56 surface |
| Inline spellcheck (red squiggle) | confirmed (Chromium Hunspell; dictionary CDN fetches blocked at the Ferrite proxy, bundled `.bdic` instead) | confirmed (native `NSSpellChecker`, no download involved) | confirmed (Chromium Hunspell; same proxy block + bundle as Windows) |

## OS floors

- **Windows:** 10 1809+ (WebView2 requirement). The Evergreen Runtime ships
  with Windows 11 and most managed Windows 10 fleets, but not all. Ferrite
  checks at startup (`webview2_runtime_version`, before the first WebView2
  instance) and exits with the installer address when the runtime is absent.
- **macOS:** 13+ baseline. Per-feature floors that force it: `isInspectable`
  needs 13.3+, PiP configuration needs Mac Catalyst 13.1+. Do not advertise
  an older floor without re-auditing these cells.
- **Linux:** X11 primary; Wayland via `WRY_WAYLAND` is compile-covered in CI
  (`ferrite-tabs --features gtk` clippy) but behaviorally untested: no
  display in runners (see `docs/LIMITATIONS.md`).

## Runtime distribution (Windows)

**Evergreen is the default, deliberately:** it auto-updates with security
patches and has the smaller footprint. Fixed Version (pinned runtime) is
only for strict-compatibility requirements, which Ferrite does not have:
a privacy browser must track engine security patches, not freeze them.
Obligation that comes with Evergreen: the runtime updates out from under
the pinned SDK, so use feature detection for newer APIs and run the
`release.yml` matrix (which builds all three targets) on every bump.

## Security posture (inherited, not authored)

Sandboxing quality, site isolation, and CVE patch cadence are a function of
the Layer-1 maintainer: Chromium's security team, Apple's WebKit team, or
the WebKitGTK project. On pure resourcing grounds WebKitGTK is the weakest
of the three, the actual security ceiling on Linux. This is not an argument
against wrapped-native engines (a bespoke engine would be dramatically
worse); it is a bound on what "most secure" can honestly promise per
platform. Threat-model and marketing copy must respect it.

## Engine health in-product

`ferrite://about` shows the running engine version plus the runtime update
link (Evergreen installer on Windows, system updates elsewhere). Engine
patches arrive outside Ferrite's own updates. The page turns a displayed
fact into the action.
