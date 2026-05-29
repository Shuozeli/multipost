//! Profile-stats collector for Twitter / X.
//!
//! Drives the logged-in profile (`x.com/<handle>`) over CDP and scrapes
//! the rendered DOM:
//!
//! - Account: the follower / following counts from the profile header
//!   links (`a[href$="/followers"]`, `a[href$="/following"]`).
//! - Per-tweet: each `article`'s action bar `[role="group"]` carries an
//!   accessibility `aria-label` with the **exact** counts ("12 replies,
//!   3 reposts, 88 likes, 4 bookmarks, 5039 views") — more reliable than
//!   the abbreviated visible text. Tweet id comes from the `…/status/<id>`
//!   permalink, timestamp from `<time datetime>`.
//!
//! Twitter has no per-post "reads" concept, so [`PostStats::reads`] stays
//! `None`; `impressions` maps to the tweet's view count.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use multipost_core::{
    AccountStats, Platform, PostStats, PublishError, Result, StatsCollector, StatsOptions,
    StatsSnapshot,
};

use crate::cdp::{BrowserSession, PageSession};

/// Twitter profile-stats collector. Stateless.
#[derive(Debug, Default, Clone)]
pub struct TwitterStatsCollector;

impl TwitterStatsCollector {
    /// Construct a new collector.
    pub fn new() -> Self {
        Self
    }
}

fn transient(e: impl std::fmt::Display) -> PublishError {
    PublishError::Transient(format!("twitter stats: {e}"))
}

#[async_trait]
impl StatsCollector for TwitterStatsCollector {
    fn platform(&self) -> Platform {
        Platform::Twitter
    }

    async fn collect(&self, opts: &StatsOptions) -> Result<StatsSnapshot> {
        let cdp = opts
            .cdp_url
            .as_deref()
            .ok_or_else(|| PublishError::Rejected("twitter stats: cdp_url required".into()))?;
        let handle = opts
            .handle
            .as_deref()
            .ok_or_else(|| PublishError::Rejected("twitter stats: handle required".into()))?
            .trim_start_matches('@');

        let session = BrowserSession::connect(cdp).await.map_err(transient)?;
        let url = format!("https://x.com/{handle}");
        let tab = session.create_tab(&url).await.map_err(transient)?;
        let mut page = session.open_page(&tab).await.map_err(transient)?;

        wait_profile_ready(&mut page).await?;

        let now = Utc::now();
        let account = scrape_account(&mut page, now).await?;
        let posts = scrape_posts(&mut page, handle, opts.max_posts, now).await?;

        let _ = session.close_tab(&tab.id).await;
        Ok(StatsSnapshot { account, posts })
    }
}

