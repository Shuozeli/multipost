use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use multipost_core::{
    CrawlOptions, Crawler, DiscoveredItem, DiscoveryMetrics, Platform, PublishError, Result,
};
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;
use tokio::time::sleep;
use tracing::debug;
use url::Url;

#[derive(Debug, Default, Clone)]
pub struct YouTubeCrawler;

const SCROLL_PIXELS: i32 = 2400;
const SCROLL_INTERVAL_SECS: u64 = 2;
const MAX_SCROLLS_PER_SOURCE: usize = 6;

impl YouTubeCrawler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Crawler for YouTubeCrawler {
    fn platform(&self) -> Platform {
        Platform::YouTube
    }

    async fn run(&self, opts: &CrawlOptions) -> Result<Vec<DiscoveredItem>> {
        let sources = youtube_sources(opts)?;
        let mut all = Vec::new();
        let per_source_scrolls = scroll_budget(opts.duration_secs, sources.len());

        for source in sources {
            let url = normalize_youtube_url(&source);
            pwright_run(opts, &["open", &url]).await?;
            sleep(Duration::from_secs(3)).await;

            for _ in 0..per_source_scrolls {
                let mut items = extract_visible_videos(opts, &url).await?;
                all.append(&mut items);
                let js = format!("window.scrollBy(0, {SCROLL_PIXELS}); 1");
                if let Err(e) = pwright_run(opts, &["eval", &js]).await {
                    debug!("youtube scroll eval failed: {e}");
                }
                sleep(Duration::from_secs(SCROLL_INTERVAL_SECS)).await;
            }
        }

        Ok(dedup_keep_last(all))
    }
}

fn youtube_sources(opts: &CrawlOptions) -> Result<Vec<String>> {
    if !opts.source_urls.is_empty() {
        return Ok(opts.source_urls.clone());
    }
    let env = std::env::var("MULTIPOST_YOUTUBE_CRAWL_URLS")
        .or_else(|_| std::env::var("MULTIPOST_CRAWL_SOURCE_URLS"))
        .unwrap_or_default();
    let sources: Vec<String> = env
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if sources.is_empty() {
        return Err(PublishError::Transient(
            "youtube crawl requires --url or MULTIPOST_YOUTUBE_CRAWL_URLS".into(),
        ));
    }
    Ok(sources)
}

fn scroll_budget(duration_secs: u64, source_count: usize) -> usize {
    let sources = source_count.max(1) as u64;
    let per_source_secs = duration_secs.max(5) / sources;
    let scrolls = (per_source_secs / SCROLL_INTERVAL_SECS).max(1) as usize;
    scrolls.min(MAX_SCROLLS_PER_SOURCE)
}

fn normalize_youtube_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return raw.to_string();
    };
    let host = url.host_str().unwrap_or_default();
    if !host.ends_with("youtube.com") && !host.ends_with("youtu.be") {
        return raw.to_string();
    }
    let path = url.path().trim_end_matches('/');
    if path.starts_with("/watch") || path.ends_with("/videos") || path.ends_with("/shorts") {
        return url.to_string();
    }
    if path.starts_with("/@") || path.starts_with("/channel/") || path.starts_with("/c/") {
        url.set_path(&format!("{path}/videos"));
    }
    url.to_string()
}

