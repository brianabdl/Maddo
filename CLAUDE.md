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

## Non-negotiable design constraint

This project deliberately does **not** attempt to defeat Cloudflare's
protection on idx.co.id. No TLS/JA3 fingerprint spoofing, no stealth
browser patches (e.g. puppeteer-extra-stealth-style plugins), no automated
challenge-solving, and no headless workarounds. `--headless` is expected to
fail against Cloudflare's managed challenge and is left that way on
purpose. When extending this code, do not introduce anything that evades
bot detection instead of just waiting it out with a real browser session.
This constraint shaped every architectural decision below, so keep it in
mind before "helpfully" adding a stealth flag or a custom TLS client.

## Architecture

The core idea: launch a real, unmodified Chromium-based browser (Brave by
default) over the Chrome DevTools Protocol via `chromiumoxide`, let
Cloudflare's JS/managed challenge resolve exactly as it would for a human
visitor, and then reuse that same authenticated browser tab for everything
else. Concretely:

- **`src/browser.rs`** — `Session::open()` launches the browser, opens the
  target page, and polls `document.querySelectorAll('.attach-card').length`
  once a second (up to 30s) until real content appears, meaning the
  challenge cleared. If it never clears, it saves a screenshot
  (`idx_challenge_debug.png`) and bails rather than trying to work around
  it. `Session::close()` shuts the browser down; callers must call it
  explicitly (there's no `Drop` impl).

- **`src/api.rs`** — a typed client for IDX's own internal endpoint,
  `/primary/ListedCompany/GetAnnouncement`. This isn't a hidden or reverse
  engineered API: it's literally what the site's own frontend calls when a
  user paginates, filters by date, or searches, discovered by watching the
  page's network traffic while using the UI. `fetch_announcements()` runs
  a `fetch()` call *inside* the already-cleared browser tab via
  `page.evaluate()`, so the request automatically carries the tab's real
  session cookies (`credentials: 'include'`, same-origin). Query params:
  `kodeEmiten` (ticker), `emitenType` (security type), `indexFrom` /
  `pageSize` (pagination), `dateFrom` / `dateTo` (YYYYMMDD), `lang`,
  `keyword`. The `emitenType` codes (`s` Saham, `o` Obligasi & Sukuk, `etf`
  ETF, `dd` Dire Dinfra, `eba` EBA, `*` all) were extracted by driving the
  site's own filter dropdown and reading the resulting request; they only
  exist as the `SecurityType` enum in `src/main.rs`, so if IDX adds a new
  security type there's no code-level pointer to it beyond that enum.

- **`src/download.rs`** — downloads attachment files the same way: a
  same-origin `fetch()` executed in-page, with the response bytes
  marshaled back to Rust as base64 (chunked to avoid blowing the JS string
  stack for large files) and decoded/written to disk on the Rust side.
  This exists because the `StaticData` file host is *also* behind
  Cloudflare and returns 403 to a bare `curl`/`reqwest` request; only the
  browser's own authenticated `fetch()` gets through. Downloads run in
  batches of `--concurrency` URLs (via `Promise.all` inside one
  `page.evaluate()` call per batch), with `--delay-ms` between batches.

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
    against the poll interval, specifically so `Session::close()` still
    runs on Ctrl+C and no browser process is left orphaned.

Data flow for a single `fetch`/`download` call is straightforward:
`Session::open` → `fetch_filtered` (loops pages, calls
`api::fetch_announcements`) → (`download`: `build_download_tasks` →
`download::download_all`) → `Session::close`. `watch` replaces the page-loop
with a `tokio::time::interval` loop calling `poll_once` until interrupted.
