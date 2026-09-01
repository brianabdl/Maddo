// Downloads attachment files through the same authenticated browser tab used for
// listing: a same-origin `fetch()` inheriting real session cookies, exactly like the
// browser itself would do when a user clicks a link. No separate HTTP client, no
// cookie-jar copying, no custom TLS stack.

use anyhow::{Context, Result};
use base64::Engine;
use chromiumoxide::Page;
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

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

/// Downloads `tasks` in batches of `concurrency` (concurrent inside the page via
/// Promise.all, batches run sequentially with `delay_ms` between them so we don't
/// hammer the server).
pub async fn download_all(
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
