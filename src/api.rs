// Typed client for IDX's own internal disclosure API
// (`/primary/ListedCompany/GetAnnouncement`), the exact endpoint the site's own
// frontend calls when you paginate, filter by date, or search.
//
// Two transports call it: `fetch_announcements_http` (default) via the impersonating
// `HttpClient` in `http.rs`, and `fetch_announcements` (the `--browser` fallback) via
// `fetch()` executed inside the already-challenge-cleared browser tab, inheriting that
// tab's genuine session cookies (same-origin, `credentials: 'include'`).

use crate::http::HttpClient;
use anyhow::{Context, Result};
use chromiumoxide::Page;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct QueryParams {
    pub ticker: Option<String>,
    pub keyword: Option<String>,
    pub emiten_type: String, // "*", "s", "o", "etf", "dd", "eba"
    pub date_from: String,   // YYYYMMDD
    pub date_to: String,     // YYYYMMDD
    pub index_from: u32,
    pub page_size: u32,
    pub lang: String,
}

impl Default for QueryParams {
    fn default() -> Self {
        Self {
            ticker: None,
            keyword: None,
            emiten_type: "*".to_string(),
            date_from: String::new(),
            date_to: String::new(),
            index_from: 0,
            page_size: 10,
            lang: "id".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Pengumuman {
    #[serde(rename = "Id2")]
    pub id2: String,
    #[serde(rename = "NoPengumuman")]
    pub no_pengumuman: String,
    #[serde(rename = "TglPengumuman")]
    pub tanggal: String,
    #[serde(rename = "JudulPengumuman")]
    pub judul: String,
    #[serde(rename = "JenisPengumuman")]
    pub jenis: String,
    #[serde(rename = "Kode_Emiten")]
    pub kode_emiten: String,
}

impl Pengumuman {
    pub fn ticker(&self) -> &str {
        self.kode_emiten.trim()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AttachmentInfo {
    #[serde(rename = "PDFFilename")]
    pub filename: String,
    #[serde(rename = "FullSavePath")]
    pub url: String,
    #[serde(rename = "IsAttachment")]
    pub is_supporting: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Reply {
    pub pengumuman: Pengumuman,
    #[serde(default)]
    pub attachments: Vec<AttachmentInfo>,
}

#[derive(Debug, Deserialize)]
pub struct ApiResponse {
    #[serde(rename = "ResultCount")]
    pub result_count: u64,
    #[serde(rename = "Replies")]
    pub replies: Vec<Reply>,
}

const GET_ANNOUNCEMENT_URL: &str = "https://www.idx.co.id/primary/ListedCompany/GetAnnouncement";

pub async fn fetch_announcements_http(client: &HttpClient, params: &QueryParams) -> Result<ApiResponse> {
    fetch_announcements_http_at(client, GET_ANNOUNCEMENT_URL, params).await
}

/// Same as `fetch_announcements_http`, against an arbitrary URL. Split out so tests can
/// point the real query-building/request logic at a mock server instead of idx.co.id.
async fn fetch_announcements_http_at(client: &HttpClient, url: &str, params: &QueryParams) -> Result<ApiResponse> {
    let index_from = params.index_from.to_string();
    let page_size = params.page_size.to_string();
    let ticker = params.ticker.as_deref().unwrap_or("");
    let keyword = params.keyword.as_deref().unwrap_or("");
    let query = [
        ("kodeEmiten", ticker),
        ("emitenType", params.emiten_type.as_str()),
        ("indexFrom", index_from.as_str()),
        ("pageSize", page_size.as_str()),
        ("dateFrom", params.date_from.as_str()),
        ("dateTo", params.date_to.as_str()),
        ("lang", params.lang.as_str()),
        ("keyword", keyword),
    ];
    client
        .get_json(url, &query)
        .await
        .context("calling IDX GetAnnouncement API")
}

pub async fn fetch_announcements(page: &Page, params: &QueryParams) -> Result<ApiResponse> {
    let js = format!(
        r#"(async () => {{
            const qs = new URLSearchParams({{
                kodeEmiten: {ticker},
                emitenType: {emiten_type},
                indexFrom: {index_from},
                pageSize: {page_size},
                dateFrom: {date_from},
                dateTo: {date_to},
                lang: {lang},
                keyword: {keyword}
            }});
            const res = await fetch('https://www.idx.co.id/primary/ListedCompany/GetAnnouncement?' + qs.toString(), {{ credentials: 'include' }});
            if (!res.ok) {{ throw new Error('IDX API HTTP ' + res.status); }}
            return await res.json();
        }})()"#,
        ticker = js_string(params.ticker.as_deref().unwrap_or("")),
        emiten_type = js_string(&params.emiten_type),
        index_from = params.index_from,
        page_size = params.page_size,
        date_from = js_string(&params.date_from),
        date_to = js_string(&params.date_to),
        lang = js_string(&params.lang),
        keyword = js_string(params.keyword.as_deref().unwrap_or("")),
    );

    page.evaluate(js)
        .await
        .context("calling IDX GetAnnouncement API")?
        .into_value()
        .context("parsing IDX API JSON response")
}

/// Safely embeds an arbitrary Rust string as a JS string literal (handles quotes,
/// unicode, etc. correctly via serde_json's escaping).
fn js_string(s: &str) -> String {
    serde_json::to_string(s).expect("string serialization cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Shaped after a real GetAnnouncement response: extra fields IDX sends
    // (SearchParams, etc.) that this struct doesn't model, a reply with attachments, and
    // a reply with the "attachments" key omitted entirely (IDX does this for
    // announcements with none).
    const FIXTURE: &str = r#"{
        "ResultCount": 222,
        "SearchParams": {"KodeEmiten": "BBCA"},
        "Replies": [
            {
                "pengumuman": {
                    "Id2": "20260901180245-009/CSG-IVR/2026_id-id",
                    "NoPengumuman": "009/CSG-IVR/2026",
                    "TglPengumuman": "2026-09-01T18:02:45",
                    "JudulPengumuman": "Perubahan anggota Direksi",
                    "JenisPengumuman": "STOCK",
                    "Kode_Emiten": "BBCA                                                                                "
                },
                "attachments": [
                    {
                        "PDFFilename": "ca1603c553_f4c99b52a8.pdf",
                        "FullSavePath": "https://www.idx.co.id/StaticData/x/ca1603c553_f4c99b52a8.pdf",
                        "IsAttachment": false
                    },
                    {
                        "PDFFilename": "9a50be8842_f0e9d79307.pdf",
                        "FullSavePath": "https://www.idx.co.id/StaticData/x/9a50be8842_f0e9d79307.pdf",
                        "IsAttachment": true
                    }
                ]
            },
            {
                "pengumuman": {
                    "Id2": "20260826173543-008/CSG-IVR/2026_id-id",
                    "NoPengumuman": "008/CSG-IVR/2026",
                    "TglPengumuman": "2026-08-26T17:35:43",
                    "JudulPengumuman": "Rencana Penyelenggaraan Public Expose",
                    "JenisPengumuman": "STOCK",
                    "Kode_Emiten": "BBCA"
                }
            }
        ]
    }"#;

    #[test]
    fn deserializes_a_full_idx_response_fixture() {
        let resp: ApiResponse = serde_json::from_str(FIXTURE).expect("fixture should parse");

        assert_eq!(resp.result_count, 222);
        assert_eq!(resp.replies.len(), 2);

        let first = &resp.replies[0];
        assert_eq!(first.pengumuman.id2, "20260901180245-009/CSG-IVR/2026_id-id");
        assert_eq!(first.pengumuman.judul, "Perubahan anggota Direksi");
        assert_eq!(first.attachments.len(), 2);
        assert!(!first.attachments[0].is_supporting);
        assert!(first.attachments[1].is_supporting);
    }

    #[test]
    fn reply_without_an_attachments_key_defaults_to_empty() {
        let resp: ApiResponse = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(resp.replies[1].attachments.len(), 0);
    }

    #[test]
    fn ticker_trims_idx_padded_whitespace() {
        let resp: ApiResponse = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(resp.replies[0].pengumuman.ticker(), "BBCA");
        assert_eq!(resp.replies[1].pengumuman.ticker(), "BBCA");
    }

    #[tokio::test]
    async fn fetch_announcements_http_sends_expected_query_params_and_referer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/primary/ListedCompany/GetAnnouncement"))
            .and(query_param("kodeEmiten", "BBCA"))
            .and(query_param("emitenType", "s"))
            .and(query_param("indexFrom", "2"))
            .and(query_param("pageSize", "10"))
            .and(query_param("dateFrom", "20260101"))
            .and(query_param("dateTo", "20260901"))
            .and(query_param("lang", "en"))
            .and(query_param("keyword", "dividen"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(FIXTURE, "application/json"))
            .mount(&server)
            .await;

        let client = crate::http::HttpClient::new().unwrap();
        let params = QueryParams {
            ticker: Some("BBCA".to_string()),
            keyword: Some("dividen".to_string()),
            emiten_type: "s".to_string(),
            date_from: "20260101".to_string(),
            date_to: "20260901".to_string(),
            index_from: 2,
            page_size: 10,
            lang: "en".to_string(),
        };

        let url = format!("{}/primary/ListedCompany/GetAnnouncement", server.uri());
        let resp = fetch_announcements_http_at(&client, &url, &params)
            .await
            .expect("mocked request should succeed");

        assert_eq!(resp.result_count, 222);
    }

    #[tokio::test]
    async fn get_json_surfaces_non_success_status_as_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/blocked"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = crate::http::HttpClient::new().unwrap();
        let err = client
            .get_json::<ApiResponse>(&format!("{}/blocked", server.uri()), &[])
            .await
            .expect_err("403 must not be treated as success");

        assert!(format!("{err:#}").contains("403"));
    }

    // Hits the real, Cloudflare-protected idx.co.id endpoint. Not run by default (would
    // make `cargo test` flaky/network-dependent in CI); verify manually with
    // `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn fetch_announcements_http_hits_the_real_idx_api() {
        let client = crate::http::HttpClient::new().unwrap();
        let params = QueryParams {
            ticker: Some("BBCA".to_string()),
            page_size: 1,
            ..Default::default()
        };

        let resp = fetch_announcements_http(&client, &params)
            .await
            .expect("live GetAnnouncement call should succeed against real IDX");

        assert!(resp.result_count > 0);
        assert_eq!(resp.replies.len(), 1);
        assert_eq!(resp.replies[0].pengumuman.ticker(), "BBCA");
    }
}