async fn extract_visible_videos(
    opts: &CrawlOptions,
    source_url: &str,
) -> Result<Vec<DiscoveredItem>> {
    let stdout = pwright_output(opts, &["eval", EXTRACT_JS]).await?;
    let trimmed = extract_json_payload(&stdout).unwrap_or_else(|| stdout.trim());
    let raw: Vec<RawVideo> = serde_json::from_str(trimmed).map_err(|e| {
        let preview: String = trimmed.chars().take(160).collect();
        PublishError::Transient(format!(
            "youtube DOM extraction returned invalid JSON: {e}; output={preview:?}"
        ))
    })?;
    let now = Utc::now();
    let mut out = Vec::new();
    for video in raw {
        let Some(id) = video.video_id() else {
            continue;
        };
        let title = video.title.trim();
        if title.is_empty() {
            continue;
        }
        let mut metadata = HashMap::new();
        metadata.insert("source_url".into(), json!(source_url));
        metadata.insert("published_text".into(), json!(video.published_text));
        metadata.insert("duration_text".into(), json!(video.duration_text));
        if let Some(thumbnail) = video.thumbnail {
            metadata.insert("thumbnail_url".into(), json!(thumbnail));
        }

        out.push(DiscoveredItem {
            platform: Platform::YouTube,
            item_id: id,
            captured_at: now,
            author_handle: video.channel_handle.unwrap_or_default(),
            author_name: video.channel_name,
            body: title.to_string(),
            url: video.href,
            metrics: DiscoveryMetrics {
                view_count: parse_count(video.views_text.as_deref()),
                ..Default::default()
            },
            metadata,
        });
    }
    Ok(out)
}

fn extract_json_payload(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .map(str::trim)
        .rev()
        .find(|line| line.starts_with('[') || line.starts_with('{'))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVideo {
    title: String,
    href: Option<String>,
    views_text: Option<String>,
    published_text: Option<String>,
    duration_text: Option<String>,
    channel_name: Option<String>,
    channel_handle: Option<String>,
    thumbnail: Option<String>,
}

impl RawVideo {
    fn video_id(&self) -> Option<String> {
        let href = self.href.as_deref()?;
        if let Ok(url) = Url::parse(href) {
            if let Some(v) = url.query_pairs().find(|(k, _)| k == "v") {
                return Some(v.1.into_owned());
            }
            if url.host_str().is_some_and(|h| h.ends_with("youtu.be")) {
                return url
                    .path_segments()
                    .and_then(|mut s| s.next().map(ToOwned::to_owned));
            }
        }
        href.split("watch?v=")
            .nth(1)
            .and_then(|s| s.split(['&', '#']).next())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
    }
}

fn parse_count(raw: Option<&str>) -> Option<i64> {
    let text = raw?.to_ascii_lowercase();
    let mut number = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            number.push(ch);
        } else if !number.is_empty() {
            break;
        }
    }
    let base: f64 = number.parse().ok()?;
    let multiplier = if text.contains('万') {
        10_000.0
    } else if text.contains('亿') {
        100_000_000.0
    } else if text.contains('k') {
        1_000.0
    } else if text.contains('m') {
        1_000_000.0
    } else {
        1.0
    };
    Some((base * multiplier).round() as i64)
}

async fn pwright_run(opts: &CrawlOptions, args: &[&str]) -> Result<()> {
    let _ = pwright_output(opts, args).await?;
    Ok(())
}

