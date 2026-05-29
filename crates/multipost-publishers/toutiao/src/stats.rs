//! Profile-stats collector for Toutiao 头条号.
//!
//! Drives the logged-in dashboard over CDP and pulls stats with two
//! authenticated `fetch`es (cookies ride along — no token signing needed):
//!
//! - `/mp/fe_api/home/merge_v2` → `data.statistic.data`: account totals
//!   (followers, income, reads/plays).
//! - `/api/feed/mp_provider/v1/` (paginated by `offset`/`count`) → the
//!   account's own works, each with `itemCounter` per-post stats (展现
//!   `showCount`, 阅读 `readCount`, 点赞 `diggCount`, 评论, 收藏 `repinCount`,
//!   播放 `videoWatchCount`).
//!
//! `merge_v2` only returns ~4 recent works and ignores cursors, so the
//! per-post stats come from the paginable `mp_provider` feed. That feed
//! requires the logged-in `visited_uid`, which the dashboard stashes in
//! `localStorage["__tea_cache_tokens_1231"].user_unique_id`.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use multipost_core::{
    AccountStats, Platform, PostStats, PublishError, Result, StatsCollector, StatsOptions,
    StatsSnapshot,
};
use serde_json::Value;

use crate::cdp::{BrowserSession, PageSession};

const DASHBOARD_URL: &str = "https://mp.toutiao.com/profile_v4/index";
const MERGE_URL: &str = "/mp/fe_api/home/merge_v2?app_id=1231";
const PAGE_SIZE: usize = 20;

/// Toutiao profile-stats collector. Stateless.
#[derive(Debug, Default, Clone)]
pub struct ToutiaoStatsCollector;

impl ToutiaoStatsCollector {
    /// Construct a new collector.
    pub fn new() -> Self {
        Self
    }
}

fn transient(e: impl std::fmt::Display) -> PublishError {
    PublishError::Transient(format!("toutiao stats: {e}"))
}

#[async_trait]
impl StatsCollector for ToutiaoStatsCollector {
    fn platform(&self) -> Platform {
        Platform::Toutiao
    }

    async fn collect(&self, opts: &StatsOptions) -> Result<StatsSnapshot> {
        let cdp = opts
            .cdp_url
            .as_deref()
            .ok_or_else(|| PublishError::Rejected("toutiao stats: cdp_url required".into()))?;
        let session = BrowserSession::connect(cdp).await.map_err(transient)?;
        let tab = session.create_tab(DASHBOARD_URL).await.map_err(transient)?;
        let mut page = session.open_page(&tab).await.map_err(transient)?;

        wait_dashboard_ready(&mut page).await?;

        let now = Utc::now();
        let account = fetch_account(&mut page, now).await?;
        let uid = fetch_uid(&mut page).await?;
        let posts = fetch_posts(&mut page, &uid, opts.max_posts, now).await?;

        let _ = session.close_tab(&tab.id).await;
        Ok(StatsSnapshot { account, posts })
    }
}

