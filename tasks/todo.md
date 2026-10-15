# Todo: Ferrite v0.1.1 (tracks tasks/plan.md)

## Phase 0: Diagnose

### Task 1: Click-moment logs for the dead toolbar
**Description:** Get the discriminating runtime evidence no static analysis can
provide: console lines produced by clicking dead toolbar buttons, plus whether
web-page links respond to clicks.
**Acceptance criteria:**
- [ ] Console lines from 2–3 toolbar clicks pasted by the user
- [ ] Web-link clickability answered (dead everywhere vs dead toolbar only)
**Verification:**
- [ ] Log lines classify as refusal (`chrome envelope rejected`) or silence
**Dependencies:** None (needs the human)
**Files likely touched:** none
**Estimated scope:** XS

### Task 2: Classify the theme report
**Description:** Determine whether the unchanged old tab was a website (correct
behavior — sites keep their styling) or a Ferrite page (live-push failure).
**Acceptance criteria:**
- [x] Tab kind recorded — ANSWER: a Ferrite page (not a website). Live-push
  failure is real; folded into Task 3 scope.
- [ ] If Ferrite page: becomes a Phase-1 fix task
**Verification:**
- [ ] Matches `apply_pack_live` routing logic either way
**Dependencies:** None (needs the human)
**Files likely touched:** none (or shell.rs live-push path)
**Estimated scope:** XS

## Checkpoint: Diagnosis
- [ ] Both answers recorded; button cause narrowed to delivery vs refusal

## Phase 1: P0 — dead toolbar

### Task 3: Fix per Phase-0 evidence (toolbar input + theme live-push)
**Description:** Implement the fix the evidence points at (delivery path or
refusal path for the toolbar), plus the confirmed theme live-push failure on
existing Ferrite pages (new tabs already work via the factory block, so the
gap is strictly the live `evaluate_script` pushes to open pages and chrome).
Not both toolbar theories, not a guess.
**Acceptance criteria:**
- [ ] Toolbar buttons and dropdowns respond on the user's real install
- [ ] No regression in content-page input or keyboard paths
**Verification:**
- [ ] Before/after click-moment logs from the user
- [ ] `cargo test -p ferrite-shell`, clippy clean
**Dependencies:** Task 1
**Files likely touched:** `crates/ferrite-shell/src/shell.rs`, possibly
`crates/ferrite-chrome/assets/chrome.html`, `crates/ferrite-tabs/src/webview.rs`
**Estimated scope:** M

### Task 4: Regression net for the toolbar fix
**Description:** Whatever broke gets a standing guard (unit/integration test,
plus a CI assertion where the failure class allows it).
**Acceptance criteria:**
- [ ] A test fails if the fix is reverted
- [ ] CI runs it on every push
**Verification:**
- [ ] Revert-fix locally, watch the test go red, restore
**Dependencies:** Task 3
**Files likely touched:** test modules adjacent to the fix
**Estimated scope:** S

## Checkpoint: Usability
- [ ] Toolbar verified working by user on real install; theme behavior confirmed

## Phase 2: P1 — v0.1.1 ship vehicle

### Task 5: Answer the pdf-renderer shipping question
**Description:** Inspect each artifact (MSI file table, deb contents, dmg,
tarball) for the `ferrite-pdf-renderer` child binary and pdfium; fix packaging
so all ship it, or document the exception with a reason.
**Acceptance criteria:**
- [ ] Per-artifact yes/no table recorded
- [ ] Gaps closed in packaging config, or exception documented
**Verification:**
- [ ] CI asserts child binary presence in each packaged artifact
**Dependencies:** None
**Files likely touched:** `crates/ferrite-browser/wix/main.wxs`,
`crates/ferrite-browser/Cargo.toml` (deb metadata), `.github/workflows/release.yml`
**Estimated scope:** M

### Task 6: CI installer self-proof
**Description:** Each OS job installs its own artifact and asserts the
user-visible contract instead of assuming the packager did it.
**Acceptance criteria:**
- [ ] Windows: silent-install MSI, assert Start Menu entry, embedded exe icon,
  `ferrite --version` runs
- [ ] Linux/macOS: assert package contents + icon presence
- [ ] Any miss fails the run before publish
**Verification:**
- [ ] Deliberately break one assertion locally/CI, watch it fail
**Dependencies:** Task 5 (needs to know what must be present)
**Files likely touched:** `.github/workflows/release.yml`
**Estimated scope:** M