async fn pwright_output(opts: &CrawlOptions, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new(&opts.pwright_bin);
    if let Some(c) = opts.cdp_url.as_deref() {
        cmd.env("PWRIGHT_CDP", c);
    }
    cmd.args(args)
        .stdout(Stdio::piped())
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
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn dedup_keep_last(items: Vec<DiscoveredItem>) -> Vec<DiscoveredItem> {
    let mut by_id: HashMap<String, DiscoveredItem> = HashMap::new();
    for it in items {
        by_id.insert(it.item_id.clone(), it);
    }
    by_id.into_values().collect()
}

const EXTRACT_JS: &str = r#"
(() => {
  const clean = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const pageTitle = clean(document.querySelector('ytd-channel-name #text, #channel-name #text')?.textContent);
  const handle = clean(document.querySelector('#channel-handle, yt-content-metadata-view-model span')?.textContent);
  const rows = [];
  const seen = new Set();
  const add = (row) => {
    const id = row?.videoId || '';
    const title = clean(row?.title);
    if (!id || seen.has(id) || !title) return;
    seen.add(id);
    rows.push({
      title,
      href: new URL(`/watch?v=${id}`, location.origin).toString(),
      viewsText: row.viewsText || null,
      publishedText: row.publishedText || null,
      durationText: row.durationText || null,
      channelName: pageTitle || null,
      channelHandle: handle || null,
      thumbnail: row.thumbnail || null
    });
  };
  const textContent = (v) => clean(v?.content || v?.simpleText || (Array.isArray(v?.runs) ? v.runs.map((r) => r.text || '').join('') : ''));
  const imageUrl = (v) => {
    const srcs = v?.thumbnailViewModel?.image?.sources || v?.sources || v?.thumbnails || [];
    return srcs[srcs.length - 1]?.url || null;
  };
  const durationText = (v) => {
    const overlays = v?.contentImage?.thumbnailViewModel?.overlays || [];
    for (const overlay of overlays) {
      const badges = overlay?.thumbnailBottomOverlayViewModel?.badges || [];
      for (const badge of badges) {
        const text = badge?.thumbnailBadgeViewModel?.text;
        if (text) return clean(text);
      }
    }
    return null;
  };
  const addLockup = (lockup) => {
    const id = lockup?.contentId || lockup?.rendererContext?.commandContext?.onTap?.innertubeCommand?.watchEndpoint?.videoId;
    const title = textContent(lockup?.metadata?.lockupMetadataViewModel?.title);
    const parts = (lockup?.metadata?.lockupMetadataViewModel?.metadata?.contentMetadataViewModel?.metadataRows || [])
      .flatMap((row) => row?.metadataParts || [])
      .map((part) => textContent(part?.text))
      .filter(Boolean);
    add({
      videoId: id,
      title,
      viewsText: parts.find((x) => /views|次观看|次觀看/i.test(x)),
      publishedText: parts.find((x) => /ago|前|Streamed|Premiered/i.test(x)),
      durationText: durationText(lockup),
      thumbnail: imageUrl(lockup?.contentImage)
    });
  };
  const walkInitial = (v) => {
    if (!v || typeof v !== 'object') return;
    if (v.lockupViewModel) addLockup(v.lockupViewModel);
    for (const child of Object.values(v)) walkInitial(child);
  };
  walkInitial(window.ytInitialData);
  for (const a of Array.from(document.querySelectorAll('a[href*="watch?v="]'))) {
    const href = a.href || '';
    const url = new URL(href, location.href);
    const id = url.searchParams.get('v');
    if (!id || seen.has(id)) continue;
    const title = clean(a.getAttribute('title') || a.getAttribute('aria-label') || a.textContent);
    if (!title || /^\d{1,2}:\d{2}/.test(title)) continue;
    seen.add(id);
    const card = a.closest('ytd-rich-item-renderer,ytd-grid-video-renderer,ytd-video-renderer,yt-lockup-view-model') || a.parentElement;
    const meta = Array.from(card?.querySelectorAll('#metadata-line span, .inline-metadata-item, yt-content-metadata-view-model span') || [])
      .map((x) => clean(x.textContent))
      .filter(Boolean);
    const img = card?.querySelector('img');
    const duration = clean(card?.querySelector('ytd-thumbnail-overlay-time-status-renderer span, .badge-shape-wiz__text')?.textContent);
    add({
      videoId: id,
      title,
      viewsText: meta.find((x) => /views|次观看|次觀看/i.test(x)) || null,
      publishedText: meta.find((x) => /ago|前|Streamed|Premiered/i.test(x)) || null,
      durationText: duration || null,
      thumbnail: img?.currentSrc || img?.src || null
    });
  }
  return rows;
})()
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_counts_supports_common_units() {
        assert_eq!(parse_count(Some("9.5K views")), Some(9500));
        assert_eq!(parse_count(Some("19K views")), Some(19000));
        assert_eq!(parse_count(Some("3.2万次观看")), Some(32000));
    }

    #[test]
    fn normalizes_channel_urls_to_videos_tab() {
        assert_eq!(
            normalize_youtube_url("https://www.youtube.com/@abc"),
            "https://www.youtube.com/@abc/videos"
        );
    }
}
