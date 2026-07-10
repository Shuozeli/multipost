//! Bilibili REST API client for video upload + submit.
//!
//! The upload flow mirrors what `bilibili-api-python` does:
//!
//! 1. **Preupload** — `GET member.bilibili.com/preupload` returns an
//!    upload endpoint, auth token, chunk size, and a `upos_uri`.
//! 2. **Init** — `POST {endpoint}/{upos_path}?uploads&output=json`
//!    returns an `upload_id`.
//! 3. **Chunk upload** — `PUT` each ~10 MB chunk with the `upload_id`.
//! 4. **Complete** — `POST` the parts list to finalize the file.
//! 5. **Upload cover** (optional) — `POST member.bilibili.com/x/vu/web/cover/up`.
//! 6. **Submit** — `POST member.bilibili.com/x/vu/web/add/v3` with
//!    video metadata (title, desc, tags, tid, cover, etc.).

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, info};

use crate::credentials::BilibiliCredentials;

/// Default category ID: 财经 › 股票 (207). Override via `VideoMeta.tid`.
pub const DEFAULT_TID: u32 = 207;

/// Chunk size for upload (10 MB, same as Bilibili's default).
const CHUNK_SIZE: usize = 10 * 1024 * 1024;

// ── Preupload ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PreuploadResponse {
    #[serde(rename = "OK")]
    ok: u8,
    auth: String,
    biz_id: u64,
    #[allow(dead_code)]
    chunk_size: Option<usize>,
    endpoint: String,
    upos_uri: String,
}

/// Call the preupload API to get upload parameters.
async fn preupload(
    client: &Client,
    creds: &BilibiliCredentials,
    filename: &str,
    file_size: u64,
) -> Result<PreuploadResponse> {
    let url = format!(
        "https://member.bilibili.com/preupload?\
         name={}&size={}&r=upos&profile=ugcfx%2Fbup&ssl=0\
         &version=2.14.0.0&build=2140000&upcdn=bldsa\
         &probe_version=20221109&zone=cs&os=upos&biz_id=171",
        urlencoding::encode(filename),
        file_size,
    );
    let resp: PreuploadResponse = client
        .get(&url)
        .header("Cookie", creds.cookie_header())
        .header(
            "Referer",
            "https://member.bilibili.com/platform/upload/video/frame",
        )
        .send()
        .await?
        .json()
        .await
        .context("preupload JSON")?;
    if resp.ok != 1 {
        bail!("preupload failed: OK={}", resp.ok);
    }
    Ok(resp)
}

// ── Init upload ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct InitResponse {
    upload_id: String,
}

async fn init_upload(
    client: &Client,
    creds: &BilibiliCredentials,
    endpoint: &str,
    upos_path: &str,
    auth: &str,
) -> Result<String> {
    let url = format!(
        "{}/{}?uploads&output=json&os=upos&profile=ugcfx%2Fbup",
        endpoint, upos_path,
    );
    let resp: InitResponse = client
        .post(&url)
        .header("X-Upos-Auth", auth)
        .header("Cookie", creds.cookie_header())
        .header(
            "Referer",
            "https://member.bilibili.com/platform/upload/video/frame",
        )
        .send()
        .await?
        .json()
        .await
        .context("init upload JSON")?;
    Ok(resp.upload_id)
}

// ── Upload session (bundles repeated params) ───────────────────────

struct UploadSession<'a> {
    client: &'a Client,
    creds: &'a BilibiliCredentials,
    endpoint: String,
    upos_path: String,
    auth: String,
}

// ── Chunk upload ────────────────────────────────────────────────────

async fn upload_chunks(sess: &UploadSession<'_>, upload_id: &str, data: &[u8]) -> Result<usize> {
    let file_size = data.len();
    let chunk_size = CHUNK_SIZE;
    let total_chunks = file_size.div_ceil(chunk_size);

    for i in 0..total_chunks {
        let start = i * chunk_size;
        let end = std::cmp::min(start + chunk_size, file_size);
        let chunk = &data[start..end];

        let url = format!(
            "{}/{}?partNumber={}&uploadId={}&chunk={}&chunks={}\
             &size={}&start={}&end={}&total={}",
            sess.endpoint,
            sess.upos_path,
            i + 1,
            urlencoding::encode(upload_id),
            i,
            total_chunks,
            chunk.len(),
            start,
            end,
            file_size,
        );

        sess.client
            .put(&url)
            .header("X-Upos-Auth", &sess.auth)
            .header("Cookie", sess.creds.cookie_header())
            .header(
                "Referer",
                "https://member.bilibili.com/platform/upload/video/frame",
            )
            .header("Content-Type", "application/octet-stream")
            .body(chunk.to_vec())
            .send()
            .await
            .with_context(|| format!("upload chunk {}/{}", i + 1, total_chunks))?;

        let pct = end * 100 / file_size;
        debug!(
            chunk = i + 1,
            total = total_chunks,
            pct,
            "bilibili: chunk uploaded"
        );
    }

    Ok(total_chunks)
}

// ── Complete upload ─────────────────────────────────────────────────

async fn complete_upload(
    sess: &UploadSession<'_>,
    upload_id: &str,
    biz_id: u64,
    total_chunks: usize,
    filename: &str,
) -> Result<()> {
    let parts: Vec<serde_json::Value> = (1..=total_chunks)
        .map(|i| serde_json::json!({"partNumber": i, "eTag": "etag"}))
        .collect();

    let url = format!(
        "{}/{}?output=json&name={}&profile=ugcfx%2Fbup&uploadId={}&biz_id={}",
        sess.endpoint,
        sess.upos_path,
        urlencoding::encode(filename),
        urlencoding::encode(upload_id),
        biz_id,
    );

    sess.client
        .post(&url)
        .header("X-Upos-Auth", &sess.auth)
        .header("Cookie", sess.creds.cookie_header())
        .header(
            "Referer",
            "https://member.bilibili.com/platform/upload/video/frame",
        )
        .json(&serde_json::json!({"parts": parts}))
        .send()
        .await
        .context("complete upload")?;

    Ok(())
}

