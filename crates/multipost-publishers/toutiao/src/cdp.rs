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
}
