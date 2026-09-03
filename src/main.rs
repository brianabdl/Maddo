//! Maddo: a CLI for IDX's public "Keterbukaan Informasi" (listed-company disclosures) feed.
//!
//! By default, requests go through `wreq` (see `http.rs`), which presents a real Chrome
//! TLS/HTTP2/JA3 handshake without spawning a browser. Pass `--browser` to fall back to
//! driving a real, unmodified Chromium-based browser over CDP instead (see `browser.rs`):
//! it opens the page, waits for Cloudflare's JS/managed challenge to clear as it would
//! for a normal human visitor, then runs same-origin `fetch()` calls *inside* that tab so
//! they inherit its genuine session cookies. See CLAUDE.md's "IDX API transport" section
//! for why the default changed and what it does and doesn't attempt. Headless browser
//! mode is left unsupported on purpose: if Cloudflare blocks it, that's its bot detection
//! doing its job.

mod api;
mod backend;
mod browser;
mod download;
mod http;
mod server;

use anyhow::{Context, Result};
use api::QueryParams;
use chrono::NaiveDate;
use clap::{Args, Parser, Subcommand};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Maddo: fetch, watch, and download IDX listed-company disclosures.
#[derive(Parser)]
#[command(name = "maddo", version, about)]
struct Cli {
    /// Use a real Chromium-based browser (via CDP) instead of the default impersonating
    /// HTTP client. Fallback for if/when the default transport stops getting through.
    #[arg(long, global = true)]
    browser: bool,

    /// Path to a Chromium-based browser executable (Chrome, Chromium, Brave, Edge...).
    /// Only used with --browser.
    #[arg(long, global = true, default_value = "/usr/bin/brave")]
    browser_path: String,

    /// Run the browser headless. Only used with --browser; Cloudflare's challenge
    /// frequently blocks headless sessions and this is not worked around here on
    /// purpose. Default is headed.
    #[arg(long, global = true)]
    headless: bool,

    /// Pause between paginated/batched requests, in milliseconds. Keeps the tool from
    /// hammering IDX's servers.
    #[arg(long, global = true, default_value_t = 800)]
    delay_ms: u64,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List announcements matching the given filters.
    Fetch(FetchArgs),
    /// Download attachment files for announcements matching the given filters.
    Download(DownloadArgs),
    /// Poll for newly published announcements and print (and optionally download) them as they land.
    Watch(WatchArgs),
    /// Serve a small local web UI for browsing announcements in a browser.
    Live(LiveArgs),
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum SecurityType {
    Saham,
    Obligasi,
    Etf,
    DireDinfra,
    Eba,
}

impl SecurityType {
    fn code(self) -> &'static str {
        match self {
            SecurityType::Saham => "s",
            SecurityType::Obligasi => "o",
            SecurityType::Etf => "etf",
            SecurityType::DireDinfra => "dd",
            SecurityType::Eba => "eba",
        }
    }
}

/// Filter fields shared by every subcommand (date range, ticker, keyword, security type).
#[derive(Args, Clone)]
struct CoreFilterArgs {
    /// Only announcements from this date onward (YYYY-MM-DD). Default: no lower bound.
    #[arg(long)]
    date_from: Option<String>,

    /// Only announcements up to this date (YYYY-MM-DD). Default: today.
    #[arg(long)]
    date_to: Option<String>,

    /// Filter by stock ticker / kode emiten (e.g. TPIA, BBCA). Default: all tickers.
    #[arg(long)]
    ticker: Option<String>,

    /// Free-text search across announcement titles.
    #[arg(long)]
    keyword: Option<String>,

    /// Filter by security type. Default: all types.
    #[arg(long = "type", value_enum)]
    security_type: Option<SecurityType>,

    /// API response language.
    #[arg(long, default_value = "id")]
    lang: String,
}

impl CoreFilterArgs {
    fn emiten_type(&self) -> String {
        self.security_type
            .map(|t| t.code().to_string())
            .unwrap_or_else(|| "*".to_string())
    }

