// Default transport: `wreq` builds real browser TLS/HTTP2/JA3 handshakes (impersonation,
// not a from-scratch fake) so requests present a fingerprint a real, currently common
// browser would, without spawning one. This is a deliberate exception to the project's
// original browser-only stance — see the "IDX API transport" section of CLAUDE.md for why
// bare `reqwest`/`curl` no longer gets through and what this does and doesn't attempt
// beyond fingerprint matching. `browser.rs` remains as the `--browser` fallback for if/when
// this stops working.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;
use wreq::{Client, RequestBuilder, Response, StatusCode};
use wreq_util::Profile;

const DEFAULT_REFERER: &str = "https://www.idx.co.id/id/";
const PROBE_URL: &str = "https://www.idx.co.id/primary/ListedCompany/GetAnnouncement";

/// Minimum spacing between requests, enforced across every clone of a given `HttpClient`
/// (they share one timer). IDX/Cloudflare rate-limits with `429 Too Many Requests` if
/// requests come in too fast, which a burst like `full_scan`'s ~130 probes or a
/// concurrent `download` batch can trigger without this.
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(300);

/// How many times `send` retries a `429` before giving up and returning it as a normal
/// response for the caller's own status check to turn into an error.
const MAX_429_RETRIES: u32 = 5;

/// Cap on how many working fingerprints `full_scan` keeps. Once this many are found it
/// stops probing the rest of `Profile::VARIANTS`, since the pool only needs a handful of
/// fallbacks, not every fingerprint IDX currently accepts.
const MAX_POOL_SIZE: usize = 5;

/// Where `autodetect` persists the pool of fingerprints last known to clear Cloudflare,
/// so a warm run can skip straight to one instead of rescanning every profile.
fn pool_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
            PathBuf::from(home).join(".cache")
        });
    base.join("maddo").join("emulation_pool.json")
}

/// Profile derives Debug but not (de)serialize in this build, so the pool is persisted as
/// each variant's Debug name (e.g. "Chrome149") rather than pulling in wreq-util's
/// `emulation-serde` feature for a two-field cache file.
fn profile_name(profile: Profile) -> String {
    format!("{profile:?}")
}

fn profile_by_name(name: &str) -> Option<Profile> {
    Profile::VARIANTS.iter().copied().find(|p| profile_name(*p) == name)
}

fn load_pool() -> Vec<Profile> {
    let Ok(data) = std::fs::read_to_string(pool_path()) else {
        return Vec::new();
    };
    let Ok(names) = serde_json::from_str::<Vec<String>>(&data) else {
        return Vec::new();
    };
    names.iter().filter_map(|n| profile_by_name(n)).collect()
}

/// Best-effort: a failed write just means the next run rescans instead of reusing the
/// pool, not a correctness problem, so errors here are swallowed rather than surfaced.
fn save_pool(pool: &[Profile]) {
    let path = pool_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let names: Vec<String> = pool.iter().copied().map(profile_name).collect();
    if let Ok(data) = serde_json::to_string_pretty(&names) {
        let _ = std::fs::write(path, data);
    }
}

#[derive(Clone)]
pub struct HttpClient {
    client: Client,
    /// Timestamp of this client's last request, shared across all its clones so a
    /// concurrent download batch is paced as one stream, not one per clone.
    last_request: Arc<Mutex<Option<Instant>>>,
}

impl HttpClient {
    /// Builds a client with the newest Chrome fingerprint and no network call. Only
    /// used by tests, which point requests at a mock server and don't need
    /// `autodetect`'s live probing.
    #[cfg(test)]
    pub(crate) fn new() -> Result<Self> {
        Self::with_emulation(Profile::Chrome149)
    }

    fn with_emulation(emulation: Profile) -> Result<Self> {
        let client = Client::builder()
            .emulation(emulation)
            .cookie_store(true)
            .build()
            .context("building HTTP client")?;
        Ok(Self { client, last_request: Arc::new(Mutex::new(None)) })
    }

