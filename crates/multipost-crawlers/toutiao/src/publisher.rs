//! [`Crawler`] impl for Toutiao — drives `pwright network-listen
//! --include-body` as a subprocess and decodes captured responses.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use multipost_core::{CrawlOptions, Crawler, DiscoveredItem, Platform, PublishError, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::sleep;
use tracing::{debug, warn};

use crate::parser::decode_feed_response;

/// Toutiao 推荐 feed crawler.
///
/// Stateless — one instance can be reused across [`run`](Crawler::run)
/// calls. Each call spawns its own pwright subprocesses.
#[derive(Debug, Default, Clone)]
pub struct ToutiaoCrawler;

const TOUTIAO_HOME: &str = "https://www.toutiao.com/";
const FEED_URL_SUBSTRING: &str = "/api/pc/list/feed";
const SCROLL_PIXELS: i32 = 2000;
const SCROLL_INTERVAL_SECS: u64 = 3;

impl ToutiaoCrawler {
    /// Build a new crawler. No setup is performed until [`run`].
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Crawler for ToutiaoCrawler {
    fn platform(&self) -> Platform {
        Platform::Toutiao
    }

    async fn run(&self, opts: &CrawlOptions) -> Result<Vec<DiscoveredItem>> {
        // 1. Open the Toutiao home page on the primary session. `open`
        //    populates pwright's state.json with the active tab so the
        //    subsequent `network-listen` and `eval` invocations attach
        //    to the same target.
        pwright_run(opts, &["open", TOUTIAO_HOME]).await?;
        sleep(Duration::from_secs(3)).await;

        // 2. Spawn the listener subprocess. It will exit on its own when
        //    --duration expires.
        let cdp = opts.cdp_url.clone();
        let duration_arg = opts.duration_secs.to_string();
        let mut listener = Command::new(&opts.pwright_bin);
        if let Some(c) = cdp.as_deref() {
            listener.env("PWRIGHT_CDP", c);
        }
        listener
            .args([
                "network-listen",
                "--filter",
                FEED_URL_SUBSTRING,
                "--resource-type",
                "XHR",
                "--include-body",
                "--duration",
                &duration_arg,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        let mut child = listener
            .spawn()
            .map_err(|e| PublishError::Transient(format!("spawn pwright: {e}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PublishError::Transient("pwright stdout missing".into()))?;
        let mut reader = BufReader::new(stdout).lines();

        // 3. Drive scroll in parallel for the duration of the listener.
        //    Cancelled implicitly when this method returns (task is
        //    spawned with `tokio::spawn` and uses opts ownership).
        let scroll_opts = opts.clone();
        let scroll_duration = Duration::from_secs(opts.duration_secs);
        let scroller = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + scroll_duration;
            loop {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                let js = format!("window.scrollBy(0, {SCROLL_PIXELS}); 1");
                if let Err(e) = pwright_run(&scroll_opts, &["eval", &js]).await {
                    debug!("scroll eval failed: {e}");
                }
                sleep(Duration::from_secs(SCROLL_INTERVAL_SECS)).await;
            }
        });

        // 4. Drain the JSONL stream as the listener emits events.
        let now = Utc::now();
        let mut items = Vec::new();
        while let Some(line) = reader
            .next_line()
            .await
            .map_err(|e| PublishError::Transient(format!("listener stdout: {e}")))?
        {
            if line.is_empty() {
                continue;
            }
            let event: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    debug!("skip non-JSON listener line: {e}");
                    continue;
                }
            };
            if event.get("event").and_then(Value::as_str) != Some("response") {
                continue;
            }
            let body = match event.get("body").and_then(Value::as_str) {
                Some(b) => b,
                None => continue,
            };
            match decode_feed_response(body, now) {
                Ok(mut decoded) => items.append(&mut decoded),
                Err(e) => warn!("toutiao feed decode failed: {e}"),
            }
        }

        // 5. Reap the children.
        let status = child
            .wait()
            .await
            .map_err(|e| PublishError::Transient(format!("listener wait: {e}")))?;
        if !status.success() {
            warn!("pwright network-listen exited non-zero: {status}");
        }
        let _ = scroller.await; // already deadline-bounded

        // 6. De-dup within this run; later metrics win.
        items = dedup_keep_last(items);
        Ok(items)
    }
}

/// Run `pwright <args>` with the configured binary and CDP env var,
/// returning Err on non-zero exit.
async fn pwright_run(opts: &CrawlOptions, args: &[&str]) -> Result<()> {
    let mut cmd = Command::new(&opts.pwright_bin);
    if let Some(c) = opts.cdp_url.as_deref() {
        cmd.env("PWRIGHT_CDP", c);
    }
    cmd.args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let out = cmd
        .output()
        .await
        .map_err(|e| PublishError::Transient(format!("pwright {args:?}: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(PublishError::Transient(format!(
            "pwright {args:?} failed ({}): {stderr}",
            out.status
        )));
    }
    Ok(())
}

fn dedup_keep_last(items: Vec<DiscoveredItem>) -> Vec<DiscoveredItem> {
    use std::collections::HashMap;
    let mut by_id: HashMap<String, DiscoveredItem> = HashMap::new();
    for it in items {
        by_id.insert(it.item_id.clone(), it);
    }
    by_id.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_keeps_one_per_item_id() {
        // Arrange
        let mk = |id: &str, reads: i64| DiscoveredItem {
            platform: Platform::Toutiao,
            item_id: id.to_string(),
            captured_at: Utc::now(),
            author_handle: "src".into(),
            author_name: None,
            body: "body".into(),
            url: None,
            metrics: multipost_core::DiscoveryMetrics {
                read_count: Some(reads),
                ..Default::default()
            },
            metadata: Default::default(),
        };
        let items = vec![mk("a", 1), mk("b", 1), mk("a", 999)];

        // Act
        let deduped = dedup_keep_last(items);

        // Assert
        assert_eq!(deduped.len(), 2);
        // The "a" we kept should be the 999-reads one (last writer
        // wins, but HashMap order isn't stable — so just check by id)
        let a = deduped.iter().find(|i| i.item_id == "a").unwrap();
        assert_eq!(a.metrics.read_count, Some(999));
    }
}
