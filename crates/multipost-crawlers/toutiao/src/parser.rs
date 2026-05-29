//! Decode a single `/api/pc/list/feed` response body into
//! [`DiscoveredItem`]s.
//!
//! The response shape (May 2026):
//!
//! ```json
//! {
//!   "message": "success",
//!   "data": [
//!     {
//!       "item_id": "7512...",
//!       "title": "...",
//!       "source": "齐鲁壹点",
//!       "media_name": "...",
//!       "article_url": "https://...",
//!       "read_count": 339,
//!       "like_count": 2,
//!       "comment_count": 0,
//!       "share_count": 3,
//!       "repin_count": 1,
//!       "behot_time": 1779674687,
//!       "cell_type": 0,
//!       "tag": "news_finance",
//!       "abstract": "..."
//!     },
//!     ...
//!   ]
//! }
//! ```
//!
//! Cell-type quirks:
//! * `cell_type == 48` — promoted card / ad widget; no title or
//!   metrics. Filtered out.
//! * `cell_type == 32` — 微头条 (short-form). No `source` field; the
//!   "body" is in `title` and the author may be elsewhere.

use std::collections::HashMap;

use chrono::{DateTime, TimeZone, Utc};
use multipost_core::{DiscoveredItem, DiscoveryMetrics, Platform};
use serde_json::Value;

/// Parse one feed response into normalized items.
///
/// Returns `Ok(Vec)` (possibly empty) on a well-formed payload; returns
/// `Err` only if the top-level JSON is unparseable. Unknown / missing
/// per-item fields are tolerated — they end up as `None`.
pub fn decode_feed_response(
    raw_json: &str,
    captured_at: DateTime<Utc>,
) -> Result<Vec<DiscoveredItem>, serde_json::Error> {
    let v: Value = serde_json::from_str(raw_json)?;
    let arr = match v.get("data").and_then(|d| d.as_array()) {
        Some(a) => a,
        None => return Ok(Vec::new()),
    };
    let mut out = Vec::with_capacity(arr.len());
    for raw in arr {
        if let Some(item) = decode_item(raw, captured_at) {
            out.push(item);
        }
    }
    Ok(out)
}

fn decode_item(raw: &Value, captured_at: DateTime<Utc>) -> Option<DiscoveredItem> {
    // cell_type 48 = ad/widget, drop unconditionally
    let cell_type = raw.get("cell_type").and_then(Value::as_i64);
    if cell_type == Some(48) {
        return None;
    }
    let item_id = raw.get("item_id").and_then(Value::as_str)?.to_string();
    if item_id.is_empty() {
        return None;
    }

    let title = str_or_empty(raw, "title");
    let abstract_ = str_or_empty(raw, "abstract");
    let body = if title.is_empty() {
        abstract_.clone()
    } else {
        title.clone()
    };

    let source = str_or_empty(raw, "source");
    let media_name = str_or_empty(raw, "media_name");
    // For micro-posts (cell_type 32) `source` may be empty; fall back
    // to media_name. Either may still be empty for fully-system items.
    let author_handle = if !source.is_empty() {
        source.clone()
    } else {
        media_name.clone()
    };
    let author_name = if !media_name.is_empty() && media_name != author_handle {
        Some(media_name)
    } else {
        None
    };

    let url = raw
        .get("article_url")
        .and_then(Value::as_str)
        .or_else(|| raw.get("share_url").and_then(Value::as_str))
        .map(|s| s.to_string());

    let metrics = DiscoveryMetrics {
        read_count: i64_field(raw, "read_count").or_else(|| i64_field(raw, "go_detail_count")),
        like_count: i64_field(raw, "like_count").or_else(|| i64_field(raw, "digg_count")),
        comment_count: i64_field(raw, "comment_count"),
        share_count: i64_field(raw, "share_count"),
        view_count: None, // not surfaced on Toutiao feed
        bookmark_count: i64_field(raw, "repin_count"),
    };

    // captured_at on the row: prefer the publish time (behot_time, in
    // seconds) when present, else fall back to crawl wall time.
    let captured = raw
        .get("behot_time")
        .and_then(Value::as_i64)
        .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
        .unwrap_or(captured_at);

    let mut metadata: HashMap<String, Value> = HashMap::new();
    for key in [
        "cell_type",
        "tag",
        "tip",
        "abstract",
        "behot_time",
        "group_id",
    ] {
        if let Some(v) = raw.get(key) {
            metadata.insert(key.to_string(), v.clone());
        }
    }

    Some(DiscoveredItem {
        platform: Platform::Toutiao,
        item_id,
        captured_at: captured,
        author_handle,
        author_name,
        body,
        url,
        metrics,
        metadata,
    })
}

fn str_or_empty(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn i64_field(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.timestamp_millis_opt(1_700_000_000_000)
            .single()
            .unwrap()
    }

    #[test]
    fn decode_filters_ad_widgets() {
        // Arrange
        let payload = serde_json::json!({
            "data": [
                { "item_id": "1", "title": "real", "source": "src",
                  "read_count": 100, "cell_type": 0 },
                { "item_id": "ad-x", "cell_type": 48 }
            ]
        })
        .to_string();

        // Act
        let items = decode_feed_response(&payload, now()).unwrap();

        // Assert
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "1");
    }

    #[test]
    fn decode_extracts_metrics_and_falls_back_for_micro_posts() {
        // Arrange
        let payload = serde_json::json!({
            "data": [
                {
                    "item_id": "weitoutiao-1",
                    "title": "微头条正文",
                    "cell_type": 32,
                    "media_name": "Author Display",
                    "read_count": 1234,
                    "like_count": 5,
                    "comment_count": 2,
                    "share_count": 1,
                    "repin_count": 9,
                    "behot_time": 1_700_000_000
                }
            ]
        })
        .to_string();

        // Act
        let items = decode_feed_response(&payload, now()).unwrap();

        // Assert
        let it = &items[0];
        assert_eq!(it.author_handle, "Author Display"); // fell back from missing source
        assert_eq!(it.metrics.read_count, Some(1234));
        assert_eq!(it.metrics.bookmark_count, Some(9)); // repin → bookmark
        assert_eq!(
            it.metadata.get("cell_type"),
            Some(&serde_json::Value::Number(32.into()))
        );
    }

    #[test]
    fn decode_empty_data_yields_empty() {
        let items = decode_feed_response(r#"{"data": []}"#, now()).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn decode_missing_data_yields_empty() {
        let items = decode_feed_response(r#"{"message":"ok"}"#, now()).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn decode_bad_json_errors() {
        let err = decode_feed_response("{not json", now());
        assert!(err.is_err());
    }
}
