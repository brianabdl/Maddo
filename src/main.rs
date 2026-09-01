//! Maddo: a CLI for IDX's public "Keterbukaan Informasi" (listed-company disclosures) feed.
//!
//! Drives a real, unmodified Chromium-based browser over CDP so Cloudflare's
//! JS/managed challenge on idx.co.id resolves exactly as it would for a normal human
//! visitor. Every subsequent request (listing, filtering, downloading) goes through
//! same-origin `fetch()` calls executed *inside* that already-cleared browser tab, so
//! they inherit its genuine session cookies. This is session reuse, not evasion: no
//! TLS/JA3 spoofing, no stealth patches, no challenge-solving, no headless workarounds.
//! Headless mode is left unsupported on purpose: if Cloudflare blocks it, that's its
//! bot detection doing its job.

mod api;
mod browser;
mod download;

use anyhow::{Context, Result};
use api::QueryParams;
use chrono::NaiveDate;
use clap::{Args, Parser, Subcommand};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

/// Maddo: fetch, watch, and download IDX listed-company disclosures.
#[derive(Parser)]
#[command(name = "maddo", version, about)]
struct Cli {
    /// Path to a Chromium-based browser executable (Chrome, Chromium, Brave, Edge...).
    #[arg(long, global = true, default_value = "/usr/bin/brave")]
    browser_path: String,

    /// Run the browser headless. Cloudflare's challenge frequently blocks headless
    /// sessions; this is not worked around here on purpose. Default is headed.
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Command::Fetch(args) => run_fetch(&cli, args).await,
        Command::Download(args) => run_download(&cli, args).await,
        Command::Watch(args) => run_watch(&cli, args).await,
    }
}

async fn fetch_filtered(
    page: &chromiumoxide::Page,
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

        let resp = api::fetch_announcements(page, &params).await?;
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
    let session = browser::Session::open(&cli.browser_path, cli.headless).await?;
    let replies = fetch_filtered(&session.page, &args.filter, cli.delay_ms).await?;
    session.close().await?;

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
    let session = browser::Session::open(&cli.browser_path, cli.headless).await?;

    let replies = if let Some(path) = &args.from_json {
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&data).context("parsing --from-json file")?
    } else {
        fetch_filtered(&session.page, &args.filter, cli.delay_ms).await?
    };

    let tasks = build_download_tasks(&replies, args.main_only);

    eprintln!("Downloading {} file(s) to {}...", tasks.len(), args.out_dir.display());
    let (ok, err) =
        download::download_all(&session.page, &tasks, &args.out_dir, args.concurrency, cli.delay_ms)
            .await?;
    session.close().await?;

    eprintln!("Done: {ok} succeeded, {err} failed.");
    if err > 0 && ok == 0 {
        anyhow::bail!("all downloads failed");
    }
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
    let session = browser::Session::open(&cli.browser_path, cli.headless).await?;

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
                if let Err(e) = poll_once(&session.page, args, &mut seen, &mut first_poll, cli.delay_ms).await {
                    eprintln!("Poll failed: {e:#}. Retrying next interval.");
                }
            }
        }
    }

    session.close().await
}

async fn poll_once(
    page: &chromiumoxide::Page,
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

    let resp = api::fetch_announcements(page, &params).await?;

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
            let (ok, err) =
                download::download_all(page, &tasks, &args.out_dir, args.concurrency, delay_ms).await?;
            eprintln!("  {ok} succeeded, {err} failed.");
        }
    }

    Ok(())
}