### Task 7: Pre-upload asset-name uniqueness assertion
**Description:** One script, run before `softprops` upload, failing the run on
any duplicate asset basename across the publish set. Kills the SBOM/DMG
collision class permanently.
**Acceptance criteria:**
- [ ] Duplicated fixture names fail the script; unique set passes
- [ ] Runs before every publish, blocks upload on failure
**Verification:**
- [ ] Unit-test the script logic or fixture-test it in CI
**Dependencies:** None
**Files likely touched:** `.github/workflows/release.yml`
**Estimated scope:** S

### Task 8: Cut v0.1.1
**Description:** Bump workspace version + store baselines, tag `v0.1.1`, watch
the full run green, merge the manifests PR, audit the asset list against the
expected manifest.
**Acceptance criteria:**
- [ ] Tag `v0.1.1` on main, all 4 builds + publish green
- [ ] Release page holds exactly the expected set, no strays
- [ ] Manifests PR merged
**Verification:**
- [ ] Asset list diffed against expected manifest by hand once
- [ ] Download buttons fetch real files (Task 9)
**Dependencies:** Tasks 3, 4, 5, 6, 7
**Files likely touched:** `Cargo.toml` (workspace version), `homebrew/`,
`winget/`, tag only otherwise
**Estimated scope:** S (mostly watching + verifying)

### Task 9: Post-release site check
**Description:** Verify the client path end to end from the landing page with
the new version live.
**Acceptance criteria:**
- [ ] `latest.json` targets resolve to downloadable files with matching SHAs
- [ ] Rollout is 1
- [ ] Landing Download flow verified in a real browser
**Verification:**
- [ ] Fetch each URL, compare SHA against the manifest
**Dependencies:** Task 8
**Files likely touched:** none (verification only)
**Estimated scope:** XS

## Checkpoint: Release
- [ ] v0.1.1 fresh-install check on Windows: no console, real icon, Start Menu
  entry, working toolbar, correct theme behavior

## Phase 3: P1 — trust chain

### Task 10: Provision the update-signing seed
**Description:** Generate `FERRITE_RELEASE_SIGNING_SEED`, store as a repo
secret, verify the next manifest carries a signature; document rotation.
**Acceptance criteria:**
- [ ] Secret stored; signed manifest observed on the next release
- [ ] Rotation procedure written down (INTERNAL or docs)
**Verification:**
- [ ] Client-side signature check passes against the published manifest
**Dependencies:** Task 8 (needs a release to observe)
**Files likely touched:** repo settings (human) + docs note
**Estimated scope:** S

### Task 11: Rollout discipline
**Description:** Define the criteria and decider for raising rollout past 1.
**Acceptance criteria:**
- [ ] Written rule (what "crash-free hours" means, who raises)
- [ ] First raise only after the rule is met on v0.1.1
**Verification:**
- [ ] Rule recorded in `tasks/plan.md` or docs
**Dependencies:** Task 8
**Files likely touched:** docs note
**Estimated scope:** XS

### Task 12: OS code-signing certificates — DECIDED: no spending, closed unfunded.
### (Task 13 is the covering deliverable.)
**Description:** Price, identity-validation steps, and reputation notes for
Windows + Apple certs. Blocked on funding by design; blocks nothing.
**Acceptance criteria:**
- [ ] Cost + process documented per OS
- [ ] Explicit funded/unfunded decision recorded
**Verification:**
- [ ] Decision recorded; Task 13 covers the unfunded path
**Dependencies:** None (human decision)
**Files likely touched:** docs note
**Estimated scope:** XS

### Task 13: Download-page expectation notice
**Description:** Honest SmartScreen/Gatekeeper wording on the landing page:
what the warning looks like, the 3-click bypass, checksum link. Kills the #1
support question before it is asked.
**Acceptance criteria:**
- [ ] Notice live next to every download button
- [ ] Wording verified accurate on a real Windows machine
**Verification:**
- [ ] Visual check of the landing page
**Dependencies:** None
**Files likely touched:** `docs/index.html`
**Estimated scope:** S

## Checkpoint: Trust
- [ ] Signed manifest verified by client code path; warning notice live

## Phase 4: P2 — hygiene + backlog

