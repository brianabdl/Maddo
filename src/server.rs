// Minimal HTTP server behind the `live` subcommand: serves a small single-page UI and
// proxies its two API calls through whichever `Backend` the run picked. Deliberately
// hand-rolled on `tokio::net::TcpListener` rather than pulling in a web framework:
// there are exactly three routes, all GET, and no bodies to parse. The bind address
// comes from `--host` and defaults to loopback; anything wider is the caller's call
// (the container image sets 0.0.0.0 and relies on the published port mapping).
//
// The file route is a proxy, not an open relay: it refuses any URL outside
// `https://www.idx.co.id/`, so the page can't be used to fetch arbitrary hosts through
// the impersonating client.

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::api::{QueryParams, Reply};
use crate::backend::Backend;
use crate::download;

const INDEX_HTML: &str = include_str!("ui/index.html");
const IDX_PREFIX: &str = "https://www.idx.co.id/";
const MAX_REQUEST_BYTES: usize = 8 * 1024;
const MAX_FILENAME_CHARS: usize = 200;

/// Serves until the returned future is dropped (the caller races it against Ctrl+C).
pub async fn serve(backend: Arc<Backend>, addr: SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    eprintln!("Live UI on http://{addr} (press Ctrl+C to stop).");

    loop {
        let (stream, _) = listener.accept().await.context("accepting connection")?;
        let backend = Arc::clone(&backend);
        tokio::spawn(async move {
            if let Err(e) = handle(stream, backend).await {
                eprintln!("Request failed: {e:#}");
            }
        });
    }
}

async fn handle(mut stream: TcpStream, backend: Arc<Backend>) -> Result<()> {
    let Some(target) = read_request_target(&mut stream).await? else {
        return respond(&mut stream, "400 Bad Request", "text/plain", b"bad request").await;
    };

    let (path, query_string) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target.as_str(), ""),
    };
    let query = parse_query(query_string);

    match path {
        "/" | "/index.html" => {
            respond(&mut stream, "200 OK", "text/html; charset=utf-8", INDEX_HTML.as_bytes()).await
        }
        "/api/announcements" => match announcements(&backend, &query).await {
            Ok(json) => respond(&mut stream, "200 OK", "application/json", json.as_bytes()).await,
            Err(e) => {
                let body = serde_json::json!({ "error": format!("{e:#}") }).to_string();
                respond(&mut stream, "502 Bad Gateway", "application/json", body.as_bytes()).await
            }
        },
        "/api/file" => match file(&backend, &query).await {
            Ok(bytes) => {
                let disposition = content_disposition(&download_name(&query));
                respond_with_headers(
                    &mut stream,
                    "200 OK",
                    "application/pdf",
                    &[("Content-Disposition", &disposition)],
                    &bytes,
                )
                .await
            }
            Err(e) => {
                let body = format!("{e:#}");
                respond(&mut stream, "502 Bad Gateway", "text/plain", body.as_bytes()).await
            }
        },
        _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found").await,
    }
}

async fn announcements(backend: &Backend, query: &[(String, String)]) -> Result<String> {
    let page: u32 = param(query, "page").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let page_size: u32 = param(query, "pageSize")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
        .clamp(1, 100);

    let params = QueryParams {
        ticker: non_empty(param(query, "ticker")),
        keyword: non_empty(param(query, "keyword")),
        emiten_type: non_empty(param(query, "type")).unwrap_or_else(|| "*".to_string()),
        date_from: compact_date(param(query, "dateFrom")).unwrap_or_else(|| "19010101".to_string()),
        date_to: compact_date(param(query, "dateTo"))
            .unwrap_or_else(|| chrono::Local::now().format("%Y%m%d").to_string()),
        index_from: page - 1,
        page_size,
        lang: non_empty(param(query, "lang")).unwrap_or_else(|| "id".to_string()),
    };

    let resp = backend.fetch_announcements(&params).await?;
    let replies = resp.replies.iter().map(with_suggested_filenames).collect::<Result<Vec<_>>>()?;
    Ok(serde_json::json!({
        "resultCount": resp.result_count,
        "page": page,
        "pageSize": page_size,
        "replies": replies,
    })
    .to_string())
}

