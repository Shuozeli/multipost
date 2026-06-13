//! YouTube Studio browser publisher.
//!
//! This is the cookie-auth fallback for accounts where OAuth/Data API is
//! unavailable. It drives a logged-in YouTube Studio Chrome profile over CDP.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result as AnyResult, anyhow};
use futures_util::{SinkExt, StreamExt};
use multipost_core::{
    AuthStatus, Content, MediaPayload, PublishError, PublishHandle, Result, Visibility,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::Command;
use tokio_tungstenite::tungstenite::Message;

/// Per-account credentials for the YouTube Studio CDP backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudioCredentials {
    /// Credential discriminator. Use `studio_cdp`.
    #[serde(default)]
    pub kind: String,
    /// Chrome DevTools Protocol HTTP endpoint, e.g. `http://alienware-win-yuacx:9222`.
    pub cdp_url: String,
    /// SSH host where Chrome runs. Empty means Chrome is local to the server.
    #[serde(default)]
    pub ssh_host: String,
    /// SSH username for `ssh_host`.
    #[serde(default)]
    pub ssh_user: String,
    /// Optional SSH password. If set, staging uses `sshpass`.
    #[serde(default)]
    pub ssh_password: String,
    /// SSH port. `None` means 22.
    #[serde(default)]
    pub ssh_port: Option<u16>,
    /// Directory on the Chrome host where uploads are staged.
    #[serde(default)]
    pub remote_temp_dir: Option<String>,
    /// Cached channel display name for drift checks.
    #[serde(default)]
    pub display_name: String,
    /// Cached channel handle, e.g. `@newfinnews`.
    #[serde(default)]
    pub handle: String,
}

impl StudioCredentials {
    /// Whether a JSON credentials value should be handled by Studio/CDP.
    pub fn is_studio(value: &serde_json::Value) -> bool {
        value
            .get("kind")
            .and_then(|v| v.as_str())
            .is_some_and(|kind| kind == "studio_cdp")
            || value.get("cdp_url").is_some()
    }

    /// Default remote staging directory.
    pub fn effective_remote_temp_dir(&self) -> &str {
        self.remote_temp_dir
            .as_deref()
            .unwrap_or("C:/Users/cyuan/Videos/multipost-uploads")
    }

    fn ssh_target(&self) -> Option<String> {
        if self.ssh_host.is_empty() {
            None
        } else if self.ssh_user.is_empty() {
            Some(self.ssh_host.clone())
        } else {
            Some(format!("{}@{}", self.ssh_user, self.ssh_host))
        }
    }
}

/// Parse Studio credentials from stored JSON.
pub(crate) fn parse_credentials(value: &serde_json::Value) -> Result<StudioCredentials> {
    serde_json::from_value(value.clone())
        .map_err(|e| PublishError::Other(anyhow!("invalid youtube studio credentials: {e}")))
}

/// Check whether the CDP Chrome is logged into YouTube Studio.
pub(crate) async fn check_auth(credentials: &StudioCredentials) -> Result<AuthStatus> {
    let browser = BrowserSession::connect(&credentials.cdp_url)
        .await
        .map_err(|e| PublishError::Transient(format!("youtube studio CDP: {e}")))?;
    let target = browser
        .create_tab("https://studio.youtube.com/")
        .await
        .map_err(|e| PublishError::Transient(format!("youtube studio open tab: {e}")))?;
    let target_id = target.id.clone();
    let result = async {
        let mut page = browser
            .open_page(&target)
            .await
            .context("open youtube studio page")?;
        page.wait_for_loadish().await?;
        let body = page.body_text().await?;
        if body.contains("Sign in") || body.contains("登录") {
            return Ok(AuthStatus::Expired);
        }
        let looks_logged_in = body.contains("Channel dashboard")
            || body.contains("Content")
            || body.contains("频道内容")
            || body.contains("信息中心");
        if !looks_logged_in {
            return Ok(AuthStatus::Pending);
        }
        if !credentials.handle.is_empty() || !credentials.display_name.is_empty() {
            let identity = page
                .evaluate(
                    r#"
                    (() => document.body.innerText + "\n" + document.title + "\n" +
                      [...document.querySelectorAll('[aria-label],img[alt]')]
                        .map(e => e.getAttribute('aria-label') || e.getAttribute('alt') || '')
                        .join('\n'))()
                    "#,
                )
                .await
                .unwrap_or(Value::Null)
                .as_str()
                .unwrap_or("")
                .to_string();
            if !credentials.handle.is_empty() && identity.contains(&credentials.handle) {
                return Ok(AuthStatus::Active);
            }
            if !credentials.display_name.is_empty() && identity.contains(&credentials.display_name)
            {
                return Ok(AuthStatus::Active);
            }
            tracing::warn!(
                handle = %credentials.handle,
                display_name = %credentials.display_name,
                "youtube studio identity hint was not visible; treating logged-in Studio as active"
            );
        }
        Ok(AuthStatus::Active)
    }
    .await;
    let _ = browser.close_tab(&target_id).await;
    result.map_err(|e: anyhow::Error| PublishError::Transient(format!("youtube studio auth: {e}")))
}

