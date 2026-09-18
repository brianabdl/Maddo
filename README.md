# Maddo

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](Cargo.toml)

A command-line tool that fetches, watches, and downloads public company
disclosures ("Keterbukaan Informasi") from the [Indonesia Stock
Exchange](https://www.idx.co.id/id/perusahaan-tercatat/keterbukaan-informasi).
List announcements by ticker, keyword, or date, pull down every attached PDF,
or watch the feed live from the terminal or a small local web UI.

## Why it exists

IDX's disclosure pages and file host sit behind Cloudflare, and a plain
`curl` or `reqwest` call gets blocked even with a browser-like `User-Agent`.
Maddo's default transport gets past that by presenting a real Chrome
TLS/HTTP2/JA3 handshake, the same fingerprint an actual Chrome install sends,
rather than by solving or defeating any challenge Cloudflare is presenting
live. No headless-browser stealth patches, no CAPTCHA solving: just a
fingerprint that doesn't trigger a challenge in the first place. If that
stops working, `--browser` falls back to a real, unmodified Chromium-based
browser (Brave by default) that opens the page and waits for the challenge
to clear on its own.

Under the hood, Maddo also calls IDX's own internal
`GetAnnouncement` endpoint, the exact one the site's frontend uses when you
paginate or filter, discovered by watching real network traffic rather than
reverse-engineering anything hidden. That means native filtering by ticker,
date, keyword, and security type, with no HTML scraping involved.

## Quickstart

```sh
cargo build --release
./target/release/maddo fetch --ticker BBCA --pages 1
```

## Features

- **`fetch`**: list matching announcements as JSON.
- **`download`**: fetch and save attachment files (PDF, XLSX, XBRL) to disk,
  filenames prefixed with date and ticker.
- **`watch`**: poll the feed on an interval and report only what's new since
  the last poll, optionally downloading it too.
- **`live`**: a small local web UI with the same filters, pagination, and
  auto-refresh, for browsing the feed without a terminal.

## Usage

```sh
# List announcements
maddo fetch --ticker TPIA --date-from 2026-08-01 --date-to 2026-09-01 --pages 3
maddo fetch --keyword "laporan keuangan" --page-size 20

# Download attachments
maddo download --ticker BBCA --date-from 2026-09-01 --main-only --out-dir ./pdfs
maddo download --from-json out.json --out-dir ./pdfs   # skip fetching, reuse a saved result

# Watch live
maddo watch --ticker BBCA --interval-secs 15 --download --out-dir ./live

# Web UI
maddo live --port 9000
```

All of `fetch`, `download`, and `watch` share the same filters:

| Flag | Description |
| --- | --- |
| `--ticker <CODE>` | Stock ticker, e.g. `TPIA`, `BBCA`. Default: all. |
| `--keyword <TEXT>` | Free-text search across announcement titles. |
| `--type <TYPE>` | Security type: `saham`, `obligasi`, `etf`, `dire-dinfra`, `eba`. |
| `--date-from` / `--date-to <YYYY-MM-DD>` | Date range. Defaults: no lower bound, today. |
| `--lang <id\|en>` | API response language. Default: `id`. |

`fetch` and `download` add `--page`, `--pages`, `--page-size` for
pagination. `watch` uses `--window` instead, since it always looks at the
current head of the feed. `watch`'s first poll only establishes a baseline;
it won't dump IDX's entire history as "new" on startup.

Global flags: `--browser` (use the Chromium fallback instead of the default
transport), `--browser-path` (default `/usr/bin/brave`), `--headless`
(browser fallback only, and not supported by Cloudflare here in practice),
and `--delay-ms` (pacing between batched requests, default `800`).

Run `maddo --help` or `maddo <command> --help` for the full list.

## License

[GPL-3.0](LICENSE)
