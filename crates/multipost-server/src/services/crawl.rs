//! `Crawl` gRPC service implementation.
//!
//! Same async-submit + long-poll shape as the Posts service: `Submit`
//! returns a job ID immediately, `GetJob` long-polls for terminal
//! state, and the actual crawl runs in a background tokio task.
//! Results are persisted to the SQLite-backed
//! [`multipost_storage::discovered::DiscoveredRepository`].

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tonic::{Request, Response, Status};
use tracing::{info, warn};
use uuid::Uuid;

use multipost_core::{CrawlOptions, DiscoveredItem, Platform, UserCrawlOptions};
use multipost_proto::crawl::crawl_server::Crawl as CrawlTrait;
use multipost_proto::crawl::{
    CrawlJob as ProtoCrawlJob, CrawlJobState as ProtoCrawlJobState,
    DiscoveredItem as ProtoDiscoveredItem, DiscoveryMetrics as ProtoMetrics, GetCrawlJobRequest,
    ListItemsRequest, ListItemsResponse, SubmitCrawlRequest, SubmitUserCrawlRequest,
};

use crate::state::{AppState, CrawlJobInternal};

const MIN_DURATION_SECS: u32 = 5;
const MAX_DURATION_SECS: u32 = 300;
const MAX_LONG_POLL_SECS: u32 = 300;
const MAX_LIST_ITEMS_LIMIT: u32 = 200;
const DEFAULT_LIST_ITEMS_LIMIT: u32 = 50;
const MIN_MAX_POSTS: u32 = 1;
const MAX_MAX_POSTS: u32 = 300;
const DEFAULT_MAX_POSTS: u32 = 100;
/// Safety stop on per-user crawl scrolling, in seconds.
const USER_CRAWL_MAX_DURATION_SECS: u64 = 180;

pub struct CrawlService {
    state: Arc<AppState>,
}

impl CrawlService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl CrawlTrait for CrawlService {
    async fn submit(
        &self,
        req: Request<SubmitCrawlRequest>,
    ) -> std::result::Result<Response<ProtoCrawlJob>, Status> {
        let r = req.into_inner();
        let platform = parse_platform(&r.platform)?;
        if !self.state.crawlers.contains_key(&platform) {
            return Err(Status::failed_precondition(format!(
                "no crawler registered for platform {:?}",
                platform
            )));
        }

        let duration = clamp(r.duration_secs, MIN_DURATION_SECS, MAX_DURATION_SECS);
        let source_urls: Vec<String> = r
            .source_urls
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let skip_persist = r.skip_persist;
        let job_id = Uuid::new_v4();
        let internal = CrawlJobInternal {
            id: job_id,
            platform,
            state: ProtoCrawlJobState::CrawlStatePending,
            duration_secs: duration,
            items_captured: 0,
            items: Vec::new(),
            last_error: None,
            submitted_at: Utc::now(),
            finished_at: None,
        };
        {
            let mut map = self
                .state
                .crawl_jobs
                .lock()
                .map_err(|_| Status::internal("crawl_jobs poisoned"))?;
            map.insert(job_id, internal.clone());
        }
        self.state.emit_crawl_event(internal.clone());

        // Run the crawler in a background task.
        let st = self.state.clone();
        tokio::spawn(async move {
            run_crawl_job(st, job_id, platform, duration, source_urls, skip_persist).await;
        });

        Ok(Response::new(to_proto(
            &internal, /* include_items */ false,
        )))
    }

    async fn submit_user_crawl(
        &self,
        req: Request<SubmitUserCrawlRequest>,
    ) -> std::result::Result<Response<ProtoCrawlJob>, Status> {
        let r = req.into_inner();
        let platform = parse_platform(&r.platform)?;
        if !self.state.user_crawlers.contains_key(&platform) {
            return Err(Status::failed_precondition(format!(
                "no user crawler registered for platform {:?}",
                platform
            )));
        }
        let handle = r.handle.trim().to_string();
        if handle.is_empty() {
            return Err(Status::invalid_argument("handle is required"));
        }
        let max_posts = if r.max_posts == 0 {
            DEFAULT_MAX_POSTS
        } else {
            clamp(r.max_posts, MIN_MAX_POSTS, MAX_MAX_POSTS)
        };

        let job_id = Uuid::new_v4();
        let internal = CrawlJobInternal {
            id: job_id,
            platform,
            state: ProtoCrawlJobState::CrawlStatePending,
            // Report the safety-stop duration; user crawls are bounded by
            // post count, not time.
            duration_secs: USER_CRAWL_MAX_DURATION_SECS as u32,
            items_captured: 0,
            items: Vec::new(),
            last_error: None,
            submitted_at: Utc::now(),
            finished_at: None,
        };
        {
            let mut map = self
                .state
                .crawl_jobs
                .lock()
                .map_err(|_| Status::internal("crawl_jobs poisoned"))?;
            map.insert(job_id, internal.clone());
        }
        self.state.emit_crawl_event(internal.clone());

        let st = self.state.clone();
        tokio::spawn(async move {
            run_user_crawl_job(st, job_id, platform, handle, max_posts).await;
        });

        Ok(Response::new(to_proto(
            &internal, /* include_items */ false,
        )))
    }

