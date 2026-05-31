//! [`UserCrawler`] impl for Twitter / X — captures a single account's
//! `UserTweets` timeline by navigating to `x.com/<handle>` and scrolling
//! to paginate until `max_posts` is reached.
//!
//! Unlike the home feed (one page on navigation), `UserTweets` paginates
//! on scroll, so this crawler drives a scroll loop like the Toutiao feed
//! crawler and accumulates pages until it has enough posts or hits the
//! duration safety stop.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use multipost_core::{
    DiscoveredItem, Platform, PublishError, Result, UserCrawlOptions, UserCrawler,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{Instant, sleep};
use tracing::{debug, warn};

use crate::parser::decode_user_tweets;

/// Per-user Twitter / X profile crawler.
#[derive(Debug, Default, Clone)]
pub struct TwitterUserCrawler;

const NOOP_URL: &str = "about:blank";
const FEED_URL_SUBSTRING: &str = "UserTweets";
const SCROLL_PIXELS: i32 = 3000;
const SCROLL_INTERVAL_SECS: u64 = 3;
/// Let the browser recover from a previous (batch) job's teardown before
/// driving it again — without this, back-to-back crawls race and the
/// early ones capture nothing.
const STARTUP_SETTLE_MS: u64 = 1200;
/// Give the spawned listener time to attach to the browser before we
/// navigate, so the initial `UserTweets` (which fires on nav) is caught.
const LISTENER_ATTACH_MS: u64 = 1500;
/// Let the SPA hydrate after navigation before scrolling — `scrollBy` on
/// an unhydrated page is a no-op and never triggers pagination XHR.
const POST_NAV_SETTLE_SECS: u64 = 4;
/// How many times to re-navigate when an attempt captures nothing.
/// x.com intermittently throttles rapid profile loads for a stretch; a
/// fresh navigation after a short wait clears it.
const MAX_ATTEMPTS: u32 = 3;
/// If an attempt has captured no posts within this window after
/// navigating, treat it as a stalled load and retry rather than waiting
/// out the full duration.
const EMPTY_PROBE_SECS: u64 = 25;
/// Poll cadence for the listener stream so we can check the stall
/// condition even while no events are arriving.
const POLL_TIMEOUT_SECS: u64 = 2;

