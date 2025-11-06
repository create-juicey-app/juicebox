# Juicebox

Fast Catbox-like hotlink file share.

Juicebox is a lightweight, high-speed file hosting service with direct hotlinking. Upload, share, done.

## Quick Start

Prerequisites:

- Rust
- Node.js 20+ (for frontend bundle and Jest tests)

Install:

```bash
git clone https://github.com/create-juicey-app/juicebox.git
cd juicebox
cp .env.example .env
# Generate a secret (min 16 bytes) and add IP_HASH_SECRET to .env
openssl rand -hex 32
```

Run:

```bash
cargo run --release
```

Open http://localhost:8080

## Configuration (env)

Common options (set in .env or your environment):

- MAX_FILE_SIZE — per-upload limit (e.g. 750MB, 1GB, or raw bytes)
- JUICEBOX_STORAGE_ROOT — base directory; other storage paths resolve under it
- JUICEBOX_DATA_DIR — metadata dir (default: data/)
- JUICEBOX_UPLOAD_DIR — files dir (default: files/)
- JUICEBOX_CHUNK_DIR — chunk dir (default: data/chunks)
- JUICEBOX_PUBLIC_DIR — serve static assets from a different directory
- JUICEBOX_PROD_HOST — canonical host for generated links when APP_ENV=production
- APP_ENV — set to production for prod-only checks

## Frontend (will be deprecated)

Build once:

```bash
npm install
npm run build
```

Dev mode:

```bash
npm run build:watch
```

Tests:

```bash
npm test
```

## Telemetry (optional)

Sentry can capture errors and traces if enabled:

- SENTRY_DSN — your DSN; leave unset in dev to disable (or set to disabled/off)
- SENTRY_ENV — environment label (defaults from APP_ENV)
- SENTRY_RELEASE — release identifier; falls back to crate version/commit
- SENTRY_TRACES_SAMPLE_RATE — 0.0–1.0 (defaults to 0.1 in production)

## Usage

- Visit http://localhost:8080
- Upload a file in the web UI
- Share the direct link

API (curl):

```bash
curl -F 'file=@path/to/yourfile.png' http://localhost:8080/api/upload
```

## CDN / Cloudflare

Juicebox sends cache-friendly headers on file downloads.
Optional automatic purge on delete when both are set:

- CLOUDFLARE_ZONE_ID
- CLOUDFLARE_API_TOKEN (with Zone.Cache Purge)

## Contributing

- Fork, branch, commit, push, PR

```bash
git checkout -b feature/your-feature
git commit -am 'Add new feature'
git push origin feature/your-feature
```

## License

MIT

## Links

- Repo: https://github.com/create-juicey-app/juicebox
- Issues: https://github.com/create-juicey-app/juicebox/issues

### Cloudflare / CDN notes

If you front Juicebox with a CDN such as Cloudflare the server will send cache headers on file downloads so the CDN can serve files from the edge instead of the origin. That reduces download latency and origin bandwidth use dramatically for files that are requested more than once.

Behaviour implemented by the server:

- For files with a long TTL the server will send: `Cache-Control: public, max-age=31536000, immutable` so browsers and CDNs cache aggressively.
- For files with a shorter remaining TTL the server will use `Cache-Control: public, max-age=<remaining_seconds>` and set an `Expires` header derived from the file metadata.

When you delete a file the server will attempt to purge the corresponding edge cache entry via the Cloudflare Purge API. Purges run in the background (do not delay the HTTP delete response). Purge calls are optional and only attempted when the following environment variables are set:

- `CLOUDFLARE_ZONE_ID` — the numeric or hex zone identifier for your site
- `CLOUDFLARE_API_TOKEN` — an API token with the `Zone.Cache Purge` scope for that zone

If those variables are not present the server will safely no-op and continue functioning normally (useful for local development / tests).

---

## API

You can upload files via a simple POST request:

```http
POST /api/upload
Content-Type: multipart/form-data

file=<your file>
```

**Example (using curl):**

```bash
curl -F 'file=@path/to/yourfile.png' http://localhost:8080/api/upload
```

---

## Contributing

Pull requests and issues are welcome! Please open an issue first to discuss major changes.

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/your-feature`)
3. Commit your changes (`git commit -am 'Add new feature'`)
4. Push to the branch (`git push origin feature/your-feature`)
5. Open a pull request

---

## License

MIT License

---

## Acknowledgements

- [Catbox](https://catbox.moe/) for inspiration
- Rust, JavaScript, HTML, CSS communities

---

## Links

- [GitHub Repository](https://github.com/create-juicey-app/juicebox)
- [Issues](https://github.com/create-juicey-app/juicebox/issues)