    /// Sleeps out whatever's left of `MIN_REQUEST_INTERVAL` since this client's (or any
    /// of its clones') last request.
    async fn throttle(&self) {
        let mut last = self.last_request.lock().await;
        if let Some(previous) = *last {
            let elapsed = previous.elapsed();
            if elapsed < MIN_REQUEST_INTERVAL {
                tokio::time::sleep(MIN_REQUEST_INTERVAL - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }

    /// Sends one request, paced by `throttle`, retrying on `429 Too Many Requests` (up to
    /// `MAX_429_RETRIES` times) instead of surfacing it straight to the caller. Honors a
    /// `Retry-After` header when IDX sends one, otherwise backs off exponentially. `build`
    /// is called fresh on every attempt because a `RequestBuilder` is consumed by `send`.
    async fn send(&self, url: &str, build: impl Fn(&Client) -> RequestBuilder) -> Result<Response> {
        for attempt in 0..=MAX_429_RETRIES {
            self.throttle().await;
            let resp = build(&self.client).send().await.with_context(|| format!("GET {url}"))?;
            if resp.status() != StatusCode::TOO_MANY_REQUESTS || attempt == MAX_429_RETRIES {
                return Ok(resp);
            }
            let wait = retry_after(&resp).unwrap_or_else(|| Duration::from_secs(1 << attempt.min(5)));
            eprintln!(
                "GET {url}: rate limited (429), retrying in {}s ({}/{MAX_429_RETRIES})...",
                wait.as_secs(),
                attempt + 1
            );
            tokio::time::sleep(wait).await;
        }
        unreachable!("loop always returns by the attempt == MAX_429_RETRIES branch")
    }

    /// Tries every entry in `pool`, oldest-confirmed-first, pruning (and persisting the
    /// prune of) any that now get rejected, and returns the first client that still
    /// clears Cloudflare. Returns `Ok(None)` with `pool` left empty if none do.
    async fn try_pool(pool: &mut Vec<Profile>) -> Result<Option<Self>> {
        while !pool.is_empty() {
            let profile = pool[0];
            let client = Self::with_emulation(profile)?;
            if client.probe().await.is_ok() {
                return Ok(Some(client));
            }
            pool.remove(0);
            save_pool(pool);
        }
        Ok(None)
    }

    /// Probes known browser fingerprints (`Profile` variants other than the non-browser
    /// OkHttp family) against IDX's real GetAnnouncement endpoint and collects up to
    /// `MAX_POOL_SIZE` that clear Cloudflare, stopping early once it has enough, for
    /// `autodetect` to rebuild its pool from once the persisted one has shrunk to nothing
    /// usable.
    async fn full_scan() -> Vec<Profile> {
        let mut good = Vec::new();
        for &profile in Profile::VARIANTS.iter() {
            if good.len() >= MAX_POOL_SIZE {
                break;
            }
            if let Ok(client) = Self::with_emulation(profile) {
                if client.probe().await.is_ok() {
                    good.push(profile);
                }
            }
        }
        good
    }

    /// Reuses the persisted pool of fingerprints last known to clear Cloudflare
    /// (`~/.cache/maddo/emulation_pool.json`, or `$XDG_CACHE_HOME` if set), pruning any
    /// that now get rejected as it goes. Once the pool has shrunk to zero or one entries
    /// (too thin to be a reliable signal on its own), it's rebuilt from a full scan of
    /// every known browser fingerprint before trying continues. Cloudflare's acceptance
    /// of any single fingerprint isn't stable over time (see CLAUDE.md's "IDX API
    /// transport" section), so a cold run, or a run where the whole pool has gone stale,
    /// pays for a full rescan; a warm run typically only needs its first probe to
    /// succeed.
    pub async fn autodetect() -> Result<Self> {
        let mut pool = load_pool();
        if pool.len() <= 1 {
            pool = Self::full_scan().await;
            save_pool(&pool);
        }
        if let Some(client) = Self::try_pool(&mut pool).await? {
            return Ok(client);
        }

        // The persisted pool is exhausted (including possibly right after a full scan,
        // if Cloudflare is rejecting everything it used to accept). Rescan once more
        // before giving up.
        pool = Self::full_scan().await;
        save_pool(&pool);
        if let Some(client) = Self::try_pool(&mut pool).await? {
            return Ok(client);
        }

        anyhow::bail!(
            "no browser fingerprint got past Cloudflare on {PROBE_URL} after scanning all {} known profiles",
            Profile::VARIANTS.len()
        );
    }

    async fn probe(&self) -> Result<()> {
        let resp = self
            .send(PROBE_URL, |client| {
                client
                    .get(PROBE_URL)
                    .query(&[
                        ("kodeEmiten", ""),
                        ("emitenType", "*"),
                        ("indexFrom", "0"),
                        ("pageSize", "1"),
                        ("dateFrom", ""),
                        ("dateTo", ""),
                        ("lang", "id"),
                        ("keyword", ""),
                    ])
                    .header("Referer", DEFAULT_REFERER)
                    .header("Accept", "application/json, text/plain, */*")
            })
            .await?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            anyhow::bail!("HTTP {status}")
        }
    }

    pub async fn get_json<T: DeserializeOwned>(&self, url: &str, query: &[(&str, &str)]) -> Result<T> {
        let referer = match query.iter().find(|(k, _)| *k == "lang") {
            Some((_, lang)) => format!("https://www.idx.co.id/{lang}/"),
            None => DEFAULT_REFERER.to_string(),
        };
        let resp = self
            .send(url, |client| {
                client
                    .get(url)
                    .query(query)
                    .header("Referer", referer.as_str())
                    .header("Accept", "application/json, text/plain, */*")
            })
            .await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("GET {url} returned HTTP {status}");
        }
        resp.json().await.context("parsing JSON response")
    }

    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self
            .send(url, |client| client.get(url).header("Referer", DEFAULT_REFERER))
            .await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("GET {url} returned HTTP {status}");
        }
        Ok(resp.bytes().await.context("reading response body")?.to_vec())
    }
}

