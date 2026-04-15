# Release / distribution (Phase 6)

This document is the source of truth for how we **distribute ao-rs** today and what we explicitly **are not shipping yet**.

## Decision record (Issue #34 — Phase 6)

**Decision (now)**: We support **local installs** as the primary distribution method, and we will add **CI-built unsigned tarball artifacts** next. We will not ship signed installers until the desktop UI stabilizes.

### Supported targets

- **Tier 1 (supported)**: macOS (Apple Silicon + Intel), Linux (x86_64)
- **Tier 2 (best-effort, later)**: Windows (x86_64)

Rationale:

- macOS + Linux cover the majority of early adopters (Rust dev tooling, tmux-centric workflow).
- Windows support is feasible but adds packaging/testing surface area (path handling, toolchain availability, signing) that we defer until the core UX is stable.

### What we ship

- **Ship now**
  - **`ao-rs` CLI binary** (from `crates/ao-cli`)
  - **Dashboard server** as a subcommand (`ao-rs dashboard`) — treated as part of the CLI distribution (not a separate package)
- **Do not ship (yet)**
  - **Prebuilt dashboard/UI bundles** (Vite `dist/`) — only built locally for development
  - **Tauri desktop app bundles/installers** (from `crates/ao-desktop`) — prototype only, `bundle.active=false`
  - **Signing/notarization** — explicitly out of scope for this phase

### Artifact strategy

- **Phase 6 (current)**: Local install via Cargo (fast iteration, simplest support story).
- **Next step (Phase 7)**: GitHub Actions builds **unsigned** per-target tarballs for the CLI:
  - `ao-rs-${version}-${target}.tar.gz` containing the `ao-rs` binary (and `LICENSE`, `README` as needed)
  - checksums (e.g. `sha256`) published alongside artifacts
- **Later (Slice 6+ / when UI stabilizes)**: Tauri bundles + optional signing/notarization:
  - macOS notarization, Windows code signing, Linux packaging (AppImage/deb/rpm) as appropriate

Rationale:

- CI tarballs give users a “download-and-run” path without forcing Rust toolchains.
- Signing is expensive (certificates, notarization automation, key management) and only worth doing once the desktop deliverable is a stable product.

## What “release” means right now

- **CLI**: ship a single `ao-rs` binary (from `crates/ao-cli`) installed via Cargo.
- **Dashboard**: runs as part of the CLI (`ao-rs dashboard`).
- **Desktop UI**: built separately (`crates/ao-desktop`) when needed; not distributed as an installer yet.

## Local install (recommended)

### Install/update the CLI

```bash
cargo install --path crates/ao-cli --locked
```

Cargo installs binaries into:

- macOS/Linux default: `~/.cargo/bin/ao-rs`

Verify:

```bash
which ao-rs
ao-rs --help
```

### Build desktop UI (optional)

```bash
cd crates/ao-desktop/ui
npm install
npm run build
```

## “Release checklist” (manual)

Before you publish anything:

- Run `cargo test` (or at least `cargo test -p ao-dashboard -p ao-cli`)
- Run `npm run build` in `crates/ao-desktop/ui`
- Run the full manual checklist in **[`SMOKE.md`](SMOKE.md)**

## Later (not implemented yet)

When we’re ready for distribution beyond local installs:

- GitHub Actions workflow building unsigned artifacts:
  - `ao-rs` binaries for macOS/Linux (Windows optional)
  - checksums for integrity validation
- Desktop artifacts (Tauri bundle) only after the UI is stable.
- Signing/notarization only after we commit to shipping desktop installers.