impl TwitterUserCrawler {
    /// Build a new crawler. No setup is performed until [`crawl_user`].
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl UserCrawler for TwitterUserCrawler {
    fn platform(&self) -> Platform {
        Platform::Twitter
    }

    async fn crawl_user(&self, opts: &UserCrawlOptions) -> Result<Vec<DiscoveredItem>> {
        let handle = opts.handle.trim().trim_start_matches('@');
        if handle.is_empty() {
            return Err(PublishError::Rejected("empty handle".into()));
        }
        let profile_url = format!("https://x.com/{handle}");

        // 0. Settle — in a batch, the previous job's listener/tab teardown
        //    may still be in flight; give the browser a moment.
        sleep(Duration::from_millis(STARTUP_SETTLE_MS)).await;

        // Capture, retrying from scratch if an attempt comes back empty
        // (x.com throttles rapid profile loads in bursts; a fresh
        // navigation after a brief wait clears it).
        let mut by_id: HashMap<String, DiscoveredItem> = HashMap::new();
        for attempt in 1..=MAX_ATTEMPTS {
            by_id = capture_once(opts, &profile_url).await?;
            if !by_id.is_empty() {
                break;
            }
            warn!(handle, attempt, "user crawl captured 0 posts; retrying");
            sleep(Duration::from_secs(attempt as u64 * 3)).await;
        }

        let mut items: Vec<DiscoveredItem> = by_id.into_values().collect();
        items.truncate(opts.max_posts);
        Ok(items)
    }
}

/// One navigate-listen-scroll-drain cycle. Returns the de-duped posts
/// captured (empty if the SPA never loaded the timeline — the caller
/// retries). Aborts early if nothing is captured within
/// [`EMPTY_PROBE_SECS`] (a stalled load) so a retry can start promptly.
async fn capture_once(
    opts: &UserCrawlOptions,
    profile_url: &str,
) -> Result<HashMap<String, DiscoveredItem>> {
    // 1. Park on about:blank so the navigation re-fires the initial
    //    UserTweets XHR (the SPA treats same-route nav as a no-op).
    pwright_run(opts, &["open", NOOP_URL]).await?;

    // 2. Spawn the listener BEFORE navigation so we don't miss the first
    //    page. It exits on its own at --duration.
    let duration_arg = opts.max_duration_secs.to_string();
    let mut listener = Command::new(&opts.pwright_bin);
    if let Some(c) = opts.cdp_url.as_deref() {
        listener.env("PWRIGHT_CDP", c);
    }
    listener
        .args([
            "network-listen",
            "--filter",
            FEED_URL_SUBSTRING,
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

    // 3. Navigate once the listener has attached, then let the SPA
    //    hydrate before scrolling.
    sleep(Duration::from_millis(LISTENER_ATTACH_MS)).await;
    pwright_run(opts, &["goto", profile_url]).await?;
    sleep(Duration::from_secs(POST_NAV_SETTLE_SECS)).await;

    let scroll_opts = opts.clone();
    let scroll_deadline = Instant::now() + Duration::from_secs(opts.max_duration_secs);
    let scroller = tokio::spawn(async move {
        loop {
            if Instant::now() >= scroll_deadline {
                break;
            }
            let js = format!("window.scrollBy(0, {SCROLL_PIXELS}); 1");
            if let Err(e) = pwright_run(&scroll_opts, &["eval", &js]).await {
                debug!("scroll eval failed: {e}");
            }
            sleep(Duration::from_secs(SCROLL_INTERVAL_SECS)).await;
        }
    });

    // 4. Drain JSONL, decoding each UserTweets page. Poll with a timeout
    //    so we can bail on a stalled load even while no events arrive.
    let now = Utc::now();
    let nav_at = Instant::now();
    let mut by_id: HashMap<String, DiscoveredItem> = HashMap::new();
    loop {
        let next =
            tokio::time::timeout(Duration::from_secs(POLL_TIMEOUT_SECS), reader.next_line()).await;
        match next {
            Ok(Ok(Some(line))) => {
                if let Some(body) = response_body(&line) {
                    match decode_user_tweets(&body, now) {
                        Ok(decoded) => {
                            for it in decoded {
                                by_id.insert(it.item_id.clone(), it);
                            }
                        }
                        Err(e) => warn!("twitter UserTweets decode failed: {e}"),
                    }
                    if by_id.len() >= opts.max_posts {
                        debug!(count = by_id.len(), "reached max_posts, stopping early");
                        break;
                    }
                }
            }
            Ok(Ok(None)) => break, // listener exited (--duration elapsed)
            Ok(Err(e)) => return Err(PublishError::Transient(format!("listener stdout: {e}"))),
            Err(_) => {
                // No event this tick. If nothing has loaded well past the
                // settle window, the page stalled — bail so we can retry.
                if by_id.is_empty() && nav_at.elapsed() > Duration::from_secs(EMPTY_PROBE_SECS) {
                    debug!("no posts within probe window; aborting attempt");
                    break;
                }
            }
        }
    }

    // 5. Tear down the listener + scroller.
    let _ = child.start_kill();
    let _ = child.wait().await;
    scroller.abort();

    Ok(by_id)
}

/// Extract the response body string from one listener JSONL line, or
/// `None` for non-response / bodyless / unparseable lines.
fn response_body(line: &str) -> Option<String> {
    if line.is_empty() {
        return None;
    }
    let event: Value = serde_json::from_str(line).ok()?;
    if event.get("event").and_then(Value::as_str) != Some("response") {
        return None;
    }
    event
        .get("body")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Run `pwright <args>` with the configured binary + CDP env var.
async fn pwright_run(opts: &UserCrawlOptions, args: &[&str]) -> Result<()> {
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