    fn resolve_dates(&self) -> Result<(String, String)> {
        let date_from = match &self.date_from {
            Some(s) => parse_date(s)?,
            None => "19010101".to_string(),
        };
        let date_to = match &self.date_to {
            Some(s) => parse_date(s)?,
            None => chrono::Local::now().format("%Y%m%d").to_string(),
        };
        Ok((date_from, date_to))
    }
}

fn parse_date(s: &str) -> Result<String> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("invalid date '{s}', expected YYYY-MM-DD"))?;
    Ok(d.format("%Y%m%d").to_string())
}

#[derive(Args, Clone)]
struct FilterArgs {
    #[command(flatten)]
    core: CoreFilterArgs,

    /// First page to fetch (1-indexed).
    #[arg(long, default_value_t = 1)]
    page: u32,

    /// Number of pages to fetch, walking forward from --page.
    #[arg(long, default_value_t = 1)]
    pages: u32,

    /// Announcements per page.
    #[arg(long, default_value_t = 10)]
    page_size: u32,
}

#[derive(Args)]
struct FetchArgs {
    #[command(flatten)]
    filter: FilterArgs,

    /// Write JSON results here instead of stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Args)]
struct DownloadArgs {
    #[command(flatten)]
    filter: FilterArgs,

    /// Skip fetching; download attachments listed in a previously saved `fetch --output` JSON file.
    #[arg(long)]
    from_json: Option<PathBuf>,

    /// Directory to save files into.
    #[arg(long, default_value = "./downloads")]
    out_dir: PathBuf,

    /// How many files to download concurrently per batch.
    #[arg(long, default_value_t = 5)]
    concurrency: usize,

    /// Only download each announcement's primary document, skip supporting attachments.
    #[arg(long)]
    main_only: bool,
}

#[derive(Args)]
struct WatchArgs {
    #[command(flatten)]
    core: CoreFilterArgs,

    /// How many of the latest matching announcements to check on each poll.
    #[arg(long, default_value_t = 20)]
    window: u32,

    /// Seconds between polls.
    #[arg(long, default_value_t = 30)]
    interval_secs: u64,

    /// Automatically download attachments for each newly seen announcement.
    #[arg(long)]
    download: bool,

    /// Directory to save downloaded files into (only with --download).
    #[arg(long, default_value = "./downloads")]
    out_dir: PathBuf,

    /// With --download, only fetch each announcement's primary document.
    #[arg(long)]
    main_only: bool,

    /// With --download, how many files to fetch concurrently per batch.
    #[arg(long, default_value_t = 5)]
    concurrency: usize,

    /// Print each new announcement as one JSON object per line instead of a human-readable line.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct LiveArgs {
    /// Port to serve the UI on. Always bound to loopback (127.0.0.1) only.
    #[arg(long, default_value_t = 8080)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Command::Fetch(args) => run_fetch(&cli, args).await,
        Command::Download(args) => run_download(&cli, args).await,
        Command::Watch(args) => run_watch(&cli, args).await,
        Command::Live(args) => run_live(&cli, args).await,
    }
}

