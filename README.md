# Maddo

A command-line tool for fetching, watching, and downloading public company
disclosures ("Keterbukaan Informasi") from the [Indonesia Stock Exchange
(IDX)](https://www.idx.co.id/id/perusahaan-tercatat/keterbukaan-informasi).

## How it works

IDX's disclosure pages sit behind Cloudflare. This tool does not attempt to
defeat that protection in any way. No TLS/JA3 fingerprint spoofing, no
stealth browser patches, no challenge-solving. Instead it drives a real,
unmodified Chromium-based browser (Brave by default) over the Chrome DevTools
Protocol, exactly the way a person would: it opens the page and waits for
Cloudflare's JS challenge to clear on its own.

Once the browser tab is past the challenge, every subsequent operation
(listing, filtering, downloading) is a same-origin `fetch()` call executed
*inside that tab*, so it automatically inherits the tab's genuine session
cookies. This is session reuse, not evasion: the browser already earned
that access normally, and reusing its cookies for further requests is the
same thing any web page does for its own asset and API calls.

Two consequences follow directly from this design:

- **Headless mode is not supported.** Cloudflare's challenge frequently
  blocks headless browser sessions. This tool does not work around that.
  If `--headless` fails for you, that's the site's bot detection doing its
  job. Run headed, or headed inside `xvfb-run` on a server without a
  display.
- **The underlying API is IDX's own.** Rather than scraping the rendered
  DOM, `maddo` calls the exact internal endpoint
  (`/primary/ListedCompany/GetAnnouncement`) that the site's own frontend
  calls when you paginate, filter by date, or search. Discovered by
  observing the page's own network traffic, not by reverse-engineering
  anything hidden. This gives reliable pagination and native date/ticker/
  keyword/type filtering for free.

## Requirements

- Rust (2024 edition toolchain)
- A Chromium-based browser installed locally (Brave, Chrome, Chromium, or
  Edge). The default `--browser-path` is `/usr/bin/brave`; override it with
  `--browser-path` or edit the default in `src/main.rs` if your browser
  lives elsewhere.

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
stop; the browser process is shut down cleanly rather than left running.

## Filtering options

All three commands share the same filter flags:

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
| `--browser-path <PATH>` | Path to a Chromium-based browser executable. Default: `/usr/bin/brave`. |
| `--headless` | Run headless. Unsupported by Cloudflare on this site in practice; see [How it works](#how-it-works). |
| `--delay-ms <MS>` | Pause between paginated/batched requests. Default: `800`. Keeps the tool from hammering IDX's servers. |

## Project layout

```
src/
  main.rs      CLI definition (clap) and command orchestration
  browser.rs   Browser session lifecycle: launch, navigate, wait past the Cloudflare challenge
  api.rs       Typed client for IDX's internal GetAnnouncement API
  download.rs  Batched in-tab file downloads (fetch + base64, decoded and written to disk)
```

## Data source and scope

This tool only reads and downloads material IDX already publishes openly
for public disclosure purposes on its own public-facing pages. It performs
no authentication bypass, credential handling, or access to non-public
data. Use it responsibly: keep `--delay-ms` reasonable, don't run
concurrent instances against the same endpoint, and respect IDX's terms of
use.