### Task 14: Threat-db test droppings to temp dirs
**Description:** Replace the gitignore band-aid with the real fix: tests write
working DBs to temp dirs, never the crate dir.
**Acceptance criteria:**
- [ ] No `ferrite-threat.db*` appears in the tree after a full test run
- [ ] Test asserting no repo writes (or equivalent guard)
**Verification:**
- [ ] `cargo test -p ferrite-privacy` then `git status` clean of droppings
**Dependencies:** None
**Files likely touched:** `crates/ferrite-privacy` test helpers
**Estimated scope:** S–M

### Task 15: Spellcheck dictionaries gap — RECON DONE
**Finding:** `assets/dicts/` holds only the README; the intended pipeline step
("fetch once at build/provision time, verify, then ship") was never built, so
no installer seeds the runtime dir and the proxy block kills the fallback
fetch. User strings promising "bundled" describe intent, not reality.
**Fix shape:** provision en-US.bdic (pinned hash), ship in all installers, seed
runtime dir on first boot, test asserts boot `dicts>0`. Language set default:
en-US only unless asked otherwise.
**Acceptance criteria:**
- [ ] Dictionaries present at boot, or docs no longer promise them
- [ ] Standing assertion matches the promise
**Verification:**
- [ ] Boot log shows the promised state
**Dependencies:** None
**Files likely touched:** bundling config or `docs/ENGINE.md`
**Estimated scope:** S

### Task 16: First-run device-key failure — RECON DONE, needs user data
**Finding:** `resolve_master_key` (load → generate → store) degrades honestly
to non-durable, and the app notifies. Prime suspect on stock Windows: UAC
elevation split (per-machine install run as admin vs user credential store),
or a genuine first-run store failure. Cannot distinguish without the full boot
log (need the `keyring store failed` / generate lines around the autosave
error). Fix follows the evidence.
**Acceptance criteria:**
- [ ] Severity answered (cosmetic warning vs sessions never saved)
- [ ] Fix or documented limitation
**Verification:**
- [ ] Restart browser, session state survives (or documented why not)
**Dependencies:** None
**Files likely touched:** session/device-key path in `ferrite-shell`
**Estimated scope:** M

### Task 17: Internal-name upstream dials
**Description:** Proxy attempts real DNS for `ferrite.*` internal names before
the custom-protocol path answers: short-circuit + test.
**Acceptance criteria:**
- [ ] No `upstream connect failed ... ferrite.*` lines on clean boot
- [ ] Internal pages still resolve through the intended path
**Verification:**
- [ ] Before/after boot logs compared locally
**Dependencies:** None
**Files likely touched:** proxy/custom-protocol routing
**Estimated scope:** S

### Task 18: Signed-path dmg flaw
**Description:** `ditto -k` writes a zip, `stapler` needs a real image —
dormant until Apple secrets exist. Fix now while cheap; verify whatever CI can
without credentials.
**Acceptance criteria:**
- [ ] Signed path produces a real (staplable-shape) dmg
- [ ] Unsigned path byte-identical behavior to today
**Verification:**
- [ ] CI mac build green; `hdiutil`-shape reasoning documented
**Dependencies:** None
**Files likely touched:** `.github/workflows/release.yml` sign step
**Estimated scope:** S

### Task 19: Dependabot backlog (8 PRs)
**Description:** Merge the 8 pending bumps; breaking majors first with compat
proof (getrandom 0.2→0.4, ed25519-dalek 3.0), each with tests + credits regen.
**Acceptance criteria:**
- [ ] All 8 merged, CI green throughout
- [ ] Credits projection regenerated + scrubbed per lockfile change
**Verification:**
- [ ] `cargo deny`, full test suite, credits guard green after each
**Dependencies:** None (sequence among themselves: majors first, one per commit)
**Files likely touched:** `Cargo.toml`/`Cargo.lock`, credits snapshot
**Estimated scope:** M (spread across commits)

### Task 20: mac .icns + Linux .desktop icon
**Description:** Generate platform icons where the tooling exists (`.icns` via
`iconutil` on the mac CI job; `.desktop` icon path on Linux), wire into bundle
metadata, verify by artifact inspection.
**Acceptance criteria:**
- [ ] `.app` shows the Ferrite mark; `.deb` ships an icon
- [ ] CI asserts presence
**Verification:**
- [ ] Artifact inspection from CI logs/downloads
**Dependencies:** None
**Files likely touched:** release workflow, bundle metadata
**Estimated scope:** M

## Checkpoint: Complete
- [ ] All acceptance criteria met; zero known issues without an owning task