// ── Submit video ────────────────────────────────────────────────────

/// Video metadata for the submit call.
#[derive(Debug, Clone)]
pub struct VideoMeta {
    /// Video title (max ~80 chars).
    pub title: String,
    /// Video description.
    pub desc: String,
    /// Comma-separated tags.
    pub tag: String,
    /// Category ID (tid). Defaults to 207 (财经 › 股票).
    pub tid: u32,
    /// 1 = original, 2 = repost.
    pub copyright: u8,
    /// Source URL (required if copyright == 2).
    pub source: String,
}

#[derive(Debug, Deserialize)]
struct SubmitResponse {
    code: i32,
    message: String,
    data: Option<SubmitData>,
}

#[derive(Debug, Deserialize)]
struct SubmitData {
    aid: Option<u64>,
    bvid: Option<String>,
}

/// The result of a successful video submission.
#[derive(Debug, Clone)]
pub struct SubmitResult {
    /// Bilibili AV number.
    pub aid: u64,
    /// Bilibili BV ID (e.g. `BV1NNNW6dE7p`).
    pub bvid: String,
}

/// Upload a video file and submit it with the given metadata.
///
/// This is the main entry point — it runs the full preupload → chunk
/// upload → complete → submit pipeline.
pub async fn upload_and_submit(
    creds: &BilibiliCredentials,
    video_bytes: &[u8],
    video_filename: &str,
    meta: &VideoMeta,
    cover_url: Option<&str>,
) -> Result<SubmitResult> {
    let client = Client::new();
    let file_size = video_bytes.len() as u64;

    // 1. Preupload
    info!(
        filename = video_filename,
        size = file_size,
        "bilibili: preupload"
    );
    let pre = preupload(&client, creds, video_filename, file_size).await?;
    let endpoint = format!("https:{}", pre.endpoint);
    let upos_path = pre.upos_uri.replace("upos://", "");
    let upos_filename = upos_path
        .split('/')
        .next_back()
        .unwrap_or("video.mp4")
        .to_string();
    debug!(endpoint = %endpoint, upos_path = %upos_path, "bilibili: preupload OK");

    // 2. Init
    let upload_id = init_upload(&client, creds, &endpoint, &upos_path, &pre.auth).await?;
    debug!(upload_id = %upload_id, "bilibili: init OK");

    let sess = UploadSession {
        client: &client,
        creds,
        endpoint,
        upos_path,
        auth: pre.auth,
    };

    // 3. Upload chunks
    let total_chunks = upload_chunks(&sess, &upload_id, video_bytes).await?;
    info!(chunks = total_chunks, "bilibili: all chunks uploaded");

    // 4. Complete
    complete_upload(&sess, &upload_id, pre.biz_id, total_chunks, &upos_filename).await?;
    info!("bilibili: upload complete");

    // 5. Submit
    let submit_body = serde_json::json!({
        "copyright": meta.copyright,
        "source": meta.source,
        "tid": meta.tid,
        "cover": cover_url.unwrap_or(""),
        "title": meta.title,
        "desc_format_id": 0,
        "desc": meta.desc,
        "dynamic": "",
        "tag": meta.tag,
        "videos": [{
            "filename": upos_filename.trim_end_matches(".mp4"),
            "title": "P1",
            "desc": "",
            "cid": pre.biz_id,
        }],
        "no_reprint": 0,
        "open_elec": 0,
        "act_reserve_create": 0,
        "subtitle": {"open": 0, "lan": ""},
        "interactive": 0,
        "up_close_danmu": false,
        "up_close_reply": false,
        "up_selection_reply": false,
        "dtime": 0,
        "csrf": sess.creds.bili_jct,
    });

    let resp: SubmitResponse = sess
        .client
        .post("https://member.bilibili.com/x/vu/web/add/v3")
        .header("Cookie", sess.creds.cookie_header())
        .header(
            "Referer",
            "https://member.bilibili.com/platform/upload/video/frame",
        )
        .json(&submit_body)
        .send()
        .await?
        .json()
        .await
        .context("submit JSON")?;

    if resp.code != 0 {
        bail!(
            "bilibili submit failed: code={}, message={}",
            resp.code,
            resp.message
        );
    }

    let data = resp.data.context("submit response missing data")?;
    let bvid = data.bvid.context("submit response missing bvid")?;
    let aid = data.aid.context("submit response missing aid")?;

    info!(bvid = %bvid, aid = aid, "bilibili: video submitted");
    Ok(SubmitResult { aid, bvid })
}

/// Check if the Bilibili session is active by calling the myinfo API.
pub async fn check_session(creds: &BilibiliCredentials) -> Result<Option<(String, String)>> {
    let client = Client::new();
    let resp: serde_json::Value = client
        .get("https://api.bilibili.com/x/space/myinfo")
        .header("Cookie", creds.cookie_header())
        .header("Referer", "https://www.bilibili.com")
        .send()
        .await?
        .json()
        .await?;

    let code = resp["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        return Ok(None);
    }
    let name = resp["data"]["name"].as_str().unwrap_or("").to_string();
    let mid = resp["data"]["mid"]
        .as_u64()
        .map(|m| m.to_string())
        .unwrap_or_default();
    Ok(Some((name, mid)))
}