/// Poll until the dashboard is logged in and the analytics token (which
/// carries `visited_uid`) has been written to localStorage.
async fn wait_dashboard_ready(page: &mut PageSession) -> Result<()> {
    let deadline = Duration::from_secs(45);
    let start = Instant::now();
    loop {
        let ready = page
            .evaluate(
                // Guarded: a freshly-created tab starts at about:blank, where
                // touching localStorage throws SecurityError — swallow it and
                // keep polling until the dashboard document is live.
                r#"(() => {
                    try {
                        const onMp = location.host.includes('mp.toutiao.com')
                            && !location.href.includes('login')
                            && !location.href.includes('passport');
                        if (!onMp) return false;
                        return !!localStorage.getItem('__tea_cache_tokens_1231');
                    } catch (e) { return false; }
                })()"#,
            )
            .await
            .map_err(transient)?
            .as_bool()
            .unwrap_or(false);
        if ready {
            return Ok(());
        }
        if start.elapsed() > deadline {
            return Err(PublishError::Transient(
                "toutiao stats: dashboard not ready within 45s (logged out?)".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Read the logged-in `visited_uid` the `mp_provider` feed requires.
async fn fetch_uid(page: &mut PageSession) -> Result<String> {
    let v = page
        .evaluate(
            r#"(() => {
                try {
                    const t = JSON.parse(localStorage.getItem('__tea_cache_tokens_1231') || '{}');
                    if (t.user_unique_id) return String(t.user_unique_id);
                } catch (e) {}
                try {
                    const s = JSON.parse(localStorage.getItem('SLARDARim_web_sdk') || '{}');
                    if (s.userId) return String(s.userId).split(':').pop();
                } catch (e) {}
                return '';
            })()"#,
        )
        .await
        .map_err(transient)?;
    let uid = v.as_str().unwrap_or("").to_string();
    if uid.is_empty() {
        return Err(PublishError::Transient(
            "toutiao stats: could not read visited_uid from localStorage".into(),
        ));
    }
    Ok(uid)
}

/// Fetch + parse the account-level `merge_v2` statistic block.
async fn fetch_account(page: &mut PageSession, now: DateTime<Utc>) -> Result<AccountStats> {
    let js = format!(
        r#"(async () => {{
            const r = await fetch("{MERGE_URL}", {{credentials: 'include'}});
            return await r.json();
        }})()"#
    );
    let v = page.evaluate(&js).await.map_err(transient)?;
    let stat = v.pointer("/data/statistic/data").unwrap_or(&Value::Null);

    let mut account = AccountStats::new(Platform::Toutiao, now);
    account.followers = loose_i64(&stat["total_subscribe_count"]);
    account.post_count = loose_i64(&stat["thread_count"]);
    account.total_views = loose_i64(&stat["total_read_play_count"]);
    account.total_income = loose_f64(&stat["total_income"]);
    account.yesterday_followers = loose_i64(&stat["yesterday_fans_count"]);
    account.yesterday_views = loose_i64(&stat["yesterday_read_count"]);
    account.yesterday_income = loose_f64(&stat["yesterday_income"]);
    if let Some(plays) = loose_i64(&stat["yesterday_play_count"]) {
        account
            .metadata
            .insert("yesterday_play_count".into(), plays.into());
    }
    Ok(account)
}

/// Page through `mp_provider` collecting up to `max_posts` works.
async fn fetch_posts(
    page: &mut PageSession,
    uid: &str,
    max_posts: usize,
    now: DateTime<Utc>,
) -> Result<Vec<PostStats>> {
    let mut posts = Vec::new();
    let mut offset: i64 = 0;
    while posts.len() < max_posts {
        let count = PAGE_SIZE.min(max_posts - posts.len());
        let url = mp_provider_url(uid, offset, count);
        let js = format!(
            r#"(async () => {{
                const r = await fetch("{url}", {{credentials: 'include'}});
                return await r.json();
            }})()"#
        );
        let v = page.evaluate(&js).await.map_err(transient)?;
        let cells = v["data"].as_array().cloned().unwrap_or_default();
        if cells.is_empty() {
            break;
        }
        for cell in &cells {
            if let Some(p) = parse_cell(cell, now) {
                posts.push(p);
            }
        }
        // `offset` is a numeric index into the works list (0, 20, 40, …) —
        // NOT the timestamp the response echoes in its own `offset` field.
        // Advance by the page size we just requested.
        if !v["has_more"].as_bool().unwrap_or(false) {
            break;
        }
        offset += count as i64;
    }
    posts.truncate(max_posts);
    Ok(posts)
}

/// Build the `mp_provider` feed URL. The genre switch + client_extra_params
/// are URL-encoded constants from the live request; only offset/count/uid vary.
fn mp_provider_url(uid: &str, offset: i64, count: usize) -> String {
    format!(
        "/api/feed/mp_provider/v1/?provider_type=mp_provider&aid=13&app_name=news_article\
         &category=mp_all&stream_api_version=88\
         &genre_type_switch=%7B%22repost%22%3A1%2C%22small_video%22%3A1%2C%22toutiao_graphic%22%3A1%2C%22weitoutiao%22%3A1%2C%22xigua_video%22%3A1%7D\
         &device_platform=pc&platform_id=0&visited_uid={uid}&offset={offset}&count={count}&keyword=\
         &client_extra_params=%7B%22category%22%3A%22mp_all%22%2C%22real_app_id%22%3A%221231%22%2C%22need_forward%22%3A%22true%22%2C%22offset_mode%22%3A%221%22%2C%22page_index%22%3A%221%22%2C%22status%22%3A%228%22%2C%22source%22%3A%220%22%7D&app_id=1231"
    )
}

