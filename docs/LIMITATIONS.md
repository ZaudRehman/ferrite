# Ferrite Limitations

Honest disclosure of what Ferrite does not do. Each item states the
behavior; where a remedy exists, it says so. The engineering detail
behind these lives with the maintainers; nothing here is hidden via stub.

## Network

- DNS is unencrypted OS DNS unless you opt into encrypted DNS in Settings.
- No predictive pre-connect; connections open on demand.
- If the local proxy dies mid-session, tabs are not re-probed. Restart
  the browser.

## Sync

- Sync reaches devices on the local network, or anywhere through a
  self-hosted relay (Settings → Sync). There is no Ferrite-operated relay.
- Pairing needs an explicit code exchange plus your approval on each side.
- History syncs only if you opt in. Revoking a device cuts it at the next
  round.
- Discovery broadcasts a stable device ID on the LAN while sync runs.

## Updates

- Updates install only with a provisioned signing root; without one the
  updater refuses everything rather than installing unverified builds.

## Search

- Remote suggestions are DuckDuckGo-only and opt-in. With any other
  default engine, the toggle does nothing.

## Media

- No Widevine CDM: Widevine-gated content does not play (notably the
  Spotify web player). Most premium video also serves PlayReady, which
  plays normally, as does all unencrypted media. Per-service status is
  unverified, so the claim stops there. A blocked player names
  the cause.
- macOS and Linux DRM support is unconfirmed. No casting, no GPU toggle.

## Platform

- Windows 10 1809+ and Linux X11 are the working targets. macOS and
  Wayland paths are largely unverified.
- macOS and Linux show engine-default script dialogs; Windows shows
  Ferrite's own origin-attributed overlay.
- Single window only: no tab tear-off, no multi-window (profiles run as
  separate processes). No page screenshots, no live tab thumbnails.

## Printing

- Print dialog only. No silent printing, no Ferrite print settings panel.

## Appearance and accessibility

- No high-contrast mode, no font scaling, no screen-reader audit yet.
  Reduced-motion preferences are honored.

## Imports

- Payments, cookies, keychains, open tabs, Firefox compressed backups,
  and Safari binary plists are never imported; each refusal names the
  export to produce instead. Every import is an explicit click with
  exact counts back.

## Privacy boundaries (inherited, not chosen)

- WebView2 sends renderer crash minidumps to Microsoft; Ferrite cannot
  disable this (also stated on the About page). SmartScreen stays off;
  malware and phishing protection is Ferrite's own proxy stack.
- Push subscription endpoints are unverified and may resolve to
  third-party infrastructure; payloads stay end-to-end encrypted either way.
- Ferrite itself collects no usage data (see `TELEMETRY.md`).

## Crash reporting

- Ferrite's own crash log is local, version-tagged, capped, and
  URL-scrubbed, copied only when you click.
