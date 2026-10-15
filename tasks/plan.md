# Implementation Plan: Ferrite v0.1.1 — every reported problem, solved with proof

## Overview

This plan closes all 14 user-reported issues plus the latent defects found beside
them. Rule for every task: **a fix without automated prevention is a placeholder.**
Each task ships its regression net (test, CI gate, or guard) or it does not ship.
Task list lives in `tasks/todo.md`; human approval required before Phase 1 starts.

Phases run in dependency order. Phase 0 is diagnosis (unblocks P0). Phase 1 makes
the app usable. Phase 2 ships v0.1.1 carrying all queued fixes. Phases 3–4 harden
trust and clear backlog. Nothing here needs money except the signing certificates
(Phase 3, Task 12), which are explicitly fenced so they block nothing else.

## Architecture Decisions

- **Evidence before fix for the two open app bugs.** The toolbar bug gets its
  discriminating experiment (click-moment logs) before any code changes. The theme
  report gets classified (website tab vs Ferrite tab) before any code changes.
  Guessing at UI bugs produced this backlog; it stops here.
- **Installers are verified by installing.** A CI job on each OS installs the
  just-built artifact and asserts the user-visible contract (shortcut exists, icon
  present, binary runs `--version`). An installer that cannot prove itself in CI
  does not publish.
- **Release asset names are asserted unique pre-upload.** The entire collision
  class (SBOMs, DMGs) dies with one script that fails the run on duplicate
  basenames before `softprops` ever sees them, instead of relying on careful
  naming forever.
- **Version bump discipline.** v0.1.1 = workspace version + store baselines +
  fresh tag. No moving the v0.1.0 tag again; released bits are immutable.
- **Signing is fenced.** Auto-update signature and OS code signing are tracked
  as explicit tasks with dollar costs and no-money fallbacks (documented
  warnings), never as silent blockers of the release.

## Task List

### Phase 0: Diagnose (unblocks P0; human needed)

- [ ] Task 1: Capture click-moment logs for the dead toolbar (human: open
  installed v0.1.0, click 2–3 dead buttons, paste new console lines; plus: do
  links inside web pages respond to clicks?)
- [ ] Task 2: Classify the theme report (human: was the unchanged old tab a
  website or a Ferrite page when the theme was switched?)

### Checkpoint: Diagnosis
- [ ] Both answers recorded; button cause narrowed to delivery vs refusal

### Phase 1: P0 — dead toolbar (app is unusable without it)

- [ ] Task 3: Fix per Phase-0 evidence (delivery path or refusal path)
- [ ] Task 4: Regression net for the toolbar fix (test + CI gate; no reoccurrence
  without a red build)

### Checkpoint: Usability
- [ ] Toolbar verified working by user on real install; theme behavior confirmed
  correct-or-fixed for both tab kinds

### Phase 2: P1 — v0.1.1 ship vehicle (carries all queued fixes)

- [ ] Task 5: Answer the pdf-renderer shipping question (is the child binary +
  pdfium inside MSI/deb/dmg/tarball? inspect each artifact; fix packaging so the
  answer is yes everywhere or documented where no)
- [ ] Task 6: CI installer self-proof (Windows: silent-install MSI, assert Start
  Menu entry + embedded icon + `ferrite --version`; Linux/macOS: assert package
  contents + icon presence; fail run on any miss)
- [ ] Task 7: Pre-upload asset-name uniqueness assertion (fail fast on duplicate
  basenames across the whole publish set)
- [ ] Task 8: Cut v0.1.1 (bump workspace version + store baselines, tag, watch
  full run green, merge manifests PR, audit asset list vs expected manifest,
  confirm download buttons fetch real files)
- [ ] Task 9: Post-release site check (latest.json targets resolve, rollout
  stays 1, Download flow verified end to end from the landing page)

### Checkpoint: Release
- [ ] v0.1.1 installed from the site on a fresh Windows machine: no console
  window, real icon, Start Menu entry, working toolbar, correct theme behavior

### Phase 3: P1 — trust chain (works without money; money tracked explicitly)

- [ ] Task 10: Provision the update-signing seed (generate
  FERRITE_RELEASE_SIGNING_SEED, store as repo secret, verify next manifest
  carries a signature; document rotation)
- [ ] Task 11: Rollout discipline (criteria for raising 1 → wider; who decides;
  first raise only after crash-free hours on v0.1.1)
- [ ] Task 12: OS code-signing certificates (Windows + Apple: cost, identity
  validation steps, reputation notes; until funded, landing page carries an
  honest "Windows/macOS will ask once" notice — Task 13)
- [ ] Task 13: Download-page expectation notice (SmartScreen/Gatekeeper wording,
  3-click bypass, checksum link; kills the #1 support question before it is asked)

### Checkpoint: Trust
- [ ] Signed manifest verified by client code path; warning notice live

### Phase 4: P2 — hygiene + backlog (no open loops left)

- [ ] Task 14: Threat-db test droppings → temp dirs (proper fix replacing the
  gitignore band-aid; test asserting no repo writes)
- [ ] Task 15: Spellcheck dictionaries gap (dicts=0 at boot vs bundled-.bdic
  docs: bundle them or correct the docs; test asserts whichever is promised)
- [ ] Task 16: First-run device-key failure (`no device key` → session never
  persists? diagnose severity, fix or document)
- [ ] Task 17: Internal-name upstream dials (`ferrite.chrome` DNS attempts:
  short-circuit + test; kills log spam and wasted dials)
- [ ] Task 18: Signed-path dmg flaw (ditto-zip vs stapler; dormant until Apple
  secrets exist — fix now while it is cheap, verify what CI can verify)
- [ ] Task 19: Dependabot backlog, 8 PRs (breaking majors first with compat
  proof: getrandom 0.2→0.4, ed25519-dalek 3.0; each with tests + credits regen)
- [ ] Task 20: mac .icns + Linux .desktop icon (generated in CI where the
  tooling exists; verified by artifact inspection)

### Checkpoint: Complete
- [ ] All acceptance criteria met; zero known issues without an owning task

## Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Toolbar cause needs user data that never comes | High — P0 stalls | Time-box Phase 0 to one ask; if unanswered, proceed with highest-evidence candidate + wider logging in v0.1.1 that self-reports |
| MSI self-proof flakes on CI runners | Med | Prove locally first (`msiexec /quiet` + assertions script), then port the exact script to CI |
| pdf-renderer missing from bundles | Med — dead PDF features | Task 5 answers before v0.1.1 ships; packaging fix rides the same release |
| Signing money never appears | Low — decided: indefinite unsigned, Task 13 covers it | Tasks 10/13 deliver trust without it |
| Dependabot majors break compat | Med | Compat proof per PR (build + test + credits regen); majors isolated one per commit |

## Open Questions (need human answers)

1. v0.1.1 timing: cut as soon as Phase 1 lands, or batch with anything else?
2. Signing budget: is ~$300/yr for a Windows cert conceivable this year, or plan indefinite unsigned?
3. Rollout raise: what counts as "crash-free hours" — your call after v0.1.1 telemetry.
4. Desktop shortcut: Start Menu only (current), or also offer a desktop icon in the installer?
