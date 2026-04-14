# Release / distribution (Phase 6)

This repo currently targets a **local install** workflow first (fast iteration), with optional artifact builds later.

## What “release” means right now

- **CLI**: ship a single `ao-rs` binary (from `crates/ao-cli`) installed via Cargo.
- **Dashboard**: runs as part of the CLI (`ao-rs dashboard`).
- **Desktop UI**: built separately (`crates/ao-desktop`) when needed; not yet packaged as a signed installer.

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

- GitHub Actions workflow building:
  - `ao-rs` binaries for macOS/Linux (and Windows if desired)
  - desktop artifacts from `crates/ao-desktop` (Tauri bundle)
- Optional signing/notarization (macOS) once the UI is stable.

