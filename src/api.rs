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

pub async fn fetch_announcements_http(client: &HttpClient, params: &QueryParams) -> Result<ApiResponse> {
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
        .get_json("https://www.idx.co.id/primary/ListedCompany/GetAnnouncement", &query)
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
