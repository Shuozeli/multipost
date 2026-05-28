//! Minimal CDP-over-WebSocket client.
//!
//! Just the pieces the Douyin publisher needs: discover the WS URL from
//! a CDP HTTP endpoint, open a session against a target, send requests and
//! await replies. Replaced or expanded later when we share CDP code across
//! browser-automation publishers (Twitter, Xhs, etc.).

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Deserialize)]
struct JsonVersion {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

/// Translate the CDP HTTP endpoint into a WebSocket URL that's reachable
/// from this process.
///
/// Chrome reports `webSocketDebuggerUrl` with its own bound host (often
/// `ws://127.0.0.1:<port>/...`). When we reach Chrome through an SSH
/// tunnel or proxy, the localhost host in that URL points at the wrong
/// machine — we rewrite it to match the host/port of `http_url`.
pub async fn resolve_ws_url(http_url: &str) -> Result<String> {
    let parsed = url::Url::parse(http_url).context("parse cdp_url")?;
    let host = parsed.host_str().ok_or_else(|| anyhow!("cdp_url has no host"))?;
    let port = parsed.port().ok_or_else(|| anyhow!("cdp_url has no explicit port"))?;
    let info = reqwest::get(format!("{http_url}/json/version"))
        .await
        .context("GET /json/version")?
        .json::<JsonVersion>()
        .await
        .context("parse /json/version")?;
    // info.web_socket_debugger_url is `ws://<host>:<port>/devtools/browser/<id>`
    // Replace the host:port portion.
    let rest = info
        .web_socket_debugger_url
        .splitn(4, '/')
        .nth(3)
        .ok_or_else(|| anyhow!("malformed webSocketDebuggerUrl"))?;
    Ok(format!("ws://{host}:{port}/{rest}"))
}

/// A target (tab) reported by `Target.getTargets`.
/// A target (tab) reported by Chrome's `/json` HTTP endpoint.
///
/// Note: `/json` uses `id` for the target ID, while the CDP method
/// `Target.getTargets` over WebSocket uses `targetId`. We use the HTTP
/// endpoint here for simplicity.
#[derive(Debug, Clone, Deserialize)]
pub struct TargetInfo {
    /// Target ID (used to address the page's WS endpoint).
    pub id: String,
    /// `page`, `iframe`, `service_worker`, etc.
    #[serde(rename = "type")]
    pub kind: String,
    /// Currently-loaded URL.
    pub url: String,
    /// Page title.
    pub title: String,
}

/// CDP connection at the browser level.
pub struct BrowserSession {
    ws_url: String,
    cdp_http_url: String,
}

impl BrowserSession {
    /// Open a session against a CDP HTTP endpoint.
    pub async fn connect(cdp_http_url: &str) -> Result<Self> {
        let ws_url = resolve_ws_url(cdp_http_url).await?;
        Ok(Self {
            ws_url,
            cdp_http_url: cdp_http_url.trim_end_matches('/').to_string(),
        })
    }

    /// HTTP endpoint we opened against (e.g. `http://localhost:9333`).
    pub fn cdp_http_url(&self) -> &str {
        &self.cdp_http_url
    }

    /// List the open tab/page targets via the REST endpoint.
    pub async fn list_pages(&self) -> Result<Vec<TargetInfo>> {
        let resp: Vec<TargetInfo> = reqwest::get(format!("{}/json", self.cdp_http_url))
            .await
            .context("GET /json")?
            .json()
            .await
            .context("parse /json")?;
        Ok(resp.into_iter().filter(|t| t.kind == "page").collect())
    }

    /// Create a new tab navigated to `url`. Uses `PUT /json/new` which
    /// avoids the WebSocket-attach contention of navigating an existing
    /// tab that may have stale CDP clients attached.
    pub async fn create_tab(&self, url: &str) -> Result<TargetInfo> {
        let client = reqwest::Client::new();
        let target: TargetInfo = client
            .put(format!("{}/json/new?{}", self.cdp_http_url, url))
            .send()
            .await
            .context("PUT /json/new")?
            .json()
            .await
            .context("parse PUT /json/new response")?;
        Ok(target)
    }

    /// Close a tab by ID. Best-effort.
    pub async fn close_tab(&self, target_id: &str) -> Result<()> {
        let _ = reqwest::get(format!("{}/json/close/{target_id}", self.cdp_http_url))
            .await
            .context("GET /json/close")?;
        Ok(())
    }

    /// Look up one target by ID via the listing endpoint.
    pub async fn get_target(&self, target_id: &str) -> Result<Option<TargetInfo>> {
        let pages = self.list_pages().await?;
        Ok(pages.into_iter().find(|p| p.id == target_id))
    }

    /// Open the browser-level CDP WS for sending Browser.* and Target.*
    /// commands. Most useful methods on a single page live on the page-level
    /// connection ([`open_page`]).
    pub async fn ws_url(&self) -> &str {
        &self.ws_url
    }

