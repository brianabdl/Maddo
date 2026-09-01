// Drives a real, unmodified Chromium-based browser over CDP. No TLS/JA3 spoofing, no
// stealth patches, no challenge-solving: Cloudflare's JS/managed challenge is left to
// resolve exactly as it would for a normal human visitor. If it ever refuses a headless
// session, that's the site's bot detection working as intended; don't try to defeat it.

use anyhow::{Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::Page;
use futures::StreamExt;
use std::time::Duration;

pub const TARGET_URL: &str = "https://www.idx.co.id/id/perusahaan-tercatat/keterbukaan-informasi";

pub struct Session {
    pub browser: Browser,
    pub page: Page,
    handler_task: tokio::task::JoinHandle<()>,
}

impl Session {
    pub async fn open(browser_path: &str, headless: bool) -> Result<Self> {
        let mut builder = BrowserConfig::builder()
            .chrome_executable(browser_path)
            .viewport(None);
        if !headless {
            builder = builder.with_head();
        }
        let config = builder
            .build()
            .map_err(|e| anyhow::anyhow!(e))
            .context("building browser config")?;

        let (browser, mut handler) = Browser::launch(config).await.context(
            "launching browser (is a Chromium-based browser installed at --browser-path?)",
        )?;

        let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        let page = browser.new_page(TARGET_URL).await.context("opening page")?;
        page.wait_for_navigation().await.context("initial navigation")?;

        wait_for_clearance(&page).await?;

        Ok(Self {
            browser,
            page,
            handler_task,
        })
    }

    pub async fn close(mut self) -> Result<()> {
        self.browser.close().await.ok();
        self.handler_task.abort();
        Ok(())
    }
}

/// Cloudflare's managed challenge runs its own JS and swaps in the real page once
/// cleared. Poll for real content instead of guessing a fixed sleep or trying to
/// detect/solve the challenge ourselves.
async fn wait_for_clearance(page: &Page) -> Result<()> {
    for _ in 0..30 {
        let count: u64 = page
            .evaluate("document.querySelectorAll('.attach-card').length")
            .await?
            .into_value()?;
        if count > 0 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let _ = page
        .save_screenshot(
            chromiumoxide::page::ScreenshotParams::builder()
                .format(chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat::Png)
                .full_page(true)
                .build(),
            "idx_challenge_debug.png",
        )
        .await;

    anyhow::bail!(
        "disclosure list never appeared after 30s, saved idx_challenge_debug.png for inspection"
    )
}
