# Juicebox Copilot Playbook

## Architecture in one minute

- Axum-based Rust backend (`src/main.rs`) wires middleware stack and routes via `handlers::build_router`; everything flows through `AppState`, so clone and pass the struct instead of reaching for globals.
- File metadata lives in `data/` (`file_owners.json`, `reports.json`, `ip_bans.json`, `chunks/`); files themselves live in `files/`. Keep the JSON mirrors in sync by calling the appropriate `state.persist_*` helper after mutations.
- Requests are tied to hashed IPs via `IP_HASH_SECRET`; never store raw addresses. Use `AppState::hash_ip[_to_string]` to derive owner IDs.
- Chunk uploads persist incremental progress under `data/chunks/<session>/session.json`; use the existing `ChunkSession` helpers instead of inventing new persistence.

## Backend coding patterns

- Handlers live in `src/handlers/**` and are exposed through `src/handlers.rs`. Add routes there and tag handlers with `#[axum::debug_handler]` for compile-time extractor checks.
- Use `util::json_error` for JSON failures and reuse `state` helpers for ban checks (`state.is_banned`), TTL parsing (`util::ttl_to_duration`), and rate limiting (`rate_limit.rs`).
- Respect `MAX_ACTIVE_FILES_PER_IP` and forbidden extensions (`util::FORBIDDEN_EXTENSIONS`) when accepting uploads; the upload flow already enforces these rules via `handlers/upload.rs`—stay consistent.
- Static responses and admin pages pull HTML from `public/`; if you add new templates, drop them there and hydrate with Tera just like `handlers/web.rs`.
- Background cleanup (expiry, admin session pruning, rate limiter GC) runs from `main.rs`; if you add new stateful resources, hook them into that loop or provide a persist helper.

## Frontend & asset pipeline

- Source JS lives in `public/js/*.js` and ships as native ES modules; shared UI logic goes through `upload.js`, `ui.js`, `utils.js`. Globals are patched via `window.*` (see `config.js`), so set new config knobs there if they must be runtime-toggled.
- Build assets with `npm run build`; `scripts/build.mjs` uses esbuild + PostCSS, hashes filenames, writes `public/dist/*`, precompresses `.br/.gz`, and emits `manifest.json`. Tera templates read that manifest, so regenerate it before relying on new bundles.
- Jest tests in `frontend_tests/` import the source modules directly; if you tweak DOM helpers, expose pure functions so tests can continue to run in jsdom.

## Running & testing

- Backend dev loop: `IP_HASH_SECRET=$(openssl rand -hex 32) cargo run` (port 1200). Set `APP_ENV=production` to enforce host redirects.
- Rust tests: `cargo test` exercises the full upload/delete/report flow through the integration suite in `tests/`; tests spin up ephemeral temp dirs, so they do not touch real data.
- Frontend: `npm install` once, `npm run build` for bundles, `npm run build:watch` while iterating, and `npm test` for jsdom unit tests.
- Optional env flags: `MAX_FILE_SIZE=750MB`, `ENABLE_STREAMING_UPLOADS=1`, `TRUST_PROXY_HEADERS=1` + `TRUSTED_PROXY_CIDRS=...`, Mailgun notifications via `MAILGUN_API_KEY`, `MAILGUN_DOMAIN`, `REPORT_EMAIL_TO`, `REPORT_EMAIL_FROM`.

## Key references

- Upload flow & chunking: `src/handlers/upload.rs`
- State helpers & persistence: `src/state.rs`
- Static hosting & config API: `src/handlers/hosting.rs`
- Security middleware & bans: `src/handlers/security.rs`
- Frontend entry & queue logic: `public/js/upload.js`
- Asset bundler: `scripts/build.mjs`

Keep instructions short and specific—call out existing helpers instead of re-implementing logic, persist state after each mutation, and regenerate the dist manifest whenever hashed assets change.
