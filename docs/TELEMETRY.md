# Telemetry: deliberate absence

Ferrite ships no usage collection. Not "off by default": the code path
does not exist, and `deny.toml` fails the build if a known analytics or
crash-reporting SDK is ever added as a dependency. Anyone auditing the
source verifies the claim by absence, not by trusting a toggle.

That is a deliberate trade with a real cost: without usage data, feature
prioritization and regression detection fly blind (no idea whether Reader
Mode serves 2% or 40% of sessions, no signal when a release regresses a
meaningful share of users). The project accepts that cost because the
strength of the "we don't" claim matters more here than the planning
convenience it would buy.

## The complete egress inventory

Background network exists, and each path is enumerated here with its
payload discipline. Nothing else in the binary opens an outbound
connection on its own behalf:

1. **Filter / threat list refresh** (hourly, `ferrite-shell` →
   `ferrite-privacy::updater::fetch_direct` → `ferrite-proxy::direct_get`):
   bare `GET`, `Host` + `Connection: close` only. No user agent (no
   fingerprint), no cookies, no query. Source is an operator env URL or a
   local file; absent source keeps last-known-good. The server necessarily
   sees IP + SNI (inherent to TLS, not Ferrite's choice to log or not).
2. **Update manifest + artifact fetch** (hourly + About-page demand,
   `ferrite-sync::updater` via ureq): bare manifest `GET`, then artifact
   bytes only after Ed25519 + sha256 gates pass. No cookies (fresh agent
   per call), no query, no version ping beyond the URL itself.
3. **Realtime SafeBrowsing lookup**: strict opt-in, default off (pinned by
   `realtime_default_off`), 300ms fail-open budget, hex prefix only.
4. **Remote omnibox suggestions**: strict opt-in, default off; each
   keystroke goes to DuckDuckGo when on, ranked below local results.
5. **Spellcheck dictionary provisioning** (once per missing dictionary:
   first tick for the active languages, on demand when the user adds a
   language in Settings; `ferrite-shell::dicts` via the strict direct
   client): bare `GET` of `{gstatic}/chrome/dict/{tag}.bdic`, 200-only,
   8MB cap, no cookies, no query, no user agent. Page-initiated
   dictionary fetches stay blocked at the proxy regardless (that block
   is about per-page fingerprinting; this fetch carries no page
   context). Failures are fail-soft and logged, never retried in a loop.

Everything else that touches the network is either the user's own
browsing (through the proxy, under their control) or OS DNS in the
clear (see the DoH gap in `docs/LIMITATIONS.md`).

## Consent-gated signals (the only data that ever leaves)

- **Breach check** (`ferrite-credentials::breach`): k-anonymity range
  query, 5-char hash prefix only, fired by an explicit password check,
  never backgrounded.
- **Diagnostics copy** (About page, restore page, crash flow): rendered
  locally, copied to the clipboard only when the user clicks. Never
  auto-submitted anywhere.
- **Crash log** (`ferrite-crash.log`): local file, version-tagged, capped
  at 64KB. Same copy-only rule.

The shared property: a person's specific action causes transmission,
never a background process deciding on their behalf.

## Identifiers: none persisted for correlation

- The rollout bucket (`ferrite-rollout-bucket`) is a local 0–99 draw,
  never transmitted (pinned by test), file holds a bare number.
- Sync device identities are random per-device UUIDs, not accounts, and
  never leave the sync paths. LAN sync is wired; the off-LAN relay
  client is wired too and syncs through operator-configured relay URLs.
  The relay routes by sender/recipient ID (delivery requires it) but
  cannot read payloads; a self-hosted relay keeps even the routing
  metadata with the operator.
- No installation ID, no session token, no cookie jar on Ferrite's own
  fetch paths.

## Bars for anything future

- **Usage stats**, if ever built: opt-in with plain-language copy, off by
  default, aggregate counters only (never click-streams), daily batches
  at most, no persistent identifier, differential-privacy /
  randomized-response noise per report. Anything less does not ship.
- **A/B experimentation**: same bar as usage stats. Silent cohort
  assignment with reporting back is usage collection under another name
  and gets no quieter review.
- **Dependency gate**: `deny.toml [bans]` lists analytics/crash SDKs;
  adding one is a red build until a human justifies it in review.

## Boundary (inherited, not chosen)

On Windows, WebView2 collects baseline diagnostics outside any app-level
setting (see the crash-reporting section in `docs/LIMITATIONS.md` and the
About page note). Ferrite's own code: genuinely absent. The platform's
own baseline: inherited, disclosed, outside Ferrite's control.