/// `reply` as JSON, with a `SuggestedFilename` added to each attachment so the page can
/// ask `/api/file` to save it under a readable name instead of IDX's hash.
fn with_suggested_filenames(reply: &Reply) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(reply).context("serializing announcement")?;
    for (i, name) in download::attachment_filenames(reply).into_iter().enumerate() {
        value["attachments"][i]["SuggestedFilename"] = name.into();
    }
    Ok(value)
}

async fn file(backend: &Backend, query: &[(String, String)]) -> Result<Vec<u8>> {
    let url = param(query, "url").context("missing url parameter")?;
    if !url.starts_with(IDX_PREFIX) {
        anyhow::bail!("refusing to proxy a URL outside {IDX_PREFIX}");
    }
    backend.get_bytes(url).await
}

/// The filename to offer for `/api/file`: the page's `name` parameter, else the last
/// segment of the IDX URL. Sanitized either way, since it ends up in a response header.
fn download_name(query: &[(String, String)]) -> String {
    let requested = param(query, "name").unwrap_or_default();
    let from_url = param(query, "url").and_then(|u| u.rsplit('/').next()).unwrap_or_default();
    [requested, from_url]
        .into_iter()
        .map(|n| download::sanitize_filename(n, MAX_FILENAME_CHARS))
        .find(|n| !n.is_empty())
        .unwrap_or_else(|| "file.pdf".to_string())
}

/// `inline` keeps the PDF opening in the tab; the filename applies when it's saved. The
/// plain `filename` is an ASCII fallback, `filename*` carries the real name (RFC 6266).
fn content_disposition(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii() { c } else { '_' }).collect();
    format!("inline; filename=\"{ascii}\"; filename*=UTF-8''{}", percent_encode(name))
}

/// Reads request headers and returns the request target from the request line. Returns
/// `None` for anything that isn't a well-formed GET within `MAX_REQUEST_BYTES`.
async fn read_request_target(stream: &mut TcpStream) -> Result<Option<String>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];

    loop {
        let n = stream.read(&mut chunk).await.context("reading request")?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > MAX_REQUEST_BYTES {
            break;
        }
    }

    let head = String::from_utf8_lossy(&buf);
    let Some(line) = head.lines().next() else {
        return Ok(None);
    };
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(target)) => Ok(Some(target.to_string())),
        _ => Ok(None),
    }
}

async fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) -> Result<()> {
    respond_with_headers(stream, status, content_type, &[], body).await
}