    async fn get_job(
        &self,
        req: Request<GetCrawlJobRequest>,
    ) -> std::result::Result<Response<ProtoCrawlJob>, Status> {
        let r = req.into_inner();
        let job_id = Uuid::parse_str(&r.id)
            .map_err(|e| Status::invalid_argument(format!("bad job id: {e}")))?;
        let wait = clamp(r.wait_seconds, 0, MAX_LONG_POLL_SECS);

        // Fast path: already terminal, or no long-polling requested.
        if let Some(job) = self.snapshot(job_id)? {
            if wait == 0 || is_terminal(job.state) {
                return Ok(Response::new(to_proto(&job, /* include_items */ true)));
            }
        } else {
            return Err(Status::not_found(format!("crawl job {job_id} not found")));
        }

        // Slow path: subscribe to the bus, drain transitions for this job
        // until terminal or deadline.
        let mut rx = self.state.crawl_events.subscribe();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(wait as u64);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(ev)) if ev.id == job_id => {
                    if is_terminal(ev.state) {
                        return Ok(Response::new(to_proto(&ev, true)));
                    }
                }
                Ok(Ok(_)) => continue, // another job's event
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_) => break, // timeout
            }
        }

        // Return whatever state we have now.
        match self.snapshot(job_id)? {
            Some(j) => Ok(Response::new(to_proto(&j, true))),
            None => Err(Status::internal("crawl job vanished mid-poll")),
        }
    }

    async fn list_items(
        &self,
        req: Request<ListItemsRequest>,
    ) -> std::result::Result<Response<ListItemsResponse>, Status> {
        let r = req.into_inner();
        let platform = parse_platform(&r.platform)?;
        let limit = clamp(r.limit, 1, MAX_LIST_ITEMS_LIMIT).max(1);
        let limit = if limit == 0 {
            DEFAULT_LIST_ITEMS_LIMIT
        } else {
            limit
        };
        let items = self
            .state
            .discovered
            .recent(platform, limit as usize)
            .await
            .map_err(|e| Status::internal(format!("recent: {e}")))?;
        let resp = ListItemsResponse {
            items: items.iter().map(item_to_proto).collect(),
        };
        Ok(Response::new(resp))
    }
}

impl CrawlService {
    fn snapshot(&self, id: Uuid) -> std::result::Result<Option<CrawlJobInternal>, Status> {
        let map = self
            .state
            .crawl_jobs
            .lock()
            .map_err(|_| Status::internal("crawl_jobs poisoned"))?;
        Ok(map.get(&id).cloned())
    }
}

/// The background worker. Drives the platform's crawler, persists items
/// to the SQLite repo, and updates the in-memory job state.
async fn run_crawl_job(
    state: Arc<AppState>,
    job_id: Uuid,
    platform: Platform,
    duration: u32,
    source_urls: Vec<String>,
    skip_persist: bool,
) {
    transition(&state, job_id, |j| {
        j.state = ProtoCrawlJobState::CrawlStateRunning;
    });

    let crawler = match state.crawlers.get(&platform).cloned() {
        Some(c) => c,
        None => {
            fail(&state, job_id, "crawler vanished after submit");
            return;
        }
    };

    let opts = CrawlOptions {
        duration_secs: duration as u64,
        source_urls,
        ..Default::default()
    };
    let _permit = match state.crawl_permits.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(e) => {
            fail(&state, job_id, &format!("crawl permit closed: {e}"));
            return;
        }
    };
    let result = crawler.run(&opts).await;

    match result {
        Ok(items) => {
            // Persist before marking complete so a fast GetJob can also
            // succeed via ListItems.
            if !skip_persist
                && !items.is_empty()
                && let Err(e) = state.discovered.upsert_many(&items).await
            {
                warn!(%job_id, error = %e, "discovered.upsert_many failed");
            }
            info!(%job_id, count = items.len(), skip_persist, "crawl complete");
            transition(&state, job_id, |j| {
                j.items_captured = items.len() as u32;
                j.items = items;
                j.state = ProtoCrawlJobState::CrawlStateCompleted;
                j.finished_at = Some(Utc::now());
            });
        }
        Err(e) => {
            warn!(%job_id, error = %e, "crawl failed");
            fail(&state, job_id, &e.to_string());
        }
    }
}

