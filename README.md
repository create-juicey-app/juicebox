# Juicebox

Fast Catbox-like hotlink file share.

Juicebox is a lightweight, high-speed file hosting and sharing service inspired by [Catbox](https://catbox.moe/). It allows users to quickly upload and share files with direct hotlinking support, making it ideal for sharing images, videos, documents, and other files.

---

## Getting Started

### Prerequisites

- **Rust** (for building and running Juicebox)
- **Node.js 20+** (for bundling frontend assets and running Jest tests)

### Installation

#### Clone the repository

```bash
git clone https://github.com/create-juicey-app/juicebox.git
cd juicebox
```

#### Build and Run

```bash
cargo build --release
cargo run --release
```

By default, Juicebox will start its backend server, which serves both the frontend web UI and the API.

Before starting the server, set an `IP_HASH_SECRET` environment variable (minimum 16 bytes) that will be used for HMAC-based IP hashing. You can generate one with:

```bash
openssl rand -hex 32
```

#### Frontend assets & tests

Install the JavaScript toolchain once:

```bash
npm install
```

Bundle the frontend (output is written to `public/dist/`):

```bash
npm run build
```

For active development you can watch for changes:

```bash
npm run build:watch
```

Run the Jest unit tests that cover shared frontend utilities:

```bash
npm test
```

### Telemetry (optional)

Juicebox ships with [Sentry](https://sentry.io/) error reporting. In production builds the backend falls back to a bundled project DSN, but you can override or disable telemetry via environment variables:

- `SENTRY_DSN` &mdash; set to your own DSN. Leave unset in development to disable Sentry, or set to `disabled`/`off` to opt out explicitly.
- `SENTRY_ENV` &mdash; optional environment label reported to Sentry (defaults to `production`/`development` based on `APP_ENV`).
- `SENTRY_RELEASE` &mdash; optional release identifier shown on Sentry's Releases tab. When omitted the server falls back to the crate version or a detected commit hash.
- `SENTRY_TRACES_SAMPLE_RATE` &mdash; floating-point value in `[0.0, 1.0]` controlling tracing/transaction sampling (defaults to `0.1` in production and `0.0` elsewhere).
- `SENTRY_VERIFY_PANIC` &mdash; set to `1`/`true` to trigger a startup panic and verify that events reach Sentry; unset or `0` for normal operation.

Release health is enabled automatically whenever Sentry runs. Each inbound request records a server-mode session so crash-free percentages and adoption metrics light up in Sentry. If you deploy via CI/CD, export either `SENTRY_RELEASE` or one of `SOURCE_VERSION`, `GIT_COMMIT`, `GIT_SHA`, `GITHUB_SHA`, `VERCEL_GIT_COMMIT_SHA`, `COMMIT_SHA`, or `REVISION` so the backend can associate sessions and errors with the right release.

Juicebox also emits structured tracing spans for every HTTP request via `tower-http`'s `TraceLayer`, and the main handlers are annotated with `#[tracing::instrument]` so the spans arrive in Sentry as nested operations under the request transaction. If your setup includes reverse proxies or additional services, make sure the `sentry-trace` and `baggage` headers are allowed to pass through so distributed traces can flow between components. During local profiling, feel free to temporarily set `SENTRY_TRACES_SAMPLE_RATE=1.0` to capture every request; dial the value back down before production rollout if throughput is high.

The `/api/config` endpoint now surfaces Sentry release, environment, DSN, and trace-propagation targets to the browser bundle. When telemetry is enabled, the frontend initialises the Sentry Browser SDK during app bootstrap, automatically propagates trace headers on API calls, and captures handled failures during the upload flow. Disable or override telemetry by adjusting the environment variables above; the client respects the same `SENTRY_DSN` flag as the backend.

When Sentry is enabled, shutdown waits briefly to flush any buffered events before exiting.

---

## Usage

- Open your browser and navigate to the address shown in the terminal (default: [http://localhost:8080](http://localhost:8080)).
- Use the web interface to upload files.
- Share the provided direct link to your uploaded file.

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