async fn respond_with_headers(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<()> {
    let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.context("writing response head")?;
    stream.write_all(body).await.context("writing response body")?;
    stream.flush().await.context("flushing response")?;
    Ok(())
}

fn parse_query(qs: &str) -> Vec<(String, String)> {
    qs.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

fn param<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
    query.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Accepts either `YYYY-MM-DD` (what the UI's date inputs send) or an already-compact
/// `YYYYMMDD`, and returns `None` for an empty/absent value so the caller can default.
fn compact_date(value: Option<&str>) -> Option<String> {
    let digits: String = value?.chars().filter(char::is_ascii_digit).collect();
    (digits.len() == 8).then_some(digits)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Encodes every byte outside the RFC 3986 unreserved set, as `filename*` requires.
fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpClient;

    #[test]
    fn parse_query_splits_pairs_and_decodes_values() {
        let q = parse_query("ticker=BBCA&keyword=rapat%20umum&type=s");
        assert_eq!(param(&q, "ticker"), Some("BBCA"));
        assert_eq!(param(&q, "keyword"), Some("rapat umum"));
        assert_eq!(param(&q, "type"), Some("s"));
    }

    #[test]
    fn parse_query_handles_empty_string_and_valueless_keys() {
        assert!(parse_query("").is_empty());
        let q = parse_query("flag&a=1");
        assert_eq!(param(&q, "flag"), Some(""));
        assert_eq!(param(&q, "a"), Some("1"));
    }

    #[test]
    fn percent_decode_handles_plus_escapes_and_stray_percent() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%2Ffile.pdf"), "/file.pdf");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn percent_decode_reassembles_multibyte_utf8() {
        assert_eq!(percent_decode("Perubahan%20anggota%20Direksi"), "Perubahan anggota Direksi");
        assert_eq!(percent_decode("%E2%82%AC"), "€");
    }

    #[test]
    fn compact_date_accepts_both_input_shapes_and_rejects_junk() {
        assert_eq!(compact_date(Some("2026-09-01")).as_deref(), Some("20260901"));
        assert_eq!(compact_date(Some("20260901")).as_deref(), Some("20260901"));
        assert_eq!(compact_date(Some("")), None);
        assert_eq!(compact_date(Some("2026-09")), None);
        assert_eq!(compact_date(None), None);
    }

    #[test]
    fn non_empty_trims_and_drops_blanks() {
        assert_eq!(non_empty(Some("  BBCA ")).as_deref(), Some("BBCA"));
        assert_eq!(non_empty(Some("   ")), None);
        assert_eq!(non_empty(None), None);
    }

    #[test]
    fn percent_encode_escapes_everything_but_unreserved_bytes() {
        assert_eq!(percent_encode("a b-c_d.e~"), "a%20b-c_d.e~");
        assert_eq!(percent_encode("€"), "%E2%82%AC");
    }

    #[test]
    fn content_disposition_carries_ascii_fallback_and_encoded_name() {
        assert_eq!(
            content_disposition("20260901_BBCA_Laporan – Tahunan.pdf"),
            "inline; filename=\"20260901_BBCA_Laporan _ Tahunan.pdf\"; \
             filename*=UTF-8''20260901_BBCA_Laporan%20%E2%80%93%20Tahunan.pdf"
        );
    }

    #[test]
    fn download_name_prefers_the_requested_name_and_sanitizes_it() {
        let q = parse_query("url=https%3A%2F%2Fwww.idx.co.id%2Fa%2Fhash.pdf&name=x%22%0D%0AEvil%3A%20y.pdf");
        let name = download_name(&q);
        assert!(!name.contains(['"', '\r', '\n', ':']));
        assert_eq!(name, "x_ Evil_ y.pdf");
    }

    #[test]
    fn download_name_falls_back_to_the_url_segment_then_a_default() {
        let q = parse_query("url=https%3A%2F%2Fwww.idx.co.id%2Fa%2Fhash.pdf");
        assert_eq!(download_name(&q), "hash.pdf");
        assert_eq!(download_name(&[]), "file.pdf");
    }

    #[test]
    fn suggested_filenames_are_added_to_each_attachment() {
        use crate::api::{AttachmentInfo, Pengumuman};
        let reply = Reply {
            pengumuman: Pengumuman {
                id2: "id1".to_string(),
                no_pengumuman: "001/X/2026".to_string(),
                tanggal: "2026-09-01T18:02:45".to_string(),
                judul: "Judul".to_string(),
                jenis: "STOCK".to_string(),
                kode_emiten: "BBCA".to_string(),
            },
            attachments: vec![AttachmentInfo {
                filename: "e45bf0b681_5f51678b83.pdf".to_string(),
                url: "https://www.idx.co.id/x.pdf".to_string(),
                is_supporting: false,
            }],
        };

        let value = with_suggested_filenames(&reply).unwrap();

        assert_eq!(value["attachments"][0]["SuggestedFilename"], "20260901_BBCA_Judul.pdf");
        assert_eq!(value["attachments"][0]["PDFFilename"], "e45bf0b681_5f51678b83.pdf");
    }

    #[tokio::test]
    async fn file_route_refuses_a_non_idx_url() {
        let backend = Backend::Http(HttpClient::new().unwrap());
        let query = parse_query("url=https%3A%2F%2Fevil.example%2Fx.pdf");

        let err = file(&backend, &query).await.expect_err("must refuse foreign hosts");

        assert!(format!("{err:#}").contains("refusing to proxy"));
    }

    #[tokio::test]
    async fn file_route_requires_a_url_parameter() {
        let backend = Backend::Http(HttpClient::new().unwrap());
        let err = file(&backend, &[]).await.expect_err("missing url must error");
        assert!(format!("{err:#}").contains("missing url"));
    }
}
