//! Decode one HomeTimeline response into [`DiscoveredItem`]s.
//!
//! Response shape (May 2026):
//!
//! ```jsonc
//! {
//!   "data": { "home": { "home_timeline_urt": {
//!     "instructions": [
//!       { "type": "TimelineAddEntries", "entries": [
//!         { "content": { "entryType": "TimelineTimelineItem",
//!                        "itemContent": { "itemType": "TimelineTweet",
//!                          "tweet_results": { "result": { ... } } } } },
//!         { "content": { "entryType": "TimelineTimelineCursor", ... } },
//!         ...
//!       ] }
//!     ]
//!   } } }
//! }
//! ```
//!
//! `tweet_results.result.__typename` may be `Tweet` or
//! `TweetWithVisibilityResults` (the latter wraps a `.tweet` field).
//! Promoted tweets (`itemContent.promotedMetadata` present) are
//! skipped. `TimelineTimelineCursor` entries are skipped.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use multipost_core::{DiscoveredItem, DiscoveryMetrics, Platform};
use serde_json::Value;
use tracing::debug;

/// Parse one HomeTimeline response body into normalized items.
pub fn decode_home_timeline(
    raw_json: &str,
    captured_at: DateTime<Utc>,
) -> Result<Vec<DiscoveredItem>, serde_json::Error> {
    let v: Value = serde_json::from_str(raw_json)?;
    let instructions = v
        .pointer("/data/home/home_timeline_urt/instructions")
        .and_then(Value::as_array);
    let Some(instructions) = instructions else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for inst in instructions {
        let entries = match inst.get("entries").and_then(Value::as_array) {
            Some(e) => e,
            None => continue,
        };
        for entry in entries {
            collect_from_entry(entry, captured_at, &mut out);
        }
    }
    Ok(out)
}

fn collect_from_entry(entry: &Value, captured_at: DateTime<Utc>, out: &mut Vec<DiscoveredItem>) {
    let content = match entry.get("content") {
        Some(c) => c,
        None => return,
    };
    match content.get("entryType").and_then(Value::as_str) {
        Some("TimelineTimelineItem") => {
            if let Some(item) = decode_item_content(content.get("itemContent"), captured_at) {
                out.push(item);
            }
        }
        Some("TimelineTimelineModule") => {
            // Threads / conversation bundles. Walk the inner items.
            if let Some(items) = content.get("items").and_then(Value::as_array) {
                for inner in items {
                    let ic = inner.get("item").and_then(|i| i.get("itemContent"));
                    if let Some(d) = decode_item_content(ic, captured_at) {
                        out.push(d);
                    }
                }
            }
        }
        _ => {
            // Cursors, separators, etc. — skip silently.
        }
    }
}

