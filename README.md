# Maddo

A command-line tool for fetching, watching, and downloading public company
disclosures ("Keterbukaan Informasi") from the [Indonesia Stock Exchange
(IDX)](https://www.idx.co.id/id/perusahaan-tercatat/keterbukaan-informasi).

## How it works

IDX's disclosure pages and file host sit behind Cloudflare. By default,
`maddo` reaches them with an HTTP client (`wreq`) built to present a real
Chrome TLS/HTTP2/JA3 handshake — the same fingerprint a real Chrome install
sends — rather than by defeating a challenge Cloudflare is actively
presenting. No headless-browser stealth patches, no CAPTCHA/JS-challenge
solving. A bare `curl`/`reqwest` request gets blocked here even with a
browser-like `User-Agent`; matching Chrome's actual handshake is what gets
through.

Pass `--browser` to fall back to the original approach instead: a real,
unmodified Chromium-based browser (Brave by default) driven over the Chrome
DevTools Protocol, exactly the way a person would — it opens the page and
waits for Cloudflare's JS challenge to clear on its own, then every
subsequent operation is a same-origin `fetch()` executed *inside that tab*,
inheriting its genuine session cookies. This fallback exists for if/when
IDX's Cloudflare rules tighten enough to block the default transport's
fingerprint too.

Two consequences follow from this:

- **Headless mode is only relevant to `--browser`, and still not
  supported there.** Cloudflare's challenge frequently blocks headless
  browser sessions, and this tool does not work around that. If
  `--browser --headless` fails for you, that's the site's bot detection
  doing its job. Run headed, or headed inside `xvfb-run` on a server
  without a display. (The default, non-`--browser` transport has no
  headless/headed distinction — it never opens a browser window.)
- **The underlying API is IDX's own.** Rather than scraping the rendered
  DOM, `maddo` calls the exact internal endpoint
  (`/primary/ListedCompany/GetAnnouncement`) that the site's own frontend
  calls when you paginate, filter by date, or search. Discovered by
  observing the page's own network traffic, not by reverse-engineering
  anything hidden. This gives reliable pagination and native date/ticker/
  keyword/type filtering for free.

## Requirements

- Rust (2024 edition toolchain)
- Only for `--browser`: a Chromium-based browser installed locally (Brave,
  Chrome, Chromium, or Edge). The default `--browser-path` is
  `/usr/bin/brave`; override it with `--browser-path` or edit the default
  in `src/main.rs` if your browser lives elsewhere. Not needed for normal
  use.

## Build

```sh
cargo build --release
```

The binary is emitted at `target/release/maddo`.

## Commands

### `fetch`: list announcements

Prints matching announcements as JSON.

```sh
maddo fetch --ticker TPIA --date-from 2026-08-01 --date-to 2026-09-01 --pages 3
maddo fetch --keyword "laporan keuangan" --page-size 20
maddo fetch --type saham --output out.json
```

### `download`: save attachment files

Fetches matching announcements and downloads their attached files (PDF,
XLSX, XBRL, etc.) to disk. Filenames are prefixed with the announcement's
date and ticker to avoid collisions.

```sh
maddo download --ticker BBCA --date-from 2026-09-01 --date-to 2026-09-01 --main-only --out-dir ./pdfs
maddo download --from-json out.json --out-dir ./pdfs
```

`--from-json` skips fetching entirely and downloads attachments listed in a
JSON file previously produced by `fetch --output`.

### `watch`: live mode

Polls the same API on an interval and reports announcements that weren't
seen on the previous poll.

```sh
maddo watch
maddo watch --ticker BBCA --interval-secs 15
maddo watch --download --main-only --out-dir ./live
maddo watch --json
```

The first poll only establishes a baseline. It does not print IDX's entire
matching history as "new". Every poll after that reports (and, with
`--download`, downloads) whatever wasn't seen before. Press `Ctrl+C` to
stop; with `--browser`, the browser process is shut down cleanly rather
than left running.

### `live`: local web UI

Serves a small single-page UI on loopback so you can browse the same feed in
a browser instead of the terminal.

```sh
maddo live
maddo live --port 9000
maddo live --host 0.0.0.0    # see the warning below
```

Open the printed URL (default <http://127.0.0.1:8080>). The page has the
same filters as the CLI (ticker, keyword, security type, date range, page
size, language), prev/next pagination, an optional 30-second auto-refresh,
and links that open each attachment PDF. Filters live in the page, so
`live` takes no filter flags of its own, only `--port` and `--host`.

`--host` defaults to `127.0.0.1`, so the server is reachable only from the
machine running it. Binding anything wider exposes an unauthenticated UI
that proxies requests through the impersonating client, so only do it where
something else controls who can reach the port: a container port mapping (see
[Docker](#docker)), a firewall, or an authenticating reverse proxy.

The server has three routes: `/` (the UI),
`/api/announcements` (the same `GetAnnouncement` call the other subcommands
make, through the active transport), and `/api/file` (a proxy that streams
one attachment back to the page). The file proxy refuses any URL outside
`https://www.idx.co.id/`, so the page can't use it to fetch arbitrary hosts.
Press `Ctrl+C` to stop; with `--browser`, the browser process is shut down
cleanly.

## Docker

`compose.yaml` runs `live` as a long-lived service, which is the easiest way
to keep the UI up across reboots:

```sh
docker compose up -d          # build and start
docker compose logs -f        # follow output
docker compose down           # stop
```

The UI is then on <http://127.0.0.1:8080>. Inside the container the server
binds `0.0.0.0` (nothing else in the container namespace would reach
loopback), but the published port is pinned to the host's loopback:

```yaml
ports:
  - "127.0.0.1:8080:8080"
```

Change that to `"8080:8080"` only if you have put access control in front of
it. To serve a different host port, edit the left-hand side (for example
`"127.0.0.1:9000:8080"`); the container-side port stays `8080` unless you
also change the `command`.

The image is CLI-complete, so the other subcommands work too:

```sh
docker compose run --rm maddo fetch --ticker BBCA --pages 1
docker compose run --rm --volume "$PWD/downloads:/data" maddo \
  download --ticker BBCA --out-dir /data
```

Two limits: `--browser` is unsupported in the image (no browser is
installed, and that fallback exists to drive a real headed one), and the
container runs read-only, so anything writing files needs a mounted volume
as in the `download` example above.

The build stage installs cmake, clang, perl, and Go on top of the Rust
image, because `wreq`'s TLS backend compiles BoringSSL from source. The
first build is therefore slow; later ones reuse cargo's cache mounts.

## Filtering options

`fetch`, `download`, and `watch` share the same filter flags (`live` sets
them in the UI instead):

| Flag | Description |
| --- | --- |
| `--ticker <CODE>` | Stock ticker / kode emiten, e.g. `TPIA`, `BBCA`. Default: all. |
| `--keyword <TEXT>` | Free-text search across announcement titles. |
| `--type <TYPE>` | Security type: `saham`, `obligasi`, `etf`, `dire-dinfra`, `eba`. Default: all. |
| `--date-from <YYYY-MM-DD>` | Lower bound on announcement date. Default: no bound. |
| `--date-to <YYYY-MM-DD>` | Upper bound on announcement date. Default: today. |
| `--lang <id\|en>` | API response language. Default: `id`. |

`fetch` and `download` additionally support `--page`, `--pages`, and
`--page-size` for pagination. `watch` uses `--window` (how many of the
latest matching announcements to check per poll) instead, since it always
looks at the current head of the feed.

## Global options

| Flag | Description |
| --- | --- |
| `--browser` | Use the `--browser` fallback (real Chromium-based browser over CDP) instead of the default `wreq`-based HTTP transport. |
| `--browser-path <PATH>` | Path to a Chromium-based browser executable. Only used with `--browser`. Default: `/usr/bin/brave`. |
| `--headless` | Run the `--browser` fallback headless. Unsupported by Cloudflare on this site in practice; see [How it works](#how-it-works). Only used with `--browser`. |
| `--delay-ms <MS>` | Pause between paginated/batched requests. Default: `800`. Keeps the tool from hammering IDX's servers. |

## Project layout

```
src/
  main.rs      CLI definition (clap) and command orchestration
  backend.rs   Backend enum unifying the two transports (Http default, Browser fallback)
  http.rs      Default transport: wreq client impersonating a real Chrome TLS/HTTP2/JA3 handshake
  browser.rs   --browser fallback: browser session lifecycle over CDP, wait past the Cloudflare challenge
  api.rs       Typed client for IDX's internal GetAnnouncement API (both transports)
  download.rs  Batched file downloads (both transports)
  server.rs    `live` subcommand: minimal HTTP server for the web UI
  ui/index.html  The single-page UI served by `live` (embedded at compile time)
Dockerfile     Two-stage build: BoringSSL-capable Rust builder, slim runtime
compose.yaml   Runs `live` as a restarting service on the host's loopback
```

## Data source and scope

This tool only reads and downloads material IDX already publishes openly
for public disclosure purposes on its own public-facing pages. It performs
no authentication bypass, credential handling, or access to non-public
data. Use it responsibly: keep `--delay-ms` reasonable, don't run
concurrent instances against the same endpoint, and respect IDX's terms of
use.
