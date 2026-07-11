//! Optional background crawl scheduler.
//!
//! This keeps the discovery store warm for downstream hotspot analysis.
//! It deliberately runs platform crawls serially because pwright's CLI
//! state is process-directory scoped.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use multipost_core::{CrawlOptions, Platform};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::state::{AppState, CrawlPlatformRunStatus};

#[derive(Debug, Clone)]
struct SchedulerConfig {
    platforms: Vec<Platform>,
    duration_secs: u64,
    interval_secs: u64,
    initial_delay_secs: u64,
    youtube_urls: Vec<String>,
}

pub fn spawn_if_configured(state: Arc<AppState>) -> Option<JoinHandle<()>> {
    let config = match SchedulerConfig::from_env() {
        Ok(Some(c)) => c,
        Ok(None) => {
            info!("crawl scheduler disabled");
            return None;
        }
        Err(e) => {
            warn!(error = %e, "crawl scheduler config invalid; disabled");
            return None;
        }
    };
    info!(
        platforms = ?config.platforms,
        duration_secs = config.duration_secs,
        interval_secs = config.interval_secs,
        "crawl scheduler enabled"
    );
    if let Ok(mut status) = state.crawl_scheduler_status.lock() {
        status.enabled = true;
        status.configured_platforms = config.platforms.clone();
    }
    Some(tokio::spawn(async move {
        run_scheduler(state, config).await;
    }))
}

async fn run_scheduler(state: Arc<AppState>, config: SchedulerConfig) {
    if config.initial_delay_secs > 0 {
        tokio::time::sleep(Duration::from_secs(config.initial_delay_secs)).await;
    }
    loop {
        for platform in &config.platforms {
            if let Err(e) = run_one(&state, &config, *platform).await {
                warn!(platform = platform.as_str(), error = %e, "scheduled crawl failed");
            }
        }
        tokio::time::sleep(Duration::from_secs(config.interval_secs)).await;
    }
}

async fn run_one(
    state: &Arc<AppState>,
    config: &SchedulerConfig,
    platform: Platform,
) -> Result<()> {
    let crawler = state
        .crawlers
        .get(&platform)
        .cloned()
        .with_context(|| format!("no crawler registered for {}", platform.as_str()))?;
    let source_urls = match platform {
        Platform::YouTube => config.youtube_urls.clone(),
        Platform::Toutiao
        | Platform::Twitter
        | Platform::Douyin
        | Platform::WxGzh
        | Platform::Bilibili => Vec::new(),
    };
    let _permit = state.crawl_permits.clone().acquire_owned().await?;
    let opts = CrawlOptions {
        duration_secs: config.duration_secs,
        source_urls,
        ..Default::default()
    };
    let started_at = chrono::Utc::now();
    if let Ok(mut status) = state.crawl_scheduler_status.lock() {
        status.running_platform = Some(platform);
        status.last_runs.insert(
            platform,
            CrawlPlatformRunStatus {
                started_at,
                finished_at: None,
                items_captured: None,
                last_error: None,
            },
        );
    }
    info!(platform = platform.as_str(), "scheduled crawl starting");
    let items = match crawler.run(&opts).await {
        Ok(items) => items,
        Err(e) => {
            if let Ok(mut status) = state.crawl_scheduler_status.lock() {
                status.running_platform = None;
                status.last_runs.insert(
                    platform,
                    CrawlPlatformRunStatus {
                        started_at,
                        finished_at: Some(chrono::Utc::now()),
                        items_captured: None,
                        last_error: Some(e.to_string()),
                    },
                );
            }
            return Err(e.into());
        }
    };
    let count = items.len();
    if count > 0 {
        state.discovered.upsert_many(&items).await?;
    }
    if let Ok(mut status) = state.crawl_scheduler_status.lock() {
        status.running_platform = None;
        status.last_runs.insert(
            platform,
            CrawlPlatformRunStatus {
                started_at,
                finished_at: Some(chrono::Utc::now()),
                items_captured: Some(count),
                last_error: None,
            },
        );
    }
    info!(
        platform = platform.as_str(),
        count, "scheduled crawl complete"
    );
    Ok(())
}

impl SchedulerConfig {
    fn from_env() -> Result<Option<Self>> {
        if !env_bool("MULTIPOST_CRAWL_ENABLED") {
            return Ok(None);
        }
        let platforms = env_list("MULTIPOST_CRAWL_PLATFORMS");
        let platforms = if platforms.is_empty() {
            vec![Platform::YouTube, Platform::Toutiao, Platform::Twitter]
        } else {
            platforms
                .iter()
                .map(|p| parse_platform(p))
                .collect::<Result<Vec<_>>>()?
        };
        if platforms.is_empty() {
            bail!("MULTIPOST_CRAWL_PLATFORMS resolved to an empty set");
        }
        let duration_secs = env_u64("MULTIPOST_CRAWL_DURATION_SECS", 30)?;
        let interval_secs = env_u64("MULTIPOST_CRAWL_INTERVAL_SECS", 900)?;
        if duration_secs == 0 {
            bail!("MULTIPOST_CRAWL_DURATION_SECS must be > 0");
        }
        if interval_secs == 0 {
            bail!("MULTIPOST_CRAWL_INTERVAL_SECS must be > 0");
        }
        let initial_delay_secs = env_u64("MULTIPOST_CRAWL_INITIAL_DELAY_SECS", 0)?;
        let youtube_urls = env_list("MULTIPOST_YOUTUBE_CRAWL_URLS")
            .into_iter()
            .chain(env_list("MULTIPOST_CRAWL_SOURCE_URLS"))
            .collect::<Vec<_>>();
        if platforms.contains(&Platform::YouTube) && youtube_urls.is_empty() {
            bail!("youtube scheduled crawl requires MULTIPOST_YOUTUBE_CRAWL_URLS");
        }
        Ok(Some(Self {
            platforms,
            duration_secs,
            interval_secs,
            initial_delay_secs,
            youtube_urls,
        }))
    }
}

fn env_bool(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse()
            .with_context(|| format!("parse {name}={v:?}")),
        _ => Ok(default),
    }
}

fn env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_platform(raw: &str) -> Result<Platform> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "youtube" | "yt" => Ok(Platform::YouTube),
        "toutiao" | "tt" => Ok(Platform::Toutiao),
        "twitter" | "x" => Ok(Platform::Twitter),
        other => bail!("unsupported scheduled crawl platform: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_platforms() {
        assert_eq!(parse_platform("youtube").unwrap(), Platform::YouTube);
        assert_eq!(parse_platform("x").unwrap(), Platform::Twitter);
        assert_eq!(parse_platform("toutiao").unwrap(), Platform::Toutiao);
    }
}
