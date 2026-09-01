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

const DEFAULT_REFERER: &str = "https://www.idx.co.id/id/";

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
        let referer = match query.iter().find(|(k, _)| *k == "lang") {
            Some((_, lang)) => format!("https://www.idx.co.id/{lang}/"),
            None => DEFAULT_REFERER.to_string(),
        };
        let resp = self
            .0
            .get(url)
            .query(query)
            .header("Referer", referer)
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
            .header("Referer", DEFAULT_REFERER)
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