/// The per-user crawl worker. Drives the platform's `UserCrawler`,
/// persists the account's posts to the same `discovered` repo, and
/// updates the in-memory job state. Mirrors [`run_crawl_job`].
async fn run_user_crawl_job(
    state: Arc<AppState>,
    job_id: Uuid,
    platform: Platform,
    handle: String,
    max_posts: u32,
) {
    transition(&state, job_id, |j| {
        j.state = ProtoCrawlJobState::CrawlStateRunning;
    });

    let crawler = match state.user_crawlers.get(&platform).cloned() {
        Some(c) => c,
        None => {
            fail(&state, job_id, "user crawler vanished after submit");
            return;
        }
    };

    let opts = UserCrawlOptions {
        handle: handle.clone(),
        max_posts: max_posts as usize,
        max_duration_secs: USER_CRAWL_MAX_DURATION_SECS,
        ..Default::default()
    };
    let result = crawler.crawl_user(&opts).await;

    match result {
        Ok(items) => {
            if !items.is_empty()
                && let Err(e) = state.discovered.upsert_many(&items).await
            {
                warn!(%job_id, error = %e, "discovered.upsert_many failed");
            }
            info!(%job_id, %handle, count = items.len(), "user crawl complete");
            transition(&state, job_id, |j| {
                j.items_captured = items.len() as u32;
                j.items = items;
                j.state = ProtoCrawlJobState::CrawlStateCompleted;
                j.finished_at = Some(Utc::now());
            });
        }
        Err(e) => {
            warn!(%job_id, %handle, error = %e, "user crawl failed");
            fail(&state, job_id, &e.to_string());
        }
    }
}

fn fail(state: &Arc<AppState>, job_id: Uuid, message: &str) {
    transition(state, job_id, |j| {
        j.state = ProtoCrawlJobState::CrawlStateFailed;
        j.last_error = Some(message.to_string());
        j.finished_at = Some(Utc::now());
    });
}

fn transition(state: &Arc<AppState>, job_id: Uuid, mutate: impl FnOnce(&mut CrawlJobInternal)) {
    let snapshot = {
        let mut map = match state.crawl_jobs.lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        let Some(j) = map.get_mut(&job_id) else {
            return;
        };
        mutate(j);
        j.clone()
    };
    state.emit_crawl_event(snapshot);
}

fn is_terminal(s: ProtoCrawlJobState) -> bool {
    matches!(
        s,
        ProtoCrawlJobState::CrawlStateCompleted | ProtoCrawlJobState::CrawlStateFailed
    )
}

fn clamp(v: u32, lo: u32, hi: u32) -> u32 {
    v.clamp(lo, hi)
}

fn parse_platform(s: &str) -> std::result::Result<Platform, Status> {
    match s.to_ascii_lowercase().as_str() {
        "youtube" => Ok(Platform::YouTube),
        "wx_gzh" | "wxgzh" | "wechat" | "wechat_mp" => Ok(Platform::WxGzh),
        "twitter" | "x" => Ok(Platform::Twitter),
        "douyin" => Ok(Platform::Douyin),
        "toutiao" => Ok(Platform::Toutiao),
        other => Err(Status::invalid_argument(format!(
            "unknown platform: {other}"
        ))),
    }
}

fn platform_str(p: Platform) -> &'static str {
    match p {
        Platform::YouTube => "youtube",
        Platform::WxGzh => "wx_gzh",
        Platform::Twitter => "twitter",
        Platform::Douyin => "douyin",
        Platform::Toutiao => "toutiao",
        Platform::Bilibili => "bilibili",
    }
}

fn to_proto(j: &CrawlJobInternal, include_items: bool) -> ProtoCrawlJob {
    ProtoCrawlJob {
        id: j.id.to_string(),
        platform: platform_str(j.platform).to_string(),
        state: j.state as i32,
        duration_secs: j.duration_secs,
        items_captured: j.items_captured,
        items: if include_items {
            j.items.iter().map(item_to_proto).collect()
        } else {
            Vec::new()
        },
        last_error: j.last_error.clone().unwrap_or_default(),
        submitted_at: Some(prost_types::Timestamp {
            seconds: j.submitted_at.timestamp(),
            nanos: 0,
        }),
        finished_at: j.finished_at.map(|t| prost_types::Timestamp {
            seconds: t.timestamp(),
            nanos: 0,
        }),
    }
}

fn item_to_proto(i: &DiscoveredItem) -> ProtoDiscoveredItem {
    ProtoDiscoveredItem {
        platform: platform_str(i.platform).to_string(),
        item_id: i.item_id.clone(),
        captured_at: Some(prost_types::Timestamp {
            seconds: i.captured_at.timestamp(),
            nanos: 0,
        }),
        author_handle: i.author_handle.clone(),
        author_name: i.author_name.clone().unwrap_or_default(),
        body: i.body.clone(),
        url: i.url.clone().unwrap_or_default(),
        metrics: Some(ProtoMetrics {
            read_count: i.metrics.read_count.unwrap_or(-1),
            like_count: i.metrics.like_count.unwrap_or(-1),
            comment_count: i.metrics.comment_count.unwrap_or(-1),
            share_count: i.metrics.share_count.unwrap_or(-1),
            view_count: i.metrics.view_count.unwrap_or(-1),
            bookmark_count: i.metrics.bookmark_count.unwrap_or(-1),
        }),
        metadata_json: serde_json::to_string(&i.metadata).unwrap_or_else(|_| "{}".to_string()),
    }
}