/// Publish one YouTube video through Studio.
pub(crate) async fn publish(
    credentials: &StudioCredentials,
    content: &Content,
    video: &MediaPayload,
    thumbnail: Option<&MediaPayload>,
) -> Result<PublishHandle> {
    let temp = TempDir::new()
        .map_err(|e| PublishError::Other(anyhow!("create youtube upload tempdir: {e}")))?;
    let video_local = temp
        .path()
        .join(safe_filename(&video.filename, "video.mp4"));
    tokio::fs::write(&video_local, &video.bytes)
        .await
        .map_err(|e| PublishError::Other(anyhow!("write temp video: {e}")))?;
    let thumb_local = if let Some(thumbnail) = thumbnail {
        let path = temp
            .path()
            .join(safe_filename(&thumbnail.filename, "thumbnail.png"));
        tokio::fs::write(&path, &thumbnail.bytes)
            .await
            .map_err(|e| PublishError::Other(anyhow!("write temp thumbnail: {e}")))?;
        Some(path)
    } else {
        None
    };

    let staged_video = stage_file(credentials, &video_local)
        .await
        .map_err(|e| PublishError::Other(anyhow!("stage video for youtube studio: {e}")))?;
    let staged_thumb = if let Some(path) = &thumb_local {
        Some(
            stage_file(credentials, path)
                .await
                .map_err(|e| PublishError::Other(anyhow!("stage thumbnail: {e}")))?,
        )
    } else {
        None
    };

    let result = drive_upload(
        credentials,
        content,
        &staged_video.remote_path,
        staged_thumb.as_ref().map(|s| s.remote_path.as_str()),
    )
    .await;

    let _ = cleanup_file(credentials, &staged_video).await;
    if let Some(staged) = staged_thumb {
        let _ = cleanup_file(credentials, &staged).await;
    }
    result
}

async fn drive_upload(
    credentials: &StudioCredentials,
    content: &Content,
    video_remote_path: &str,
    thumbnail_remote_path: Option<&str>,
) -> Result<PublishHandle> {
    let browser = BrowserSession::connect(&credentials.cdp_url)
        .await
        .map_err(|e| PublishError::Transient(format!("youtube studio CDP: {e}")))?;
    let target = browser
        .create_tab("https://studio.youtube.com/")
        .await
        .map_err(|e| PublishError::Transient(format!("youtube studio open tab: {e}")))?;
    let target_id = target.id.clone();
    let result = async {
        let mut page = browser
            .open_page(&target)
            .await
            .context("open youtube studio page")?;
        page.wait_for_loadish().await?;
        page.click_upload_entry().await?;
        page.wait_for_text("Upload videos", Duration::from_secs(30))
            .await?;
        page.set_first_file_input(video_remote_path).await?;
        page.wait_for_text("Details", Duration::from_secs(60))
            .await?;
        page.fill_textbox(0, content.text.lines().next().unwrap_or("(untitled)"))
            .await?;
        page.fill_textbox(1, extract_description(content))
            .await
            .ok();
        if let Some(path) = thumbnail_remote_path {
            page.set_thumbnail_file(path).await?;
        }
        page.select_not_made_for_kids().await?;
        page.click_next(3).await?;
        page.wait_for_text("Visibility", Duration::from_secs(30))
            .await?;
        page.select_visibility(content.visibility).await?;
        page.wait_until_upload_ready().await?;
        page.click_final_visibility_action(content.visibility)
            .await?;
        let permalink = page.wait_for_youtu_link(Duration::from_secs(90)).await?;
        let external_id = extract_video_id(&permalink)
            .ok_or_else(|| anyhow!("YouTube Studio returned a permalink without a video id"))?;
        Ok(PublishHandle {
            external_id,
            permalink: Some(permalink),
        })
    }
    .await;
    let _ = browser.close_tab(&target_id).await;
    result.map_err(|e: anyhow::Error| PublishError::Other(e))
}