    /// Open a CDP session against a specific page (target).
    pub async fn open_page(&self, target: &TargetInfo) -> Result<PageSession> {
        // Page-level CDP WS for a target uses the URL form
        //   ws://host:port/devtools/page/<targetId>
        let parsed = url::Url::parse(&self.ws_url)?;
        let host = parsed.host_str().ok_or_else(|| anyhow!("no host"))?;
        let port = parsed.port().ok_or_else(|| anyhow!("no port"))?;
        let url = format!("ws://{host}:{port}/devtools/page/{}", target.id);
        PageSession::connect(&url).await
    }
}

/// CDP connection at the page level.
///
/// Holds a WebSocket handle and dispatches request/response pairs by `id`.
/// Simple request/reply only — we don't subscribe to events yet.
pub struct PageSession {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: AtomicU64,
}

impl PageSession {
    /// Connect to a page-level WebSocket URL.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (ws, _resp) = tokio_tungstenite::connect_async(ws_url)
            .await
            .with_context(|| format!("connect to {ws_url}"))?;
        Ok(Self {
            ws,
            next_id: AtomicU64::new(1),
        })
    }

    /// Send a CDP request and wait for the matching reply by `id`.
    ///
    /// Ignores event messages that arrive before our reply. Times out
    /// after 15s — we don't want a stuck Chrome to hang the orchestrator
    /// indefinitely.
    pub async fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = json!({"id": id, "method": method, "params": params});
        self.ws
            .send(Message::Text(req.to_string()))
            .await
            .with_context(|| format!("send {method}"))?;
        let deadline = std::time::Duration::from_secs(15);
        loop {
            let msg = tokio::time::timeout(deadline, self.ws.next())
                .await
                .map_err(|_| anyhow!("cdp {method} timed out after 15s"))?
                .ok_or_else(|| anyhow!("ws closed before reply to {method}"))?
                .with_context(|| format!("recv after {method}"))?;
            let text = match msg {
                Message::Text(t) => t,
                Message::Binary(b) => String::from_utf8(b).context("non-utf8 binary frame")?,
                Message::Close(_) => return Err(anyhow!("ws closed by peer")),
                _ => continue,
            };
            let v: Value = serde_json::from_str(&text)
                .with_context(|| format!("parse reply: {text}"))?;
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(anyhow!("cdp {method} error: {err}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            // Else it's an event; ignore for now.
        }
    }

    /// Evaluate a JavaScript expression in this page's main context.
    /// Returns the deserialized result value (CDP's `RemoteObject.value`).
    pub async fn evaluate(&mut self, expression: &str) -> Result<Value> {
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

    /// Navigate the page to `url` and wait for the navigation to begin.
    /// Caller is responsible for waiting for DOM-ready / a specific element.
    pub async fn navigate(&mut self, url: &str) -> Result<()> {
        self.send("Page.enable", json!({})).await.ok();
        self.send("Page.navigate", json!({"url": url})).await?;
        Ok(())
    }

    /// Get the root document NodeId. Needed before `query_selector`.
    pub async fn get_document(&mut self) -> Result<u64> {
        let r = self.send("DOM.getDocument", json!({})).await?;
        r.get("root")
            .and_then(|v| v.get("nodeId"))
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow!("DOM.getDocument returned no nodeId"))
    }

    /// Find a node by CSS selector under `node_id`. Returns 0 if not found
    /// (Chrome's signal for "no match"). Caller should retry if the page
    /// is still loading.
    pub async fn query_selector(&mut self, node_id: u64, selector: &str) -> Result<u64> {
        let r = self
            .send(
                "DOM.querySelector",
                json!({"nodeId": node_id, "selector": selector}),
            )
            .await?;
        Ok(r.get("nodeId").and_then(|v| v.as_u64()).unwrap_or(0))
    }

    /// Set the files for an `<input type="file">` element. `files` is a list
    /// of paths *on the Chrome host*.
    pub async fn set_file_input_files(&mut self, node_id: u64, files: &[String]) -> Result<()> {
        self.send(
            "DOM.setFileInputFiles",
            json!({"nodeId": node_id, "files": files}),
        )
        .await?;
        Ok(())
    }

    /// Inject in-memory file bytes into the `<input type="file">` matched by
    /// `selector`, building `File` objects client-side from base64 and
    /// assigning them via `DataTransfer`.
    ///
    /// Unlike [`set_file_input_files`] — which hands Chrome a path that must
    /// exist on the *browser host* — this carries the bytes over the CDP
    /// WebSocket, so it works against a remote Chrome with no filesystem
    /// access. That's the only option for the cookie-auth publishers
    /// (Toutiao, Twitter) whose credentials carry no SSH info to stage files
    /// host-side the way Douyin's video upload does.
    ///
    /// `files` is `(filename, mime_type, bytes)`. All entries are assigned in
    /// a single `change` event, so a `multiple` input receives them as one
    /// batch — same as a user multi-selecting in the OS file picker.
    pub async fn upload_files_to_input(
        &mut self,
        selector: &str,
        files: &[(&str, &str, &[u8])],
    ) -> Result<()> {
        use base64::Engine as _;
        // Stage uploads on the page in `window.__mpUploads`.
        self.evaluate("window.__mpUploads = [];").await?;
        for (name, mime, bytes) in files {
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            // Stream the base64 in chunks so no single Runtime.evaluate frame
            // carries multi-MB of JS source (mirrors the body-fill chunking).
            self.evaluate("window.__mpCur = '';").await?;
            for chunk in b64.as_bytes().chunks(512 * 1024) {
                // base64's alphabet is ASCII, so every chunk is valid UTF-8.
                let piece = std::str::from_utf8(chunk).expect("base64 is ascii");
                let val = serde_json::to_string(piece)?;
                self.evaluate(&format!("window.__mpCur += {val};")).await?;
            }
            let name_json = serde_json::to_string(name)?;
            let mime_json = serde_json::to_string(mime)?;
            self.evaluate(&format!(
                "window.__mpUploads.push({{name: {name_json}, mime: {mime_json}, b64: window.__mpCur}});"
            ))
            .await?;
        }

        let sel_json = serde_json::to_string(selector)?;
        // Decode with `atob` + `Uint8Array`, NOT `fetch("data:...")`:
        // strict CSPs (x.com's `connect-src`) block fetching data: URLs,
        // which fails the whole upload. atob is unrestricted.
        let inject_js = format!(
            r#"(() => {{
                const input = document.querySelector({sel_json});
                if (!input) return {{ok: false, reason: 'no file input for selector'}};
                const dt = new DataTransfer();
                for (const u of window.__mpUploads) {{
                    const bin = atob(u.b64);
                    const bytes = new Uint8Array(bin.length);
                    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
                    dt.items.add(new File([bytes], u.name, {{type: u.mime}}));
                }}
                input.files = dt.files;
                input.dispatchEvent(new Event('input', {{bubbles: true}}));
                input.dispatchEvent(new Event('change', {{bubbles: true}}));
                window.__mpUploads = []; window.__mpCur = '';
                return {{ok: true, count: input.files.length}};
            }})()"#
        );
        let r = self.evaluate(&inject_js).await?;
        if !r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            anyhow::bail!("upload_files_to_input failed: {r}");
        }
        Ok(())
    }

    /// Dispatch a real OS-level mouse event via CDP `Input.dispatchMouseEvent`.
    /// Use this when JS-synthesized `MouseEvent`s aren't enough — some
    /// frameworks (Toutiao's byte-design hover-reveal among them) only
    /// honor genuine pointer input.
    ///
    /// `event` is one of `"mouseMoved"`, `"mousePressed"`, `"mouseReleased"`.
    pub async fn dispatch_mouse_event(
        &mut self,
        event: &str,
        x: f64,
        y: f64,
        button: &str,
        click_count: u32,
    ) -> Result<()> {
        self.send(
            "Input.dispatchMouseEvent",
            json!({
                "type": event,
                "x": x,
                "y": y,
                "button": button,
                "clickCount": click_count,
            }),
        )
        .await?;
        Ok(())
    }

    /// Real-mouse click sequence: move → press → release.
    /// Coordinates are page-relative pixels.
    pub async fn real_click(&mut self, x: f64, y: f64) -> Result<()> {
        self.dispatch_mouse_event("mouseMoved", x, y, "none", 0).await?;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        self.dispatch_mouse_event("mousePressed", x, y, "left", 1).await?;
        self.dispatch_mouse_event("mouseReleased", x, y, "left", 1).await?;
        Ok(())
    }

    /// Real-mouse hover: move to (x, y). Doesn't press.
    /// A short settle delay so React hover handlers commit.
    pub async fn real_hover(&mut self, x: f64, y: f64) -> Result<()> {
        self.dispatch_mouse_event("mouseMoved", x, y, "none", 0).await?;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        Ok(())
    }

    /// Bring this page's tab to the foreground (`Page.bringToFront`).
    ///
    /// Tabs created via `PUT /json/new` open in the background, and Chrome
    /// does not run hit-testing for a hidden tab — so real CDP mouse events
    /// (`Input.dispatchMouseEvent`) land on nothing and clicks silently
    /// no-op. Foregrounding the tab makes `real_click` actually hit the
    /// element (and is what a human's compose tab looks like anyway).
    pub async fn bring_to_front(&mut self) -> Result<()> {
        self.send("Page.bringToFront", json!({})).await?;
        Ok(())
    }

    /// Insert `text` at the current caret via CDP `Input.insertText`.
    ///
    /// Unlike `Runtime.evaluate("execCommand('insertText', ...)")`, this
    /// dispatches a genuine `beforeinput`/`input` event down the same path
    /// an IME commit takes: the event is `isTrusted` and frameworks like
    /// Draft.js / Lexical honor it. A bulk `execCommand` fill is both
    /// untrusted and instantaneous — a textbook automation tell. Callers
    /// drive this one grapheme at a time with jitter for human cadence.
    pub async fn insert_text(&mut self, text: &str) -> Result<()> {
        self.send("Input.insertText", json!({ "text": text })).await?;
        Ok(())
    }
}
