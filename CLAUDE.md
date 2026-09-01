# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Maddo is a Rust CLI (binary name `maddo`) that fetches, watches, and
downloads public company disclosures ("Keterbukaan Informasi") from the
Indonesia Stock Exchange (IDX). See `README.md` for full user-facing
documentation of commands and flags; this file covers what you need to work
on the code itself.

## Commands

```sh
cargo build              # debug build -> target/debug/maddo
cargo build --release    # release build -> target/release/maddo
cargo run -- <args>      # e.g. cargo run -- fetch --ticker BBCA --pages 1
```

There is no test suite. Verification so far has been manual: run the
relevant subcommand against the live site and inspect the output (e.g.
checking downloaded files with `file` to confirm they're valid PDFs).

The default `--browser-path` (`/usr/bin/brave`) is hardcoded in
`src/main.rs`. If Brave isn't installed at that path in your environment,
pass `--browser-path` explicitly or update the default.

## Design constraint (revised 2026-09-01)

Earlier versions of this file banned TLS/JA3 fingerprint spoofing outright
and required every request to go through a real, human-cleared browser
session. That stance has been relaxed: the default transport (`src/http.rs`,
via the `wreq`/`wreq-util` crates) now impersonates a real Chrome
TLS/HTTP2/JA3 handshake to reach IDX's endpoints without spawning a browser
process. See "IDX API transport" below for why, and for what this still
does *not* do.

What's still off-limits when extending this code: automated CAPTCHA/JS-
challenge solving, headless-browser stealth patches (e.g. puppeteer-extra-
stealth-style plugins) for the `--browser` fallback, and anything that
actively defeats a challenge Cloudflare is presenting live (as opposed to
presenting a fingerprint that doesn't trigger one in the first place).
`--headless` on the `--browser` path is still expected to fail against
Cloudflare's managed challenge and is left that way on purpose. If `wreq`'s
impersonation stops getting through, the fix is to fall back to `--browser`
or update the emulation profile — not to add challenge-solving.

## IDX API transport

Both `/primary/ListedCompany/GetAnnouncement` and the `StaticData` PDF host
sit behind Cloudflare. As of this revision:

- A bare `curl`/`reqwest` request — even with a browser-like `User-Agent`
  and `Referer` header — gets `HTTP 403` with `cf-mitigated: challenge`
  (verified live). Header-only spoofing is not enough.
- A `wreq` client built with `Emulation::Chrome149` (real Chrome TLS/HTTP2/
  JA3 handshake, not a from-scratch fake — `wreq` reuses BoringSSL's actual
  Chrome cipher/extension ordering) gets `200 OK` JSON/PDF responses from
  both hosts with no prior browser session and no cookie priming (verified
  live against both endpoints on 2026-09-01).

So the default path for all three subcommands (`fetch`, `download`, `watch`)
is now `src/http.rs`'s `HttpClient`, no browser process involved. `--browser`
remains as an opt-in fallback through the original chromiumoxide/CDP path,
for if/when IDX's Cloudflare rules tighten enough to block `wreq`'s
fingerprint too. Every request in either transport still only touches
endpoints and files IDX already serves publicly to any visitor; nothing here
authenticates as anyone or reaches non-public data.

## Architecture

`src/backend.rs` defines `Backend`, an enum over the two transports, so
`main.rs` and the subcommand handlers never branch on which one is active:

- `Backend::Http(HttpClient)` — default. `src/http.rs`'s `HttpClient` wraps
  a `wreq::Client` built with `.emulation(Emulation::Chrome149)` and
  `.cookie_store(true)`. `get_json` and `get_bytes` are the only two
  operations; both set `Referer: https://www.idx.co.id/en/` to match what a
  real page load would send.

- `Backend::Browser(browser::Session)` — the `--browser` fallback.
  `src/browser.rs`'s `Session::open()` launches a real, unmodified
  Chromium-based browser (Brave by default) over CDP via `chromiumoxide`,
  opens the target page, and polls
  `document.querySelectorAll('.attach-card').length` once a second (up to
  30s) until real content appears, meaning Cloudflare's challenge cleared.
  If it never clears, it saves a screenshot (`idx_challenge_debug.png`) and
  bails rather than trying to work around it. `Session::close()` shuts the
  browser down; callers must call it explicitly (there's no `Drop` impl).

- **`src/api.rs`** — a typed client for IDX's own internal endpoint,
  `/primary/ListedCompany/GetAnnouncement`. This isn't a hidden or reverse
  engineered API: it's literally what the site's own frontend calls when a
  user paginates, filters by date, or searches, discovered by watching the
  page's network traffic while using the UI. `fetch_announcements_http()`
  (default) calls it via `HttpClient::get_json` with the query params as a
  plain key/value slice; `fetch_announcements()` (the `--browser` fallback)
  runs a `fetch()` call *inside* the already-cleared browser tab via
  `page.evaluate()`, so the request automatically carries the tab's real
  session cookies (`credentials: 'include'`, same-origin). Query params:
  `kodeEmiten` (ticker), `emitenType` (security type), `indexFrom` /
  `pageSize` (pagination), `dateFrom` / `dateTo` (YYYYMMDD), `lang`,
  `keyword`. The `emitenType` codes (`s` Saham, `o` Obligasi & Sukuk, `etf`
  ETF, `dd` Dire Dinfra, `eba` EBA, `*` all) were extracted by driving the
  site's own filter dropdown and reading the resulting request; they only
  exist as the `SecurityType` enum in `src/main.rs`, so if IDX adds a new
  security type there's no code-level pointer to it beyond that enum.

- **`src/download.rs`** — downloads attachment files. `download_all()`
  (default) fetches each URL directly via `HttpClient::get_bytes`, batched
  `--concurrency` at a time with `futures::future::join_all`.
  `download_all_browser()` (the `--browser` fallback) goes through the same
  authenticated browser tab used for listing: a same-origin `fetch()`
  executed in-page, with the response bytes marshaled back to Rust as
  base64 (chunked to avoid blowing the JS string stack for large files) and
  decoded/written to disk on the Rust side, batched via `Promise.all`
  inside one `page.evaluate()` call per batch. Both variants sleep
  `--delay-ms` between batches.

- **`src/main.rs`** — CLI surface (`clap`) and orchestration. Three
  subcommands (`fetch`, `download`, `watch`) share `CoreFilterArgs`
  (ticker/keyword/type/date range/lang); `fetch` and `download` additionally
  take `FilterArgs` (adds page/pages/page_size) for one-shot paginated
  queries via `fetch_filtered()`. `watch` instead polls the API on a timer
  via `poll_once()`:
  - The first poll only records every seen `Id2` as a baseline; it does
    not print IDX's whole matching history as "new" on startup.
  - Every poll after that diffs the response against the `seen` set
    (`HashSet<String>` of `Id2`), reports (and, with `--download`,
    downloads) anything new, oldest-first, and adds it to `seen`.
  - `seen` is opportunistically resynced to just the current window if it
    grows past 5000 entries, so long-running watches don't leak memory
    unboundedly. This means an announcement that both appears and scrolls
    out of the `--window` between polls (unlikely at default settings, but
    possible with a small `--window` and a slow poll interval) could be
    missed rather than double-reported; that's an accepted tradeoff, not a
    bug to silently "fix" by growing the set forever.
  - Shutdown is via `tokio::select!` racing `tokio::signal::ctrl_c()`
    against the poll interval, specifically so `Backend::close()` still
    runs on Ctrl+C and no browser process (when `--browser` is active) is
    left orphaned.

Data flow for a single `fetch`/`download` call is straightforward:
`Backend::open` → `fetch_filtered` (loops pages, calls
`Backend::fetch_announcements`) → (`download`: `build_download_tasks` →
`Backend::download_all`) → `Backend::close`. `watch` replaces the page-loop
with a `tokio::time::interval` loop calling `poll_once` until interrupted.