fn decode_item_content(ic: Option<&Value>, captured_at: DateTime<Utc>) -> Option<DiscoveredItem> {
    let ic = ic?;
    if ic.get("itemType").and_then(Value::as_str) != Some("TimelineTweet") {
        return None;
    }
    // Skip promoted tweets — the recommendation tone is what we want.
    if ic.get("promotedMetadata").is_some() {
        return None;
    }
    let mut tw = ic.get("tweet_results")?.get("result")?;
    if tw.get("__typename").and_then(Value::as_str) == Some("TweetWithVisibilityResults") {
        tw = tw.get("tweet")?;
    }
    let rest_id = tw.get("rest_id").and_then(Value::as_str)?.to_string();
    let legacy = tw.get("legacy")?;
    let body = legacy
        .get("full_text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let user = tw.pointer("/core/user_results/result")?;
    let user_core = user.get("core");
    let user_legacy = user.get("legacy");
    let screen_name = user_core
        .and_then(|c| c.get("screen_name"))
        .and_then(Value::as_str)
        .or_else(|| {
            user_legacy
                .and_then(|l| l.get("screen_name"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    let display_name = user_core
        .and_then(|c| c.get("name"))
        .and_then(Value::as_str)
        .or_else(|| user_legacy.and_then(|l| l.get("name")).and_then(Value::as_str))
        .unwrap_or("");
    let author_handle = screen_name.to_string();
    let author_name = if !display_name.is_empty() && display_name != author_handle {
        Some(display_name.to_string())
    } else {
        None
    };

    let metrics = DiscoveryMetrics {
        read_count: None,
        like_count: legacy.get("favorite_count").and_then(Value::as_i64),
        comment_count: legacy.get("reply_count").and_then(Value::as_i64),
        share_count: legacy.get("retweet_count").and_then(Value::as_i64),
        view_count: tw
            .pointer("/views/count")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<i64>().ok()),
        bookmark_count: legacy.get("bookmark_count").and_then(Value::as_i64),
    };

    let url = if !author_handle.is_empty() {
        Some(format!(
            "https://x.com/{author_handle}/status/{rest_id}"
        ))
    } else {
        None
    };

    let mut metadata: HashMap<String, Value> = HashMap::new();
    for key in ["lang", "created_at", "quote_count", "conversation_id_str"] {
        if let Some(v) = legacy.get(key) {
            metadata.insert(key.to_string(), v.clone());
        }
    }
    if let Some(media) = legacy.pointer("/entities/media") {
        metadata.insert("media".to_string(), media.clone());
    }
    debug!(rest_id = %rest_id, handle = %author_handle, "decoded tweet");

    Some(DiscoveredItem {
        platform: Platform::Twitter,
        item_id: rest_id,
        captured_at,
        author_handle,
        author_name,
        body,
        url,
        metrics,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.timestamp_millis_opt(1_700_000_000_000).single().unwrap()
    }

    fn make_tweet_entry(rest_id: &str, screen: &str, text: &str, faves: i64) -> Value {
        serde_json::json!({
            "content": {
                "entryType": "TimelineTimelineItem",
                "itemContent": {
                    "itemType": "TimelineTweet",
                    "tweet_results": { "result": {
                        "__typename": "Tweet",
                        "rest_id": rest_id,
                        "core": { "user_results": { "result": {
                            "core": { "screen_name": screen, "name": "Display" }
                        } } },
                        "legacy": {
                            "full_text": text,
                            "favorite_count": faves,
                            "retweet_count": 2,
                            "reply_count": 1,
                            "quote_count": 3,
                            "bookmark_count": 4,
                            "lang": "en"
                        },
                        "views": { "count": "1234" }
                    } }
                }
            }
        })
    }

    fn wrap_response(entries: Vec<Value>) -> String {
        serde_json::json!({
            "data": { "home": { "home_timeline_urt": {
                "instructions": [
                    { "type": "TimelineAddEntries", "entries": entries }
                ]
            } } }
        })
        .to_string()
    }

    #[test]
    fn decode_extracts_basic_tweet() {
        // Arrange
        let payload = wrap_response(vec![make_tweet_entry("123", "alice", "hi world", 50)]);

        // Act
        let items = decode_home_timeline(&payload, now()).unwrap();

        // Assert
        assert_eq!(items.len(), 1);
        let it = &items[0];
        assert_eq!(it.item_id, "123");
        assert_eq!(it.author_handle, "alice");
        assert_eq!(it.body, "hi world");
        assert_eq!(it.metrics.like_count, Some(50));
        assert_eq!(it.metrics.share_count, Some(2));
        assert_eq!(it.metrics.view_count, Some(1234));
        assert_eq!(it.url.as_deref(), Some("https://x.com/alice/status/123"));
    }

    #[test]
    fn decode_unwraps_tweet_with_visibility_results() {
        // Arrange
        let mut inner = make_tweet_entry("456", "bob", "wrapped", 7);
        let result = inner["content"]["itemContent"]["tweet_results"]["result"].take();
        inner["content"]["itemContent"]["tweet_results"]["result"] = serde_json::json!({
            "__typename": "TweetWithVisibilityResults",
            "tweet": result
        });

        let payload = wrap_response(vec![inner]);

        // Act
        let items = decode_home_timeline(&payload, now()).unwrap();

        // Assert
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "456");
        assert_eq!(items[0].metrics.like_count, Some(7));
    }

    #[test]
    fn decode_skips_cursor_entries() {
        // Arrange
        let cursor = serde_json::json!({
            "content": { "entryType": "TimelineTimelineCursor", "cursorType": "Top" }
        });
        let payload = wrap_response(vec![cursor, make_tweet_entry("7", "x", "ok", 1)]);

        // Act
        let items = decode_home_timeline(&payload, now()).unwrap();

        // Assert
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "7");
    }

    #[test]
    fn decode_skips_promoted_tweets() {
        // Arrange
        let mut promoted = make_tweet_entry("ad-1", "sponsor", "buy now", 0);
        promoted["content"]["itemContent"]["promotedMetadata"] = serde_json::json!({});

        let payload = wrap_response(vec![promoted, make_tweet_entry("7", "x", "real", 5)]);

        // Act
        let items = decode_home_timeline(&payload, now()).unwrap();

        // Assert
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "7");
    }

    #[test]
    fn decode_walks_timeline_module_items() {
        // Arrange — a thread bundle with 2 inner tweets
        let inner1 = make_tweet_entry("100", "u", "first", 1);
        let inner2 = make_tweet_entry("101", "u", "second", 2);
        let module = serde_json::json!({
            "content": {
                "entryType": "TimelineTimelineModule",
                "items": [
                    { "item": { "itemContent": inner1["content"]["itemContent"].clone() } },
                    { "item": { "itemContent": inner2["content"]["itemContent"].clone() } }
                ]
            }
        });
        let payload = wrap_response(vec![module]);

        // Act
        let items = decode_home_timeline(&payload, now()).unwrap();

        // Assert
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].item_id, "100");
        assert_eq!(items[1].item_id, "101");
    }

    #[test]
    fn decode_empty_payload_yields_empty() {
        let items = decode_home_timeline(r#"{}"#, now()).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn decode_bad_json_errors() {
        let err = decode_home_timeline("nope", now());
        assert!(err.is_err());
    }
}