async fn fetch_filtered(
    backend: &backend::Backend,
    filter: &FilterArgs,
    delay_ms: u64,
) -> Result<Vec<api::Reply>> {
    let (date_from, date_to) = filter.core.resolve_dates()?;
    let mut all = Vec::new();

    for i in 0..filter.pages {
        let page_num = filter.page + i;
        let params = QueryParams {
            ticker: filter.core.ticker.clone(),
            keyword: filter.core.keyword.clone(),
            emiten_type: filter.core.emiten_type(),
            date_from: date_from.clone(),
            date_to: date_to.clone(),
            index_from: (page_num - 1) * filter.page_size,
            page_size: filter.page_size,
            lang: filter.core.lang.clone(),
        };

        let resp = backend.fetch_announcements(&params).await?;
        eprintln!(
            "Page {page_num}: {} announcements (of {} matching total).",
            resp.replies.len(),
            resp.result_count
        );

        let got = resp.replies.len() as u32;
        all.extend(resp.replies);

        if got < filter.page_size {
            eprintln!("Reached the last page of results; stopping early.");
            break;
        }
        if i + 1 < filter.pages {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    Ok(all)
}

fn build_download_tasks<'a>(
    replies: impl IntoIterator<Item = &'a api::Reply>,
    main_only: bool,
) -> Vec<download::DownloadTask> {
    let mut tasks = Vec::new();
    for reply in replies {
        let date_compact: String = reply
            .pengumuman
            .tanggal
            .chars()
            .take(10)
            .filter(|c| c.is_ascii_digit())
            .collect();
        let ticker = reply.pengumuman.ticker();

        for att in &reply.attachments {
            if main_only && att.is_supporting {
                continue;
            }
            tasks.push(download::DownloadTask {
                url: att.url.clone(),
                dest_filename: download::dest_filename(&date_compact, ticker, &att.filename),
            });
        }
    }
    tasks
}

async fn run_fetch(cli: &Cli, args: &FetchArgs) -> Result<()> {
    let backend = backend::Backend::open(&cli.browser_path, cli.headless, cli.browser).await?;
    let replies = fetch_filtered(&backend, &args.filter, cli.delay_ms).await?;
    backend.close().await?;

    let json = serde_json::to_string_pretty(&replies)?;
    match &args.output {
        Some(path) => {
            std::fs::write(path, &json).with_context(|| format!("writing {}", path.display()))?;
            eprintln!("Wrote {} announcements to {}.", replies.len(), path.display());
        }
        None => println!("{json}"),
    }
    Ok(())
}

async fn run_download(cli: &Cli, args: &DownloadArgs) -> Result<()> {
    let backend = backend::Backend::open(&cli.browser_path, cli.headless, cli.browser).await?;

    let replies = if let Some(path) = &args.from_json {
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&data).context("parsing --from-json file")?
    } else {
        fetch_filtered(&backend, &args.filter, cli.delay_ms).await?
    };

    let tasks = build_download_tasks(&replies, args.main_only);

    eprintln!("Downloading {} file(s) to {}...", tasks.len(), args.out_dir.display());
    let (ok, err) = backend
        .download_all(&tasks, &args.out_dir, args.concurrency, cli.delay_ms)
        .await?;
    backend.close().await?;

    eprintln!("Done: {ok} succeeded, {err} failed.");
    if err > 0 && ok == 0 {
        anyhow::bail!("all downloads failed");
    }
    Ok(())
}

/// Serves the local web UI until Ctrl+C. Filters live in the page itself; each search
/// it runs goes through the same `Backend` as the other subcommands.
async fn run_live(cli: &Cli, args: &LiveArgs) -> Result<()> {
    let backend = Arc::new(backend::Backend::open(&cli.browser_path, cli.headless, cli.browser).await?);
    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => eprintln!("\nStopping server..."),
        res = server::serve(Arc::clone(&backend), addr) => res?,
    }

    close_shared_backend(backend).await
}