/// Wait until the profile header (the follower/following links) renders.
/// The timeline articles may still be empty (account with no posts), which
/// is fine — we proceed with account stats.
async fn wait_profile_ready(page: &mut PageSession) -> Result<()> {
    let deadline = Duration::from_secs(45);
    let start = Instant::now();
    loop {
        let ready = page
            .evaluate(
                r#"!!document.querySelector('a[href$="/verified_followers"], a[href$="/followers"]')"#,
            )
            .await
            .map_err(transient)?
            .as_bool()
            .unwrap_or(false);
        if ready {
            // small settle so counts hydrate
            tokio::time::sleep(Duration::from_millis(500)).await;
            return Ok(());
        }
        if start.elapsed() > deadline {
            return Err(PublishError::Transient(
                "twitter stats: profile header not ready within 45s (logged out or rate-limited?)"
                    .into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Scrape follower / following counts from the profile header.
async fn scrape_account(page: &mut PageSession, now: DateTime<Utc>) -> Result<AccountStats> {
    let v = page
        .evaluate(
            r#"(() => {
                const pick = (sel) => {
                    const a = document.querySelector(sel);
                    if (!a) return null;
                    return a.getAttribute('aria-label') || a.innerText || null;
                };
                return {
                    followers: pick('a[href$="/verified_followers"]') || pick('a[href$="/followers"]'),
                    following: pick('a[href$="/following"]'),
                };
            })()"#,
        )
        .await
        .map_err(transient)?;

    let mut account = AccountStats::new(Platform::Twitter, now);
    account.followers = v["followers"].as_str().and_then(parse_count_abbrev);
    account.following = v["following"].as_str().and_then(parse_count_abbrev);
    Ok(account)
}

/// Scroll the timeline, scraping the account's own tweets until we have
/// `max_posts` or scrolling stops yielding new ones.
async fn scrape_posts(
    page: &mut PageSession,
    handle: &str,
    max_posts: usize,
    now: DateTime<Utc>,
) -> Result<Vec<PostStats>> {
    let mut by_id: std::collections::HashMap<String, PostStats> = std::collections::HashMap::new();
    let mut stale_scrolls = 0;
    let deadline = Duration::from_secs(60);
    let start = Instant::now();

    while by_id.len() < max_posts && start.elapsed() < deadline && stale_scrolls < 4 {
        let raw = page
            .evaluate(
                r#"(() => Array.from(document.querySelectorAll('article')).map(a => {
                    const grp = a.querySelector('[role="group"]');
                    const sA = a.querySelector('a[href*="/status/"]');
                    const t = a.querySelector('time');
                    const txt = a.querySelector('[data-testid="tweetText"]');
                    return {
                        aria: grp ? grp.getAttribute('aria-label') : null,
                        href: sA ? sA.getAttribute('href') : null,
                        time: t ? t.getAttribute('datetime') : null,
                        text: txt ? (txt.innerText || '') : '',
                    };
                }))()"#,
            )
            .await
            .map_err(transient)?;

        let before = by_id.len();
        if let Some(arr) = raw.as_array() {
            for entry in arr {
                let href = entry["href"].as_str().unwrap_or("");
                // Only the account's own tweets (reposts carry the original
                // author's handle in the permalink).
                let Some(id) = own_status_id(href, handle) else {
                    continue;
                };
                if by_id.contains_key(&id) {
                    continue;
                }
                let mut p = PostStats::new(Platform::Twitter, id, now);
                p.post_type = "tweet".to_string();
                p.title = entry["text"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(140)
                    .collect();
                p.created_at = entry["time"]
                    .as_str()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));
                if let Some(aria) = entry["aria"].as_str() {
                    apply_aria_metrics(&mut p, aria);
                }
                by_id.insert(p.post_id.clone(), p);
                if by_id.len() >= max_posts {
                    break;
                }
            }
        }
        if by_id.len() == before {
            stale_scrolls += 1;
        } else {
            stale_scrolls = 0;
        }
        // Scroll to load more.
        let _ = page.evaluate("window.scrollBy(0, 2000); 1").await;
        tokio::time::sleep(Duration::from_millis(1200)).await;
    }

    // Newest first (the timeline is roughly reverse-chronological already,
    // but sort defensively by created_at desc).
    let mut posts: Vec<PostStats> = by_id.into_values().collect();
    posts.sort_by_key(|p| std::cmp::Reverse(p.created_at));
    posts.truncate(max_posts);
    Ok(posts)
}

/// Extract the tweet id from a `/status/<id>` permalink **only if** it
/// belongs to `handle` (filters out reposts of other accounts).
fn own_status_id(href: &str, handle: &str) -> Option<String> {
    let prefix = format!("/{}/status/", handle.to_ascii_lowercase());
    let lower = href.to_ascii_lowercase();
    let rest = lower.strip_prefix(&prefix)?;
    let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if id.is_empty() { None } else { Some(id) }
}

