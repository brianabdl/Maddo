// Downloads attachment files. `download_all` (default) uses the impersonating
// `HttpClient` from `http.rs`. `download_all_browser` (the `--browser` fallback) goes
// through the same authenticated browser tab used for listing: a same-origin `fetch()`
// inheriting real session cookies, exactly like the browser itself would do when a user
// clicks a link.

use anyhow::{Context, Result};
use base64::Engine;
use chromiumoxide::Page;
use futures::future::join_all;
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

use crate::http::HttpClient;

pub struct DownloadTask {
    pub url: String,
    pub dest_filename: String,
}

#[derive(Debug, Deserialize)]
struct RawResult {
    url: String,
    base64: Option<String>,
    error: Option<String>,
}

/// Downloads `tasks` in batches of `concurrency` (concurrent requests via
/// `futures::join_all`, batches run sequentially with `delay_ms` between them so we
/// don't hammer the server).
pub async fn download_all(
    client: &HttpClient,
    tasks: &[DownloadTask],
    out_dir: &Path,
    concurrency: usize,
    delay_ms: u64,
) -> Result<(usize, usize)> {
    std::fs::create_dir_all(out_dir).context("creating output directory")?;

    let mut ok_count = 0;
    let mut err_count = 0;

    for (batch_idx, batch) in tasks.chunks(concurrency.max(1)).enumerate() {
        if batch_idx > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        let results = join_all(batch.iter().map(|task| async move {
            (task, client.get_bytes(&task.url).await)
        }))
        .await;

        for (task, result) in results {
            match result {
                Ok(bytes) => {
                    let dest = out_dir.join(&task.dest_filename);
                    std::fs::write(&dest, &bytes)
                        .with_context(|| format!("writing {}", dest.display()))?;
                    eprintln!("  ✓ {} ({} bytes)", task.dest_filename, bytes.len());
                    ok_count += 1;
                }
                Err(e) => {
                    eprintln!("  ! {}: {e:#}", task.dest_filename);
                    err_count += 1;
                }
            }
        }
    }

    Ok((ok_count, err_count))
}

/// Same as `download_all`, but through the `--browser` fallback's already-cleared tab.
pub async fn download_all_browser(
    page: &Page,
    tasks: &[DownloadTask],
    out_dir: &Path,
    concurrency: usize,
    delay_ms: u64,
) -> Result<(usize, usize)> {
    std::fs::create_dir_all(out_dir).context("creating output directory")?;

    let mut ok_count = 0;
    let mut err_count = 0;

    for (batch_idx, batch) in tasks.chunks(concurrency.max(1)).enumerate() {
        if batch_idx > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        let urls: Vec<&str> = batch.iter().map(|t| t.url.as_str()).collect();
        let results = fetch_batch(page, &urls).await?;

        for task in batch {
            let Some(result) = results.iter().find(|r| r.url == task.url) else {
                eprintln!("  ! {}: no result returned", task.dest_filename);
                err_count += 1;
                continue;
            };

            if let Some(err) = &result.error {
                eprintln!("  ! {}: {}", task.dest_filename, err);
                err_count += 1;
                continue;
            }

            let Some(b64) = &result.base64 else {
                eprintln!("  ! {}: empty response", task.dest_filename);
                err_count += 1;
                continue;
            };

            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .context("decoding base64 payload")?;
            let dest = out_dir.join(&task.dest_filename);
            std::fs::write(&dest, &bytes)
                .with_context(|| format!("writing {}", dest.display()))?;
            eprintln!("  ✓ {} ({} bytes)", task.dest_filename, bytes.len());
            ok_count += 1;
        }
    }

    Ok((ok_count, err_count))
}

/// Fetches one URL's bytes through the browser tab without writing them to disk:
/// what the `live` server's file proxy needs when the `--browser` transport is active.
pub async fn fetch_bytes_browser(page: &Page, url: &str) -> Result<Vec<u8>> {
    let result = fetch_batch(page, &[url])
        .await?
        .into_iter()
        .next()
        .context("no result returned")?;
    if let Some(err) = result.error {
        anyhow::bail!("fetching {url}: {err}");
    }
    let b64 = result.base64.context("empty response")?;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("decoding base64 payload")
}

async fn fetch_batch(page: &Page, urls: &[&str]) -> Result<Vec<RawResult>> {
    let urls_json = serde_json::to_string(urls)?;
    let js = format!(
        r#"(async () => {{
            const urls = {urls_json};
            return await Promise.all(urls.map(async (url) => {{
                try {{
                    const res = await fetch(url, {{ credentials: 'include' }});
                    if (!res.ok) {{ return {{ url, error: 'HTTP ' + res.status }}; }}
                    const buf = await res.arrayBuffer();
                    const bytes = new Uint8Array(buf);
                    let binary = '';
                    const chunkSize = 0x8000;
                    for (let i = 0; i < bytes.length; i += chunkSize) {{
                        binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunkSize));
                    }}
                    return {{ url, base64: btoa(binary) }};
                }} catch (e) {{
                    return {{ url, error: String(e) }};
                }}
            }}));
        }})()"#
    );

    page.evaluate(js)
        .await
        .context("running batch download script")?
        .into_value()
        .context("parsing batch download result")
}

