# Local development (dashboard + desktop UI)

## 1. API + lifecycle (`ao-dashboard` via CLI)

From a repo that has ao-rs config / sessions (or your default sessions dir):

```bash
cargo run -p ao-cli -- dashboard
```

- REST + SSE: `http://127.0.0.1:3000/api/` (default port **3000**).
- Human-readable landing + links: `http://127.0.0.1:3000/`
- Liveness JSON: `http://127.0.0.1:3000/health`
- Open the landing page in a browser automatically:

```bash
cargo run -p ao-cli -- dashboard --open
```

## 2. Web UI (Vite)

In another terminal:

```bash
cd crates/ao-desktop/ui
npm install
npm run dev
```

Set **Dashboard URL** in the app to match the server (e.g. `http://127.0.0.1:3000`).

## 3. Tauri desktop shell

```bash
cd crates/ao-desktop
# follow crate README for `tauri dev` / build; same Dashboard URL applies.
```

## PR enrichment cost

`GET /api/sessions?pr=true` calls GitHub/`gh` (bounded concurrency on the server). The desktop UI loads a **fast** session list first, enriches PR data in the background, uses **fast** refresh on SSE events, and refetches PR signals on a **timer** and after user actions.
