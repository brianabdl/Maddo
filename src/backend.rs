// Picks the transport for a run: `Http` (default, `wreq` impersonation, no browser
// process) or `Browser` (the `--browser` fallback, chromiumoxide). Every call site above
// this module goes through `Backend` so `main.rs` doesn't need to branch on transport.

use crate::api::{self, ApiResponse, QueryParams};
use crate::browser;
use crate::download::{self, DownloadTask};
use crate::http::HttpClient;
use anyhow::Result;
use std::path::Path;

pub enum Backend {
    Http(HttpClient),
    Browser(browser::Session),
}

impl Backend {
    pub async fn open(browser_path: &str, headless: bool, use_browser: bool) -> Result<Self> {
        if use_browser {
            let session = browser::Session::open(browser_path, headless).await?;
            Ok(Backend::Browser(session))
        } else {
            Ok(Backend::Http(HttpClient::new()?))
        }
    }

    pub async fn fetch_announcements(&self, params: &QueryParams) -> Result<ApiResponse> {
        match self {
            Backend::Http(client) => api::fetch_announcements_http(client, params).await,
            Backend::Browser(session) => api::fetch_announcements(&session.page, params).await,
        }
    }

    pub async fn download_all(
        &self,
        tasks: &[DownloadTask],
        out_dir: &Path,
        concurrency: usize,
        delay_ms: u64,
    ) -> Result<(usize, usize)> {
        match self {
            Backend::Http(client) => {
                download::download_all(client, tasks, out_dir, concurrency, delay_ms).await
            }
            Backend::Browser(session) => {
                download::download_all_browser(&session.page, tasks, out_dir, concurrency, delay_ms).await
            }
        }
    }

    pub async fn close(self) -> Result<()> {
        match self {
            Backend::Http(_) => Ok(()),
            Backend::Browser(session) => session.close().await,
        }
    }
}
