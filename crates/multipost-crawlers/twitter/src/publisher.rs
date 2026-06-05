//! [`Crawler`] impl for Twitter / X — captures HomeTimeline by
//! driving `pwright network-listen --include-body` on a fresh nav to
//! `https://x.com/home`.
//!
//! Unlike Toutiao, Twitter fires HomeTimeline only on full-page
//! navigation in our observed behavior, so we do **not** scroll the
//! page — one navigation = one timeline page (~30–40 tweets).

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

use crate::parser::decode_twitter_timeline;

/// Twitter / X "For you" feed crawler.
#[derive(Debug, Default, Clone)]
pub struct TwitterCrawler;

const HOME_URL: &str = "https://x.com/home";
const NOOP_URL: &str = "about:blank";
const FEED_URL_SUBSTRING: &str = "/HomeTimeline";
const TWEET_DETAIL_URL_SUBSTRING: &str = "/TweetDetail";

impl TwitterCrawler {
    /// Build a new crawler. No setup is performed until [`run`].
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Crawler for TwitterCrawler {
    fn platform(&self) -> Platform {
        Platform::Twitter
    }

    async fn run(&self, opts: &CrawlOptions) -> Result<Vec<DiscoveredItem>> {
        if !opts.source_urls.is_empty() {
            let mut items = Vec::new();
            for source_url in &opts.source_urls {
                pwright_run(opts, &["open", NOOP_URL]).await?;
                let captured =
                    capture_twitter_responses(opts, TWEET_DETAIL_URL_SUBSTRING, source_url).await;
                close_current_tab(opts).await;
                let mut captured = captured?;
                items.append(&mut captured);
            }
            return Ok(dedup_keep_last(items));
        }

        // 1. Park on about:blank so the next navigation to /home
        //    re-fires the HomeTimeline XHR (Twitter's SPA cache treats
        //    same-route navigations as no-ops).
        pwright_run(opts, &["open", NOOP_URL]).await?;

        let items = capture_twitter_responses(opts, FEED_URL_SUBSTRING, HOME_URL).await;
        close_current_tab(opts).await;
        let items = items?;
        Ok(dedup_keep_last(items))
    }
}

async fn capture_twitter_responses(
    opts: &CrawlOptions,
    filter: &str,
    url: &str,
) -> Result<Vec<DiscoveredItem>> {
    // Spawn the listener BEFORE navigation so we don't miss early GraphQL requests.
    let duration_arg = opts.duration_secs.to_string();
    let mut listener = Command::new(&opts.pwright_bin);
    if let Some(c) = opts.cdp_url.as_deref() {
        listener.env("PWRIGHT_CDP", c);
    }
    listener
        .args([
            "network-listen",
            "--filter",
            filter,
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

    sleep(Duration::from_millis(500)).await;
    pwright_run(opts, &["goto", url]).await?;

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
        match decode_twitter_timeline(body, now) {
            Ok(mut decoded) => items.append(&mut decoded),
            Err(e) => warn!("twitter timeline decode failed: {e}"),
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| PublishError::Transient(format!("listener wait: {e}")))?;
    if !status.success() {
        warn!("pwright network-listen exited non-zero: {status}");
    }

    Ok(items)
}

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

async fn close_current_tab(opts: &CrawlOptions) {
    if let Err(e) = pwright_run(opts, &["close"]).await {
        warn!("pwright close failed after twitter crawl: {e}");
    }
}

fn dedup_keep_last(items: Vec<DiscoveredItem>) -> Vec<DiscoveredItem> {
    use std::collections::HashMap;
    let mut by_id: HashMap<String, DiscoveredItem> = HashMap::new();
    for it in items {
        by_id.insert(it.item_id.clone(), it);
    }
    by_id.into_values().collect()
}