/// Parse the action-bar `aria-label` into per-tweet metrics. The label is
/// a comma list of "<n> <noun>" with **exact** counts, in any order and
/// possibly missing entries (e.g. "0 bookmarks" is often omitted).
fn apply_aria_metrics(p: &mut PostStats, aria: &str) {
    for part in aria.split(',') {
        let part = part.trim();
        let mut it = part.splitn(2, char::is_whitespace);
        let (Some(num), Some(noun)) = (it.next(), it.next()) else {
            continue;
        };
        let Some(n) = parse_count_abbrev(num) else {
            continue;
        };
        let noun = noun.to_ascii_lowercase();
        if noun.contains("repl") {
            p.comments = Some(n);
        } else if noun.contains("repost") || noun.contains("retweet") {
            p.shares = Some(n);
        } else if noun.contains("like") {
            p.likes = Some(n);
        } else if noun.contains("bookmark") {
            p.bookmarks = Some(n);
        } else if noun.contains("view") {
            p.impressions = Some(n);
        }
    }
}

/// Parse a count that may be plain ("1043"), comma-grouped ("1,234"), or
/// abbreviated ("1.2K", "3M", "2B"). Leading token of a longer string is
/// used, so "1,234 Followers" → 1234.
fn parse_count_abbrev(s: &str) -> Option<i64> {
    let token = s.split_whitespace().next()?.trim();
    let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != ',');
    if token.is_empty() {
        return None;
    }
    let (num_part, mult) = match token.chars().last() {
        Some(c) if c.eq_ignore_ascii_case(&'k') => (&token[..token.len() - 1], 1_000.0),
        Some(c) if c.eq_ignore_ascii_case(&'m') => (&token[..token.len() - 1], 1_000_000.0),
        Some(c) if c.eq_ignore_ascii_case(&'b') => (&token[..token.len() - 1], 1_000_000_000.0),
        _ => (token, 1.0),
    };
    let cleaned: String = num_part.chars().filter(|c| *c != ',').collect();
    let val: f64 = cleaned.parse().ok()?;
    Some((val * mult).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_count_handles_plain_comma_and_abbrev() {
        assert_eq!(parse_count_abbrev("1043"), Some(1043));
        assert_eq!(parse_count_abbrev("1,234"), Some(1234));
        assert_eq!(parse_count_abbrev("1.2K"), Some(1200));
        assert_eq!(parse_count_abbrev("3M"), Some(3_000_000));
        assert_eq!(parse_count_abbrev("1,234 Followers"), Some(1234));
        assert_eq!(parse_count_abbrev("88 likes"), Some(88));
        assert_eq!(parse_count_abbrev("no-number"), None);
    }

    #[test]
    fn aria_metrics_parses_full_label() {
        // Arrange
        let mut p = PostStats::new(Platform::Twitter, "1".into(), Utc::now());

        // Act
        apply_aria_metrics(
            &mut p,
            "12 replies, 3 reposts, 88 likes, 4 bookmarks, 5039 views",
        );

        // Assert
        assert_eq!(p.comments, Some(12));
        assert_eq!(p.shares, Some(3));
        assert_eq!(p.likes, Some(88));
        assert_eq!(p.bookmarks, Some(4));
        assert_eq!(p.impressions, Some(5039));
        assert_eq!(p.reads, None); // Twitter has no "reads"
    }

    #[test]
    fn aria_metrics_tolerates_missing_entries() {
        // Arrange — a tweet with only likes + views.
        let mut p = PostStats::new(Platform::Twitter, "1".into(), Utc::now());

        // Act
        apply_aria_metrics(&mut p, "5 likes, 200 views");

        // Assert
        assert_eq!(p.likes, Some(5));
        assert_eq!(p.impressions, Some(200));
        assert_eq!(p.comments, None);
        assert_eq!(p.shares, None);
    }

    #[test]
    fn own_status_id_filters_reposts() {
        // Arrange / Act / Assert — own tweet matches, repost of another
        // account does not.
        assert_eq!(
            own_status_id("/cawyuacx/status/1866453723837443", "cawyuacx"),
            Some("1866453723837443".to_string())
        );
        assert_eq!(own_status_id("/someoneelse/status/123", "cawyuacx"), None);
        assert_eq!(
            own_status_id("/cawyuacx/status/123/photo/1", "cawyuacx"),
            Some("123".into())
        );
    }
}