fn extract_description(content: &Content) -> &str {
    if let Some(idx) = content.text.find('\n') {
        content.text[idx + 1..].trim_start()
    } else {
        ""
    }
}

fn safe_filename(filename: &str, fallback: &str) -> String {
    let name = filename
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback);
    format!("{}-{name}", uuid::Uuid::new_v4())
}

fn extract_video_id(link: &str) -> Option<String> {
    link.split("youtu.be/")
        .nth(1)
        .map(|s| {
            s.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect()
        })
        .filter(|s: &String| !s.is_empty())
}

#[derive(Debug, Clone)]
struct StagedFile {
    remote_path: String,
    ssh_target: String,
}

async fn stage_file(credentials: &StudioCredentials, local_path: &Path) -> AnyResult<StagedFile> {
    if !local_path.exists() {
        return Err(anyhow!("local file not found: {}", local_path.display()));
    }
    let basename = local_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload.bin");
    let remote_dir = credentials.effective_remote_temp_dir();
    let remote_path = format!("{}/{}", remote_dir.trim_end_matches('/'), basename);
    let Some(target) = credentials.ssh_target() else {
        return Ok(StagedFile {
            remote_path: local_path.to_string_lossy().to_string(),
            ssh_target: String::new(),
        });
    };
    ssh_mkdir(credentials, &target, remote_dir).await?;
    scp_copy(credentials, local_path, &target, &remote_path).await?;
    Ok(StagedFile {
        remote_path,
        ssh_target: target,
    })
}

async fn cleanup_file(credentials: &StudioCredentials, staged: &StagedFile) -> AnyResult<()> {
    if staged.ssh_target.is_empty() {
        return Ok(());
    }
    let ps = format!(
        "powershell -NoProfile -Command \"Remove-Item -Force -ErrorAction SilentlyContinue '{}'\"",
        staged.remote_path.replace('\'', "''")
    );
    let mut cmd = ssh_command(credentials);
    cmd.arg(&staged.ssh_target).arg(ps);
    let _ = cmd.status().await;
    Ok(())
}

