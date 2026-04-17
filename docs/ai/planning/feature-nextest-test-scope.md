---
phase: planning
title: nextest adoption + per-module test scope — planning
description: Task list for landing the docs change for issue #168.
---

# Planning — nextest adoption + per-module test scope

## Milestones

- [ ] **M1: `CONTRIBUTING.md` landed.** Root-level contributor doc exists,
      contains the nextest commands, the per-module scope rule, and
      the inner dev loop cheat-sheet.
- [ ] **M2: Cross-links in place.** `docs/ai/implementation/README.md`
      points at `CONTRIBUTING.md` for testing conventions.
- [ ] **M3: Quality gates pass.** `cargo fmt --check`, `cargo clippy
      -D warnings`, `cargo t --workspace`, `cargo test --doc
      --workspace` all clean on this branch.
- [ ] **M4: PR merged.** Branch pushed, PR opened against `main`
      referencing issue #168.

## Task Breakdown

### Phase 1 — Write

- [ ] **Task 1.1 — Draft `CONTRIBUTING.md`.** Sections:
  1. Project orientation (one paragraph: what ao-rs is).
  2. Build + test commands (`cargo t`, `cargo test --doc`, `cargo
     clippy`, `cargo fmt`).
  3. Installing nextest if missing.
  4. Per-module test scope rule (table from issue body).
  5. What Rust already proves — do NOT test.
  6. Target coverage by layer (table from issue body).
  7. Inner dev loop cheat-sheet (`cargo check`, `cargo watch`).
  8. Pre-PR checklist.

- [ ] **Task 1.2 — Update `docs/ai/implementation/README.md`.** Add
      a short "Testing conventions" pointer at the top that links to
      `CONTRIBUTING.md` — so the dev-lifecycle implementation phase picks
      up the scope rule via the existing scaffold.

### Phase 2 — Verify

- [ ] **Task 2.1 — Run `cargo fmt --all -- --check`.** Expect no
      output (no Rust code changed). Confirms tooling works.
- [ ] **Task 2.2 — Run `cargo clippy --workspace --all-targets
      -- -D warnings`.** Same — no code changed, clean pass.
- [ ] **Task 2.3 — Run `cargo t --workspace`.** Confirms the test
      suite still passes. If any test was already flaky on `main`,
      note it explicitly (don't swallow).
- [ ] **Task 2.4 — Run `cargo test --doc --workspace`.** Doctests
      pass.

### Phase 3 — Ship

- [ ] **Task 3.1 — Commit.** One focused commit:
      `docs(contrib): adopt cargo-nextest + per-module test scope (#168)`.
- [ ] **Task 3.2 — Push branch.** `git push -u origin
      feature/168-chore-test-adopt-cargo-nextest-establish-per`.
- [ ] **Task 3.3 — Open PR.** Title references issue #168. Body
      calls out the N/A CI criterion with a link to the removal
      commit `e8e4c54`.

## Dependencies

```
Task 1.1 (CONTRIBUTING.md draft) ─┬─> Task 1.2 (cross-link)
                            └─> Task 2.* (verification)
All of Phase 1 + Phase 2    ───> Task 3.* (ship)
```

## Timeline & Estimates

| Task | Effort |
|---|---|
| 1.1 draft CONTRIBUTING.md | ~30 min |
| 1.2 cross-link | ~5 min |
| 2.1–2.4 verification | ~5 min wall-clock (most time is cargo cache warm-up) |
| 3.1 commit | ~2 min |
| 3.2 push | ~1 min |
| 3.3 open PR | ~5 min |

Total: about an hour.

## Risks & Mitigation

| Risk | Impact | Mitigation |
|---|---|---|
| **CONTRIBUTING.md duplicates content from README/ao-rs.yaml and drifts** | Contributors follow stale command | Pin CONTRIBUTING.md as the authoritative source; other docs link, don't copy. The design doc records this decision. |
| **Contributor expects a CONTRIBUTING.md and misses CONTRIBUTING.md** | Friction on first PR | GitHub surfaces `CONTRIBUTING.md` in the Community Standards sidebar when it's the only candidate, and README's "Development" section already shows the right commands. Re-open the CONTRIBUTING.md question if a real contributor asks. |
| **Issue's CI acceptance criterion reads as "must update workflow"** | PR reviewer thinks we punted | PR description explicitly calls out that workflows were deleted in `e8e4c54` and that the CONTRIBUTING.md captures the canonical commands for the next CI re-intro. |
| **Agent rules in `ao-rs.yaml` get out of sync with CONTRIBUTING.md** | Spawned agent follows stale rule | Low — `ao-rs.yaml` only lists the two commands we standardized on. If they change, both files move together. The `ao-rs` project rules already say `cargo t` as of commit `e1cff9d`. |

## Resources Needed

- Issue body from [#168](https://github.com/duonghb53/ao-rs/issues/168)
  — the test scope rule table is authoritative there.
- Existing references: `.cargo/config.toml`, `README.md` "Development"
  section, `docs/RELEASE.md` release-checklist section, `ao-rs.yaml`
  project rules.

## Exit Criteria

Issue #168 is closeable when:

1. `CONTRIBUTING.md` exists at repo root with all sections from Task 1.1.
2. `docs/ai/implementation/README.md` links to `CONTRIBUTING.md` for
   testing conventions.
3. Verification commands (2.1–2.4) pass clean.
4. Branch is pushed, PR is open, PR body records the N/A CI
   criterion + link to `e8e4c54`.