/// Decode one `mp_provider` cell into a [`PostStats`]. `None` if the cell
/// has no recognizable work payload.
fn parse_cell(cell: &Value, now: DateTime<Utc>) -> Option<PostStats> {
    // assembleCell may be a nested object or a JSON-encoded string.
    let assemble = match &cell["assembleCell"] {
        Value::String(s) => serde_json::from_str::<Value>(s).ok()?,
        v @ Value::Object(_) => v.clone(),
        _ => return None,
    };
    let item = &assemble["itemCell"];
    let base = &item["articleBase"];
    let counter = &item["itemCounter"];

    let post_id = base["groupID"]
        .as_str()
        .map(str::to_string)
        .or_else(|| loose_i64(&base["groupID"]).map(|n| n.to_string()))
        .or_else(|| base["gidStr"].as_str().map(str::to_string))?;

    let mut p = PostStats::new(Platform::Toutiao, post_id, now);
    p.title = base["title"].as_str().unwrap_or_default().to_string();
    p.created_at = loose_i64(&base["createTime"])
        .or_else(|| loose_i64(&base["publishTime"]))
        .and_then(|s| Utc.timestamp_opt(s, 0).single());
    p.post_type = classify(&item["articleClassification"]);
    p.impressions = loose_i64(&counter["showCount"]);
    p.reads = loose_i64(&counter["readCount"]);
    p.likes = loose_i64(&counter["diggCount"]);
    p.comments = loose_i64(&counter["commentCount"]);
    p.bookmarks = loose_i64(&counter["repinCount"]);
    p.plays = loose_i64(&counter["videoWatchCount"]);
    Some(p)
}

/// Map Toutiao's `articleClassification` to a short human label.
/// `groupSource == 5` is 微头条; `articleType` 1 marks long-form articles;
/// xigua videos otherwise. Falls back to "文章".
fn classify(c: &Value) -> String {
    let group_source = loose_i64(&c["groupSource"]).unwrap_or(0);
    let article_type = loose_i64(&c["articleType"]).unwrap_or(0);
    if group_source == 5 {
        "微头条".to_string()
    } else if matches!(article_type, 0 | 1) {
        "文章".to_string()
    } else {
        "视频".to_string()
    }
}

/// Parse an i64 from a JSON value that may be a number or a numeric string.
fn loose_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Parse an f64 from a JSON value that may be a number or a numeric string.
fn loose_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn loose_parsers_handle_string_and_number() {
        // Arrange / Act / Assert
        assert_eq!(loose_i64(&json!("848")), Some(848));
        assert_eq!(loose_i64(&json!(848)), Some(848));
        assert_eq!(loose_i64(&json!(null)), None);
        assert_eq!(loose_f64(&json!(6.21)), Some(6.21));
        assert_eq!(loose_f64(&json!("0.14")), Some(0.14));
    }

    #[test]
    fn parse_cell_extracts_counters_and_id() {
        // Arrange — minimal mp_provider cell with itemCounter + articleBase.
        let cell = json!({
            "assembleCell": {
                "itemCell": {
                    "articleBase": {
                        "groupID": "1866453723837443",
                        "title": "测试标题",
                        "createTime": 1779988979
                    },
                    "articleClassification": { "groupSource": 5, "articleType": 0 },
                    "itemCounter": {
                        "showCount": 883, "readCount": 53, "diggCount": 2,
                        "commentCount": 1, "repinCount": 4, "videoWatchCount": 0
                    }
                }
            }
        });

        // Act
        let p = parse_cell(&cell, Utc::now()).unwrap();

        // Assert
        assert_eq!(p.post_id, "1866453723837443");
        assert_eq!(p.title, "测试标题");
        assert_eq!(p.post_type, "微头条");
        assert_eq!(p.impressions, Some(883));
        assert_eq!(p.reads, Some(53));
        assert_eq!(p.likes, Some(2));
        assert_eq!(p.comments, Some(1));
        assert_eq!(p.bookmarks, Some(4));
        assert!(p.created_at.is_some());
    }

    #[test]
    fn parse_cell_handles_string_encoded_assemblecell() {
        // Arrange — some responses encode assembleCell as a JSON string.
        let inner = json!({
            "itemCell": {
                "articleBase": { "gidStr": "999", "title": "x" },
                "articleClassification": { "groupSource": 0, "articleType": 1 },
                "itemCounter": { "showCount": 10 }
            }
        });
        let cell = json!({ "assembleCell": inner.to_string() });

        // Act
        let p = parse_cell(&cell, Utc::now()).unwrap();

        // Assert
        assert_eq!(p.post_id, "999");
        assert_eq!(p.post_type, "文章");
        assert_eq!(p.impressions, Some(10));
    }
}