/// Sanitizes an announcement into a filesystem-safe, collision-resistant filename.
pub fn dest_filename(date_compact: &str, ticker: &str, original: &str) -> String {
    let safe = |s: &str| s.chars().map(|c| if c == '/' || c == '\\' { '_' } else { c }).collect::<String>();
    format!("{}_{}_{}", date_compact, safe(ticker), safe(original))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn dest_filename_joins_date_ticker_and_original_name() {
        assert_eq!(
            dest_filename("20260901", "BBCA", "report.pdf"),
            "20260901_BBCA_report.pdf"
        );
    }

    #[test]
    fn dest_filename_sanitizes_slashes_in_ticker_and_original_name() {
        // Neither field should normally contain a path separator, but if IDX ever sends
        // one, it must not escape `out_dir`.
        assert_eq!(
            dest_filename("20260901", "AB/CD", "na\\me/x.pdf"),
            "20260901_AB_CD_na_me_x.pdf"
        );
    }

    fn task(url: &str, dest_filename: &str) -> DownloadTask {
        DownloadTask {
            url: url.to_string(),
            dest_filename: dest_filename.to_string(),
        }
    }

    #[tokio::test]
    async fn download_all_writes_successful_files_and_counts_them() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/a.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PDFDATA".to_vec()))
            .mount(&server)
            .await;

        let client = crate::http::HttpClient::new().unwrap();
        let out_dir = tempdir().unwrap();
        let tasks = vec![task(&format!("{}/a.pdf", server.uri()), "a.pdf")];

        let (ok, err) = download_all(&client, &tasks, out_dir.path(), 5, 0).await.unwrap();

        assert_eq!((ok, err), (1, 0));
        let bytes = std::fs::read(out_dir.path().join("a.pdf")).unwrap();
        assert_eq!(bytes, b"PDFDATA");
    }

    #[tokio::test]
    async fn download_all_counts_http_errors_without_writing_a_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing.pdf"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = crate::http::HttpClient::new().unwrap();
        let out_dir = tempdir().unwrap();
        let tasks = vec![task(&format!("{}/missing.pdf", server.uri()), "missing.pdf")];

        let (ok, err) = download_all(&client, &tasks, out_dir.path(), 5, 0).await.unwrap();

        assert_eq!((ok, err), (0, 1));
        assert!(!out_dir.path().join("missing.pdf").exists());
    }

    #[tokio::test]
    async fn download_all_handles_a_mixed_batch_independently() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"OK".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/bad.pdf"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = crate::http::HttpClient::new().unwrap();
        let out_dir = tempdir().unwrap();
        let tasks = vec![
            task(&format!("{}/ok.pdf", server.uri()), "ok.pdf"),
            task(&format!("{}/bad.pdf", server.uri()), "bad.pdf"),
        ];

        // concurrency=1 forces two sequential single-item batches; both outcomes must
        // still be reported correctly regardless of batching.
        let (ok, err) = download_all(&client, &tasks, out_dir.path(), 1, 0).await.unwrap();

        assert_eq!((ok, err), (1, 1));
        assert!(out_dir.path().join("ok.pdf").exists());
        assert!(!out_dir.path().join("bad.pdf").exists());
    }

    #[tokio::test]
    async fn download_all_creates_out_dir_if_missing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/a.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"X".to_vec()))
            .mount(&server)
            .await;

        let client = crate::http::HttpClient::new().unwrap();
        let out_dir = tempdir().unwrap();
        let nested = out_dir.path().join("nested").join("dir");
        let tasks = vec![task(&format!("{}/a.pdf", server.uri()), "a.pdf")];

        download_all(&client, &tasks, &nested, 5, 0).await.unwrap();

        assert!(nested.join("a.pdf").exists());
    }

    // Hits the real, Cloudflare-protected StaticData PDF host. Not run by default (would
    // make `cargo test` flaky/network-dependent in CI); verify manually with
    // `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn download_all_fetches_a_real_pdf_from_idx() {
        let client = crate::http::HttpClient::new().unwrap();
        let params = crate::api::QueryParams {
            ticker: Some("BBCA".to_string()),
            page_size: 1,
            ..Default::default()
        };
        let resp = crate::api::fetch_announcements_http(&client, &params)
            .await
            .expect("live GetAnnouncement call should succeed");
        let url = resp.replies[0]
            .attachments
            .first()
            .expect("expected the latest BBCA announcement to have an attachment")
            .url
            .clone();

        let out_dir = tempdir().unwrap();
        let tasks = vec![task(&url, "live.pdf")];

        let (ok, err) = download_all(&client, &tasks, out_dir.path(), 1, 0).await.unwrap();

        assert_eq!((ok, err), (1, 0));
        let bytes = std::fs::read(out_dir.path().join("live.pdf")).unwrap();
        assert!(bytes.starts_with(b"%PDF"), "expected real PDF magic bytes");
    }
}
