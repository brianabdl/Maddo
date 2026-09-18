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

## Usage

Run `maddo --help` or `maddo <command> --help` for the full list.

## License

[GPL-3.0](LICENSE)
