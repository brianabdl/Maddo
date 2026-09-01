// Default transport: `wreq` builds real Chrome TLS/HTTP2/JA3 handshakes (browser
// impersonation, not a from-scratch fake) so requests present the same fingerprint a
// current Chrome install would, without spawning one. This is a deliberate exception to
// the project's original browser-only stance — see the "IDX API transport" section of
// CLAUDE.md for why bare `reqwest`/`curl` no longer gets through and what this does and
// doesn't attempt beyond fingerprint matching. `browser.rs` remains as the `--browser`
// fallback for if/when this stops working.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use wreq::Client;
use wreq_util::Emulation;

const REFERER: &str = "https://www.idx.co.id/en/";

#[derive(Clone)]
pub struct HttpClient(Client);

impl HttpClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .emulation(Emulation::Chrome149)
            .cookie_store(true)
            .build()
            .context("building HTTP client")?;
        Ok(Self(client))
    }

    pub async fn get_json<T: DeserializeOwned>(&self, url: &str, query: &[(&str, &str)]) -> Result<T> {
        let resp = self
            .0
            .get(url)
            .query(query)
            .header("Referer", REFERER)
            .header("Accept", "application/json, text/plain, */*")
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("GET {url} returned HTTP {status}");
        }
        resp.json().await.context("parsing JSON response")
    }

    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self
            .0
            .get(url)
            .header("Referer", REFERER)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("GET {url} returned HTTP {status}");
        }
        Ok(resp.bytes().await.context("reading response body")?.to_vec())
    }
}