/// Parses a `Retry-After` header as whole seconds (the form Cloudflare sends; the
/// HTTP-date form isn't handled since IDX hasn't been observed to send it).
fn retry_after(resp: &Response) -> Option<Duration> {
    resp.headers().get("retry-after")?.to_str().ok()?.parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Debug, Deserialize, PartialEq)]
    struct Payload {
        ok: bool,
    }

    // Issues one raw request per case (bypassing `HttpClient::send`'s own retry loop, which
    // would otherwise turn a persistently-429 mock into a slow, multi-second test) just to
    // get a real `Response` with the header set, then checks `retry_after`'s parsing of it.
    #[tokio::test]
    async fn retry_after_parses_a_whole_seconds_header_and_defaults_to_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/limited"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/limited-no-header"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let raw = wreq::Client::new();
        let with_header = raw.get(format!("{}/limited", server.uri())).send().await.unwrap();
        let without_header = raw.get(format!("{}/limited-no-header", server.uri())).send().await.unwrap();

        assert_eq!(retry_after(&with_header), Some(Duration::from_secs(2)));
        assert_eq!(retry_after(&without_header), None);
    }

    #[test]
    fn profile_name_and_profile_by_name_round_trip_for_every_variant() {
        for &profile in Profile::VARIANTS {
            let name = profile_name(profile);
            assert_eq!(
                profile_by_name(&name),
                Some(profile),
                "profile_by_name(profile_name({profile:?})) should return the same variant"
            );
        }
    }

    #[test]
    fn profile_by_name_rejects_an_unknown_name() {
        assert_eq!(profile_by_name("NotARealBrowser"), None);
    }

    // Hits the real, Cloudflare-protected idx.co.id endpoint (once per candidate
    // fingerprint until one clears it). Not run by default (would fail offline/in CI);
    // run explicitly with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn autodetect_finds_a_working_fingerprint_against_real_idx() {
        HttpClient::autodetect()
            .await
            .expect("at least one known browser fingerprint should clear Cloudflare");
    }

    #[tokio::test]
    async fn get_json_deserializes_a_successful_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/thing"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let client = HttpClient::new().unwrap();
        let got: Payload = client
            .get_json(&format!("{}/thing", server.uri()), &[])
            .await
            .unwrap();

        assert_eq!(got, Payload { ok: true });
    }

    #[tokio::test]
    async fn get_json_sends_the_query_params_and_referer_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/thing"))
            .and(query_param("a", "1"))
            .and(query_param("b", "two words"))
            .and(query_param("lang", "en"))
            .and(header("Referer", "https://www.idx.co.id/en/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let client = HttpClient::new().unwrap();
        let got: Payload = client
            .get_json(
                &format!("{}/thing", server.uri()),
                &[("a", "1"), ("b", "two words"), ("lang", "en")],
            )
            .await
            .unwrap();

        assert_eq!(got, Payload { ok: true });
    }

    #[tokio::test]
    async fn get_json_defaults_referer_to_id_when_no_lang_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/thing"))
            .and(header("Referer", DEFAULT_REFERER))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let client = HttpClient::new().unwrap();
        let got: Payload = client.get_json(&format!("{}/thing", server.uri()), &[]).await.unwrap();

        assert_eq!(got, Payload { ok: true });
    }

    #[tokio::test]
    async fn get_json_errors_on_non_success_status_without_parsing_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/blocked"))
            .respond_with(ResponseTemplate::new(403).set_body_string("<html>Just a moment...</html>"))
            .mount(&server)
            .await;

        let client = HttpClient::new().unwrap();
        let err = client
            .get_json::<Payload>(&format!("{}/blocked", server.uri()), &[])
            .await
            .expect_err("403 must surface as an error, not a parse attempt on HTML");

        assert!(format!("{err:#}").contains("403"));
    }

    #[tokio::test]
    async fn get_json_errors_on_malformed_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/bad-json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let client = HttpClient::new().unwrap();
        let result = client.get_json::<Payload>(&format!("{}/bad-json", server.uri()), &[]).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_bytes_returns_the_exact_response_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/file.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0x25, 0x50, 0x44, 0x46]))
            .mount(&server)
            .await;

        let client = HttpClient::new().unwrap();
        let bytes = client.get_bytes(&format!("{}/file.pdf", server.uri())).await.unwrap();

        assert_eq!(bytes, vec![0x25, 0x50, 0x44, 0x46]);
    }

    #[tokio::test]
    async fn get_bytes_errors_on_non_success_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gone"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = HttpClient::new().unwrap();
        let err = client
            .get_bytes(&format!("{}/gone", server.uri()))
            .await
            .expect_err("404 must be an error");

        assert!(format!("{err:#}").contains("404"));
    }
}