/// Connection handlers hold their own `Arc` clone, so reclaim sole ownership before
/// closing (a `--browser` session must be shut down explicitly). Gives in-flight
/// requests up to ~2s to finish rather than blocking shutdown on a stuck one.
async fn close_shared_backend(mut backend: Arc<backend::Backend>) -> Result<()> {
    for _ in 0..40 {
        match Arc::try_unwrap(backend) {
            Ok(b) => return b.close().await,
            Err(shared) => {
                backend = shared;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    eprintln!("Requests still in flight after 2s; exiting without a clean backend close.");
    Ok(())
}

fn print_announcement(reply: &api::Reply, as_json: bool) {
    if as_json {
        if let Ok(line) = serde_json::to_string(reply) {
            println!("{line}");
        }
        return;
    }
    let p = &reply.pengumuman;
    println!("{}  [{}]  {}", p.tanggal, p.ticker(), p.judul);
    for att in &reply.attachments {
        println!("    - {}", att.url);
    }
}

/// Polls IDX's disclosure API for newly published announcements. The first poll just
/// establishes a baseline (nothing is reported as "new" yet, to avoid dumping the
/// entire matching history on startup); every poll after that reports and optionally
/// downloads whatever wasn't seen before. Stops cleanly on Ctrl+C so the browser
/// process doesn't get left running.
async fn run_watch(cli: &Cli, args: &WatchArgs) -> Result<()> {
    let backend = backend::Backend::open(&cli.browser_path, cli.headless, cli.browser).await?;

    eprintln!(
        "Watching (window={}, every {}s). Press Ctrl+C to stop.",
        args.window, args.interval_secs
    );

    let mut seen: HashSet<String> = HashSet::new();
    let mut first_poll = true;
    let mut interval = tokio::time::interval(Duration::from_secs(args.interval_secs));

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nStopping watch...");
                break;
            }
            _ = interval.tick() => {
                if let Err(e) = poll_once(&backend, args, &mut seen, &mut first_poll, cli.delay_ms).await {
                    eprintln!("Poll failed: {e:#}. Retrying next interval.");
                }
            }
        }
    }

    backend.close().await
}

