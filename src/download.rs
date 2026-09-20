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

use crate::api::Reply;
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

const MAX_TITLE_CHARS: usize = 100;

/// Makes `s` safe to use as (part of) a filename: whitespace runs collapse to one space,
/// characters that are reserved on common filesystems (or would break a
/// `Content-Disposition` header) become `_`, and the result is cut to `max_chars` and
/// stripped of leading/trailing spaces and dots.
pub fn sanitize_filename(s: &str, max_chars: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let safe: String = collapsed
        .chars()
        .map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .take(max_chars)
        .collect();
    safe.trim_matches(|c| c == ' ' || c == '.').to_string()
}

/// Readable filenames for every attachment of `reply`, in order. IDX's own filenames are
/// opaque hashes (`e45bf0b681_5f51678b83.pdf`), so the announcement title carries the
/// meaning: `<date>_<ticker>_<title>.<ext>`. A second main document gets `_2`, and each
/// supporting attachment gets `_attachment<N>`. Numbering runs over the whole reply, so a
/// name doesn't change when `--main-only` filters some attachments out.
pub fn attachment_filenames(reply: &Reply) -> Vec<String> {
    let p = &reply.pengumuman;
    let date: String = p.tanggal.chars().take(10).filter(char::is_ascii_digit).collect();
    let title = sanitize_filename(&p.judul, MAX_TITLE_CHARS);

    let (mut mains, mut supporting) = (0, 0);
    reply
        .attachments
        .iter()
        .map(|att| {
            let ext = Path::new(&att.filename)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| sanitize_filename(e, 10))
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| "pdf".to_string());
            let title = if title.is_empty() {
                let stem = Path::new(&att.filename).file_stem().and_then(|s| s.to_str());
                sanitize_filename(stem.unwrap_or_default(), MAX_TITLE_CHARS)
            } else {
                title.clone()
            };
            let suffix = if att.is_supporting {
                supporting += 1;
                format!("_attachment{supporting}")
            } else {
                mains += 1;
                if mains == 1 { String::new() } else { format!("_{mains}") }
            };
            format!("{date}_{}_{title}{suffix}.{ext}", sanitize_filename(p.ticker(), 20))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AttachmentInfo, Pengumuman};
    use tempfile::tempdir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn reply(judul: &str, attachments: Vec<(&str, bool)>) -> Reply {
        Reply {
            pengumuman: Pengumuman {
                id2: "id1".to_string(),
                no_pengumuman: "001/X/2026".to_string(),
                tanggal: "2026-09-01T18:02:45".to_string(),
                judul: judul.to_string(),
                jenis: "STOCK".to_string(),
                kode_emiten: "BBCA    ".to_string(),
            },
            attachments: attachments
                .into_iter()
                .map(|(filename, is_supporting)| AttachmentInfo {
                    filename: filename.to_string(),
                    url: format!("https://www.idx.co.id/StaticData/{filename}"),
                    is_supporting,
                })
                .collect(),
        }
    }

    #[test]
    fn attachment_filenames_use_date_ticker_and_title_not_the_hash() {
        let r = reply("Laporan Hasil Public Expose - Tahunan", vec![("e45bf0b681_5f51678b83.pdf", false)]);
        assert_eq!(
            attachment_filenames(&r),
            ["20260901_BBCA_Laporan Hasil Public Expose - Tahunan.pdf"]
        );
    }

    #[test]
    fn attachment_filenames_number_extra_documents_and_supporting_files() {
        let r = reply(
            "Judul",
            vec![("a.pdf", false), ("b.pdf", true), ("c.pdf", false), ("d.pdf", true)],
        );
        assert_eq!(
            attachment_filenames(&r),
            [
                "20260901_BBCA_Judul.pdf",
                "20260901_BBCA_Judul_attachment1.pdf",
                "20260901_BBCA_Judul_2.pdf",
                "20260901_BBCA_Judul_attachment2.pdf",
            ]
        );
    }

    #[test]
    fn attachment_filenames_keep_the_original_extension_and_default_to_pdf() {
        let r = reply("Judul", vec![("a.XLSX", true), ("noext", true)]);
        assert_eq!(
            attachment_filenames(&r),
            ["20260901_BBCA_Judul_attachment1.XLSX", "20260901_BBCA_Judul_attachment2.pdf"]
        );
    }

    #[test]
    fn attachment_filenames_fall_back_to_the_original_stem_for_a_blank_title() {
        let r = reply("  ", vec![("e45bf0b681_5f51678b83.pdf", false)]);
        assert_eq!(attachment_filenames(&r), ["20260901_BBCA_e45bf0b681_5f51678b83.pdf"]);
    }

    #[test]
    fn sanitize_filename_replaces_reserved_characters_and_collapses_whitespace() {
        assert_eq!(sanitize_filename("A/B\\C: D?  \"E\"\n<F>|", 100), "A_B_C_ D_ _E_ _F__");
    }

    #[test]
    fn sanitize_filename_truncates_and_trims_dots_and_spaces() {
        assert_eq!(sanitize_filename("abcdefgh", 4), "abcd");
        assert_eq!(sanitize_filename("abc def", 4), "abc");
        assert_eq!(sanitize_filename(" ..name.. ", 100), "name");
        assert_eq!(sanitize_filename("../../etc", 100), "_.._etc");
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