async fn ssh_mkdir(credentials: &StudioCredentials, target: &str, dir: &str) -> AnyResult<()> {
    let ps = format!(
        "powershell -NoProfile -Command \"New-Item -ItemType Directory -Force -Path '{}' | Out-Null\"",
        dir.replace('\'', "''")
    );
    let mut cmd = ssh_command(credentials);
    cmd.arg(target).arg(ps);
    let out = cmd.output().await.context("spawn ssh mkdir")?;
    if !out.status.success() {
        return Err(anyhow!(
            "ssh mkdir failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

async fn scp_copy(
    credentials: &StudioCredentials,
    local: &Path,
    target: &str,
    remote: &str,
) -> AnyResult<()> {
    let mut cmd = if credentials.ssh_password.is_empty() {
        Command::new("scp")
    } else {
        let mut c = Command::new("sshpass");
        c.arg("-p").arg(&credentials.ssh_password).arg("scp");
        c
    };
    if let Some(port) = credentials.ssh_port {
        cmd.arg("-P").arg(port.to_string());
    }
    cmd.arg("-q")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new");
    if credentials.ssh_password.is_empty() {
        cmd.arg("-o").arg("BatchMode=yes");
    }
    cmd.arg(local).arg(format!("{target}:{remote}"));
    let out = cmd.output().await.context("spawn scp")?;
    if !out.status.success() {
        return Err(anyhow!(
            "scp failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn ssh_command(credentials: &StudioCredentials) -> Command {
    let mut cmd = if credentials.ssh_password.is_empty() {
        Command::new("ssh")
    } else {
        let mut c = Command::new("sshpass");
        c.arg("-p").arg(&credentials.ssh_password).arg("ssh");
        c
    };
    if let Some(port) = credentials.ssh_port {
        cmd.arg("-p").arg(port.to_string());
    }
    cmd.arg("-o").arg("StrictHostKeyChecking=accept-new");
    if credentials.ssh_password.is_empty() {
        cmd.arg("-o").arg("BatchMode=yes");
    }
    cmd
}

#[derive(Debug, Deserialize)]
struct JsonVersion {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TargetInfo {
    id: String,
    #[serde(rename = "type")]
    kind: String,
}

async fn resolve_ws_url(http_url: &str) -> AnyResult<String> {
    let parsed = url::Url::parse(http_url).context("parse cdp_url")?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("cdp_url has no host"))?;
    let port = parsed
        .port()
        .ok_or_else(|| anyhow!("cdp_url has no explicit port"))?;
    let info = reqwest::get(format!("{}/json/version", http_url.trim_end_matches('/')))
        .await
        .context("GET /json/version")?
        .json::<JsonVersion>()
        .await
        .context("parse /json/version")?;
    let rest = info
        .web_socket_debugger_url
        .splitn(4, '/')
        .nth(3)
        .ok_or_else(|| anyhow!("malformed webSocketDebuggerUrl"))?;
    Ok(format!("ws://{host}:{port}/{rest}"))
}

struct BrowserSession {
    ws_url: String,
    cdp_http_url: String,
}

impl BrowserSession {
    async fn connect(cdp_http_url: &str) -> AnyResult<Self> {
        let ws_url = resolve_ws_url(cdp_http_url).await?;
        Ok(Self {
            ws_url,
            cdp_http_url: cdp_http_url.trim_end_matches('/').to_string(),
        })
    }

    async fn create_tab(&self, url: &str) -> AnyResult<TargetInfo> {
        let target: TargetInfo = reqwest::Client::new()
            .put(format!("{}/json/new?{}", self.cdp_http_url, url))
            .send()
            .await
            .context("PUT /json/new")?
            .json()
            .await
            .context("parse PUT /json/new response")?;
        Ok(target)
    }

    async fn close_tab(&self, target_id: &str) -> AnyResult<()> {
        let _ = reqwest::get(format!("{}/json/close/{target_id}", self.cdp_http_url)).await?;
        Ok(())
    }

    async fn open_page(&self, target: &TargetInfo) -> AnyResult<PageSession> {
        if target.kind != "page" {
            return Err(anyhow!("target is not a page"));
        }
        let parsed = url::Url::parse(&self.ws_url)?;
        let host = parsed.host_str().ok_or_else(|| anyhow!("no host"))?;
        let port = parsed.port().ok_or_else(|| anyhow!("no port"))?;
        let url = format!("ws://{host}:{port}/devtools/page/{}", target.id);
        PageSession::connect(&url).await
    }
}

struct PageSession {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: AtomicU64,
}

impl PageSession {
    async fn connect(ws_url: &str) -> AnyResult<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .with_context(|| format!("connect to {ws_url}"))?;
        Ok(Self {
            ws,
            next_id: AtomicU64::new(1),
        })
    }

    async fn send(&mut self, method: &str, params: Value) -> AnyResult<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = json!({"id": id, "method": method, "params": params});
        self.ws
            .send(Message::Text(req.to_string()))
            .await
            .with_context(|| format!("send {method}"))?;
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(30), self.ws.next())
                .await
                .map_err(|_| anyhow!("cdp {method} timed out"))?
                .ok_or_else(|| anyhow!("ws closed before reply to {method}"))?
                .with_context(|| format!("recv after {method}"))?;
            let text = match msg {
                Message::Text(t) => t,
                Message::Binary(b) => String::from_utf8(b).context("non-utf8 binary frame")?,
                Message::Close(_) => return Err(anyhow!("ws closed by peer")),
                _ => continue,
            };
            let v: Value = serde_json::from_str(&text)?;
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(anyhow!("cdp {method} error: {err}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    async fn evaluate(&mut self, expression: &str) -> AnyResult<Value> {
        let result = self
            .send(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        if let Some(details) = result.get("exceptionDetails") {
            return Err(anyhow!("js exception: {details}"));
        }
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    async fn wait_for_loadish(&mut self) -> AnyResult<()> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(())
    }

    async fn body_text(&mut self) -> AnyResult<String> {
        Ok(self
            .evaluate("document.body ? document.body.innerText : ''")
            .await?
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    async fn wait_for_text(&mut self, needle: &str, timeout: Duration) -> AnyResult<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if self.body_text().await.unwrap_or_default().contains(needle) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err(anyhow!("timed out waiting for text {needle:?}"))
    }

    async fn click_upload_entry(&mut self) -> AnyResult<()> {
        let clicked = self
            .evaluate(
                r#"
                (() => {
                  const byLabel = [...document.querySelectorAll('[aria-label]')]
                    .find(e => /upload|上传/i.test(e.getAttribute('aria-label') || ''));
                  if (byLabel) { byLabel.click(); return true; }
                  const point = document.elementFromPoint(1393, 99);
                  if (point) { point.click(); return true; }
                  return false;
                })()
                "#,
            )
            .await?
            .as_bool()
            .unwrap_or(false);
        if !clicked {
            return Err(anyhow!("could not click YouTube Studio upload entry"));
        }
        Ok(())
    }

    async fn set_first_file_input(&mut self, path: &str) -> AnyResult<()> {
        let node = self
            .find_node("input[type=file]", Duration::from_secs(30))
            .await?;
        self.set_file_input_files(node, &[path.to_string()]).await
    }

    async fn set_thumbnail_file(&mut self, path: &str) -> AnyResult<()> {
        let node = self
            .find_node(
                "input[type=file][accept*='image'], input#file-loader[type=file]",
                Duration::from_secs(30),
            )
            .await?;
        self.set_file_input_files(node, &[path.to_string()]).await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(())
    }

    async fn fill_textbox(&mut self, index: usize, text: &str) -> AnyResult<()> {
        let js_text = serde_json::to_string(text)?;
        let ok = self
            .evaluate(&format!(
                r#"
                (() => {{
                  const boxes = [...document.querySelectorAll('[contenteditable="true"], ytcp-social-suggestions-textbox #textbox')];
                  const el = boxes[{index}];
                  if (!el) return false;
                  el.focus();
                  el.textContent = {js_text};
                  el.dispatchEvent(new InputEvent('input', {{bubbles:true, inputType:'insertText', data:{js_text}}}));
                  el.dispatchEvent(new Event('change', {{bubbles:true}}));
                  return true;
                }})()
                "#
            ))
            .await?
            .as_bool()
            .unwrap_or(false);
        if !ok {
            return Err(anyhow!("could not fill textbox #{index}"));
        }
        Ok(())
    }

    async fn select_not_made_for_kids(&mut self) -> AnyResult<()> {
        self.evaluate(
            r#"
            (() => {
              const r = document.querySelector('tp-yt-paper-radio-button[name="VIDEO_MADE_FOR_KIDS_NOT_MFK"]');
              if (r) { r.click(); return true; }
              const labels = [...document.querySelectorAll('tp-yt-paper-radio-button, [role=radio]')];
              const hit = labels.find(e => /not made for kids|不是.*儿童/i.test(e.innerText || e.getAttribute('aria-label') || ''));
              if (hit) { hit.click(); return true; }
              return false;
            })()
            "#,
        )
        .await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        Ok(())
    }

    async fn click_next(&mut self, count: usize) -> AnyResult<()> {
        for _ in 0..count {
            let ok = self
                .evaluate(
                    r#"
                    (() => {
                      const buttons = [...document.querySelectorAll('ytcp-button, button')];
                      const next = buttons.find(b => /next|下一步/i.test(b.innerText || b.getAttribute('aria-label') || ''));
                      if (next) { next.click(); return true; }
                      return false;
                    })()
                    "#,
                )
                .await?
                .as_bool()
                .unwrap_or(false);
            if !ok {
                return Err(anyhow!("could not click Next in YouTube Studio"));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Ok(())
    }

    async fn select_visibility(&mut self, visibility: Visibility) -> AnyResult<()> {
        let name = match visibility {
            Visibility::Public => "PUBLIC",
            Visibility::Unlisted | Visibility::Followers => "UNLISTED",
            Visibility::Private => "PRIVATE",
        };
        let ok = self
            .evaluate(&format!(
                r#"
                (() => {{
                  const direct = document.querySelector('tp-yt-paper-radio-button[name="{name}"]');
                  if (direct) {{ direct.click(); return true; }}
                  const wanted = "{name}".toLowerCase();
                  const radios = [...document.querySelectorAll('tp-yt-paper-radio-button, [role=radio]')];
                  const hit = radios.find(e => (e.innerText || e.getAttribute('aria-label') || '').toLowerCase().includes(wanted));
                  if (hit) {{ hit.click(); return true; }}
                  return false;
                }})()
                "#
            ))
            .await?
            .as_bool()
            .unwrap_or(false);
        if !ok {
            return Err(anyhow!("could not select visibility {name}"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        Ok(())
    }

    async fn wait_until_upload_ready(&mut self) -> AnyResult<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
        while tokio::time::Instant::now() < deadline {
            let text = self.body_text().await.unwrap_or_default();
            let uploading = text.contains("Uploading")
                || text.contains("uploading")
                || text.contains("Processing")
                || text.contains("正在上传")
                || text.contains("正在处理");
            if !uploading {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        Err(anyhow!("youtube studio upload did not become ready"))
    }

    async fn click_final_visibility_action(&mut self, visibility: Visibility) -> AnyResult<()> {
        let label = match visibility {
            Visibility::Public => "publish",
            Visibility::Private | Visibility::Unlisted | Visibility::Followers => "save",
        };
        let ok = self
            .evaluate(&format!(
                r#"
                (() => {{
                  const wanted = "{label}";
                  const buttons = [...document.querySelectorAll('ytcp-button, button')].reverse();
                  const hit = buttons.find(b => {{
                    const text = (b.innerText || b.getAttribute('aria-label') || '').trim().toLowerCase();
                    const disabled = b.disabled || b.hasAttribute('disabled') || b.getAttribute('aria-disabled') === 'true';
                    return !disabled && text === wanted;
                  }});
                  if (hit) {{ hit.click(); return true; }}
                  return false;
                }})()
                "#
            ))
            .await?
            .as_bool()
            .unwrap_or(false);
        if !ok {
            return Err(anyhow!(
                "could not click final YouTube Studio {label} button"
            ));
        }
        Ok(())
    }

    async fn wait_for_youtu_link(&mut self, timeout: Duration) -> AnyResult<String> {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            let link = self
                .evaluate(
                    r#"
                    (() => {
                      const href = [...document.querySelectorAll('a[href*="youtu.be/"]')]
                        .map(a => a.href)
                        .find(href => /https:\/\/youtu\.be\/[A-Za-z0-9_-]+/.test(href));
                      if (href) return href;
                      const m = (document.body.innerText || '').match(/https:\/\/youtu\.be\/[A-Za-z0-9_-]+/);
                      return m ? m[0] : '';
                    })()
                    "#,
                )
                .await?
                .as_str()
                .unwrap_or("")
                .to_string();
            if !link.is_empty() {
                return Ok(link);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(anyhow!("timed out waiting for YouTube permalink"))
    }

    async fn find_node(&mut self, selector: &str, timeout: Duration) -> AnyResult<u64> {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            let r = self
                .send("DOM.getDocument", json!({"depth": -1, "pierce": true}))
                .await?;
            let root = r
                .get("root")
                .and_then(|v| v.get("nodeId"))
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow!("DOM.getDocument returned no nodeId"))?;
            let q = self
                .send(
                    "DOM.querySelector",
                    json!({"nodeId": root, "selector": selector}),
                )
                .await?;
            let node = q.get("nodeId").and_then(|v| v.as_u64()).unwrap_or(0);
            if node != 0 {
                return Ok(node);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(anyhow!("timed out waiting for selector {selector:?}"))
    }

    async fn set_file_input_files(&mut self, node_id: u64, files: &[String]) -> AnyResult<()> {
        self.send(
            "DOM.setFileInputFiles",
            json!({"nodeId": node_id, "files": files}),
        )
        .await?;
        Ok(())
    }
}