async fn poll_once(
    backend: &backend::Backend,
    args: &WatchArgs,
    seen: &mut HashSet<String>,
    first_poll: &mut bool,
    delay_ms: u64,
) -> Result<()> {
    let (date_from, date_to) = args.core.resolve_dates()?;
    let params = QueryParams {
        ticker: args.core.ticker.clone(),
        keyword: args.core.keyword.clone(),
        emiten_type: args.core.emiten_type(),
        date_from,
        date_to,
        index_from: 0,
        page_size: args.window,
        lang: args.core.lang.clone(),
    };

    let resp = backend.fetch_announcements(&params).await?;

    if *first_poll {
        for reply in &resp.replies {
            seen.insert(reply.pengumuman.id2.clone());
        }
        eprintln!(
            "Baseline loaded: {} announcement(s). Now watching for new ones...",
            resp.replies.len()
        );
        *first_poll = false;
        return Ok(());
    }

    // The API returns newest first; walk oldest-to-newest so new items print in order.
    let new_items: Vec<&api::Reply> = resp
        .replies
        .iter()
        .rev()
        .filter(|r| !seen.contains(&r.pengumuman.id2))
        .collect();

    if new_items.is_empty() {
        // Bound memory growth: if nothing is new, occasionally resync `seen` to just
        // the current window instead of growing it forever.
        if seen.len() > 5000 {
            *seen = resp.replies.iter().map(|r| r.pengumuman.id2.clone()).collect();
        }
        return Ok(());
    }

    for reply in &new_items {
        seen.insert(reply.pengumuman.id2.clone());
        print_announcement(reply, args.json);
    }

    if args.download {
        let tasks = build_download_tasks(new_items.iter().copied(), args.main_only);
        if !tasks.is_empty() {
            eprintln!("Downloading {} new file(s)...", tasks.len());
            let (ok, err) = backend
                .download_all(&tasks, &args.out_dir, args.concurrency, delay_ms)
                .await?;
            eprintln!("  {ok} succeeded, {err} failed.");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{AttachmentInfo, Pengumuman, Reply};

    fn reply(id2: &str, tanggal: &str, ticker: &str, attachments: Vec<AttachmentInfo>) -> Reply {
        Reply {
            pengumuman: Pengumuman {
                id2: id2.to_string(),
                no_pengumuman: "001/X/2026".to_string(),
                tanggal: tanggal.to_string(),
                judul: "Judul".to_string(),
                jenis: "STOCK".to_string(),
                kode_emiten: ticker.to_string(),
            },
            attachments,
        }
    }

    fn attachment(filename: &str, is_supporting: bool) -> AttachmentInfo {
        AttachmentInfo {
            filename: filename.to_string(),
            url: format!("https://www.idx.co.id/StaticData/{filename}"),
            is_supporting,
        }
    }

    // -- parse_date / resolve_dates --------------------------------------------------

    #[test]
    fn parse_date_converts_iso_to_compact_form() {
        assert_eq!(parse_date("2026-09-01").unwrap(), "20260901");
    }

    #[test]
    fn parse_date_rejects_malformed_input() {
        let err = parse_date("09/01/2026").unwrap_err();
        assert!(format!("{err:#}").contains("invalid date"));
    }

    #[test]
    fn resolve_dates_defaults_to_full_history_through_today() {
        let core = CoreFilterArgs {
            date_from: None,
            date_to: None,
            ticker: None,
            keyword: None,
            security_type: None,
            lang: "id".to_string(),
        };
        let (from, to) = core.resolve_dates().unwrap();
        assert_eq!(from, "19010101");
        assert_eq!(to, chrono::Local::now().format("%Y%m%d").to_string());
    }

    #[test]
    fn resolve_dates_uses_explicit_bounds_when_given() {
        let core = CoreFilterArgs {
            date_from: Some("2026-01-01".to_string()),
            date_to: Some("2026-09-01".to_string()),
            ticker: None,
            keyword: None,
            security_type: None,
            lang: "id".to_string(),
        };
        let (from, to) = core.resolve_dates().unwrap();
        assert_eq!(from, "20260101");
        assert_eq!(to, "20260901");
    }

    #[test]
    fn resolve_dates_propagates_a_bad_date_from() {
        let core = CoreFilterArgs {
            date_from: Some("not-a-date".to_string()),
            date_to: None,
            ticker: None,
            keyword: None,
            security_type: None,
            lang: "id".to_string(),
        };
        assert!(core.resolve_dates().is_err());
    }

    // -- emiten_type --------------------------------------------------------------

    #[test]
    fn emiten_type_defaults_to_wildcard_when_unset() {
        let core = CoreFilterArgs {
            date_from: None,
            date_to: None,
            ticker: None,
            keyword: None,
            security_type: None,
            lang: "id".to_string(),
        };
        assert_eq!(core.emiten_type(), "*");
    }

    #[test]
    fn emiten_type_maps_every_security_type_variant() {
        let cases = [
            (SecurityType::Saham, "s"),
            (SecurityType::Obligasi, "o"),
            (SecurityType::Etf, "etf"),
            (SecurityType::DireDinfra, "dd"),
            (SecurityType::Eba, "eba"),
        ];
        for (variant, expected) in cases {
            let core = CoreFilterArgs {
                date_from: None,
                date_to: None,
                ticker: None,
                keyword: None,
                security_type: Some(variant),
                lang: "id".to_string(),
            };
            assert_eq!(core.emiten_type(), expected);
        }
    }

    // -- build_download_tasks ------------------------------------------------------

    #[test]
    fn build_download_tasks_includes_all_attachments_by_default() {
        let replies = vec![reply(
            "id1",
            "2026-09-01T18:02:45",
            "BBCA",
            vec![attachment("main.pdf", false), attachment("supporting.pdf", true)],
        )];

        let tasks = build_download_tasks(&replies, false);

        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn build_download_tasks_main_only_skips_supporting_attachments() {
        let replies = vec![reply(
            "id1",
            "2026-09-01T18:02:45",
            "BBCA",
            vec![attachment("main.pdf", false), attachment("supporting.pdf", true)],
        )];

        let tasks = build_download_tasks(&replies, true);

        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].dest_filename.contains("main.pdf"));
    }

    #[test]
    fn build_download_tasks_compacts_the_date_and_trims_the_ticker() {
        let replies = vec![reply(
            "id1",
            "2026-09-01T18:02:45",
            "BBCA                                                                                ",
            vec![attachment("main.pdf", false)],
        )];

        let tasks = build_download_tasks(&replies, false);

        assert_eq!(tasks[0].dest_filename, "20260901_BBCA_main.pdf");
    }

    #[test]
    fn build_download_tasks_accumulates_across_multiple_replies() {
        let replies = vec![
            reply("id1", "2026-09-01", "BBCA", vec![attachment("a.pdf", false)]),
            reply("id2", "2026-08-26", "TPIA", vec![attachment("b.pdf", false), attachment("c.pdf", true)]),
        ];

        let tasks = build_download_tasks(&replies, false);

        assert_eq!(tasks.len(), 3);
    }

    #[test]
    fn build_download_tasks_handles_a_reply_with_no_attachments() {
        let replies = vec![reply("id1", "2026-09-01", "BBCA", vec![])];

        let tasks = build_download_tasks(&replies, false);

        assert!(tasks.is_empty());
    }

    // -- CLI parsing ----------------------------------------------------------------

    #[test]
    fn cli_defaults_match_documented_values() {
        let cli = Cli::try_parse_from(["maddo", "fetch"]).unwrap();
        assert!(!cli.browser);
        assert_eq!(cli.browser_path, "/usr/bin/brave");
        assert!(!cli.headless);
        assert_eq!(cli.delay_ms, 800);

        let Command::Fetch(args) = &cli.command else { panic!("expected Fetch") };
        assert_eq!(args.filter.page, 1);
        assert_eq!(args.filter.pages, 1);
        assert_eq!(args.filter.page_size, 10);
        assert_eq!(args.filter.core.lang, "id");
    }

    #[test]
    fn cli_download_and_watch_defaults() {
        let cli = Cli::try_parse_from(["maddo", "download"]).unwrap();
        let Command::Download(args) = &cli.command else { panic!("expected Download") };
        assert_eq!(args.out_dir, PathBuf::from("./downloads"));
        assert_eq!(args.concurrency, 5);
        assert!(!args.main_only);

        let cli = Cli::try_parse_from(["maddo", "watch"]).unwrap();
        let Command::Watch(args) = &cli.command else { panic!("expected Watch") };
        assert_eq!(args.window, 20);
        assert_eq!(args.interval_secs, 30);
        assert!(!args.download);
        assert!(!args.json);
    }

    #[test]
    fn cli_live_defaults_to_port_8080() {
        let cli = Cli::try_parse_from(["maddo", "live"]).unwrap();
        let Command::Live(args) = &cli.command else { panic!("expected Live") };
        assert_eq!(args.port, 8080);
    }

    #[test]
    fn cli_live_accepts_an_explicit_port() {
        let cli = Cli::try_parse_from(["maddo", "live", "--port", "9000"]).unwrap();
        let Command::Live(args) = &cli.command else { panic!("expected Live") };
        assert_eq!(args.port, 9000);
    }

    #[test]
    fn cli_requires_a_subcommand() {
        assert!(Cli::try_parse_from(["maddo"]).is_err());
    }

    #[test]
    fn cli_rejects_an_unknown_security_type() {
        assert!(Cli::try_parse_from(["maddo", "fetch", "--type", "crypto"]).is_err());
    }

    #[test]
    fn cli_parses_a_known_security_type_through_to_emiten_type() {
        let cli = Cli::try_parse_from(["maddo", "fetch", "--type", "saham"]).unwrap();
        let Command::Fetch(args) = &cli.command else { panic!("expected Fetch") };
        assert_eq!(args.filter.core.emiten_type(), "s");
    }

    #[test]
    fn cli_global_browser_flag_applies_before_the_subcommand() {
        let cli = Cli::try_parse_from(["maddo", "--browser", "fetch"]).unwrap();
        assert!(cli.browser);
    }
}
