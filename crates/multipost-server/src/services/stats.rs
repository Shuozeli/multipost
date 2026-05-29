//! `Stats` gRPC service — drives a connected account's creator dashboard
//! to collect profile + per-post stats, persists timestamped snapshots,
//! and serves the history back.

use std::sync::Arc;

use tonic::{Request, Response, Status};
use uuid::Uuid;

use multipost_core::{AccountStats, Platform, PostStats, StatsOptions};
use multipost_proto::stats::stats_server::Stats as StatsTrait;
use multipost_proto::stats::{
    AccountStats as ProtoAccountStats, AccountStatsSeries, CollectStatsRequest,
    GetAccountStatsRequest, ListPostStatsRequest, PostStats as ProtoPostStats, PostStatsList,
    StatsSnapshot as ProtoStatsSnapshot,
};

use crate::auth::tenant_id_from_request;
use crate::state::AppState;

const DEFAULT_MAX_POSTS: usize = 100;
const MAX_POSTS_CAP: usize = 500;
const MAX_HISTORY: usize = 365;

pub struct StatsService {
    state: Arc<AppState>,
}

impl StatsService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    /// Load an account and confirm a stats collector exists for its platform.
    async fn load_account(
        &self,
        tenant_id: Uuid,
        account_id: Uuid,
    ) -> Result<multipost_storage::accounts::AccountRecord, Status> {
        self.state
            .accounts
            .get(tenant_id, account_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("no such account"))
    }
}

#[tonic::async_trait]
impl StatsTrait for StatsService {
    async fn collect(
        &self,
        req: Request<CollectStatsRequest>,
    ) -> Result<Response<ProtoStatsSnapshot>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let r = req.into_inner();
        let account_id: Uuid = r
            .account_id
            .parse()
            .map_err(|_| Status::invalid_argument("account_id not a UUID"))?;
        let account = self.load_account(tenant_id, account_id).await?;

        let collector = self
            .state
            .stats_collectors
            .get(&account.platform)
            .cloned()
            .ok_or_else(|| {
                Status::failed_precondition(format!(
                    "no stats collector for {:?}",
                    account.platform
                ))
            })?;

        // cdp_url + handle ride in the account's credentials JSON (the same
        // cookie-auth shape the publishers use).
        let cdp_url = account
            .credentials
            .get("cdp_url")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let handle = account
            .credentials
            .get("handle")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let max_posts = if r.max_posts == 0 {
            DEFAULT_MAX_POSTS
        } else {
            (r.max_posts as usize).min(MAX_POSTS_CAP)
        };

        let opts = StatsOptions {
            max_posts,
            cdp_url,
            handle,
            pwright_bin: "pwright".to_string(),
        };

        let snapshot = collector
            .collect(&opts)
            .await
            .map_err(|e| Status::internal(format!("collect: {e}")))?;

        // Persist the timestamped snapshot.
        self.state
            .stats
            .insert_account(account_id, &snapshot.account)
            .await
            .map_err(|e| Status::internal(format!("store account stats: {e}")))?;
        self.state
            .stats
            .insert_posts(account_id, &snapshot.posts)
            .await
            .map_err(|e| Status::internal(format!("store post stats: {e}")))?;

        tracing::info!(
            %account_id,
            platform = ?account.platform,
            posts = snapshot.posts.len(),
            "stats snapshot collected"
        );

        Ok(Response::new(ProtoStatsSnapshot {
            account: Some(account_to_proto(&snapshot.account)),
            posts: snapshot.posts.iter().map(post_to_proto).collect(),
        }))
    }

    async fn get_account_stats(
        &self,
        req: Request<GetAccountStatsRequest>,
    ) -> Result<Response<AccountStatsSeries>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let r = req.into_inner();
        let account_id: Uuid = r
            .account_id
            .parse()
            .map_err(|_| Status::invalid_argument("account_id not a UUID"))?;
        let account = self.load_account(tenant_id, account_id).await?;
        let limit = clamp_limit(r.limit, 30, MAX_HISTORY);

        let series = self
            .state
            .stats
            .account_history(account.platform, account_id, limit)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(AccountStatsSeries {
            snapshots: series.iter().map(account_to_proto).collect(),
        }))
    }

    async fn list_post_stats(
        &self,
        req: Request<ListPostStatsRequest>,
    ) -> Result<Response<PostStatsList>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let r = req.into_inner();
        let account_id: Uuid = r
            .account_id
            .parse()
            .map_err(|_| Status::invalid_argument("account_id not a UUID"))?;
        let account = self.load_account(tenant_id, account_id).await?;
        let limit = clamp_limit(r.limit, 50, MAX_POSTS_CAP);

        let posts = self
            .state
            .stats
            .latest_posts(account.platform, account_id, limit)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(PostStatsList {
            posts: posts.iter().map(post_to_proto).collect(),
        }))
    }
}

fn clamp_limit(requested: u32, default: usize, cap: usize) -> usize {
    if requested == 0 {
        default
    } else {
        (requested as usize).min(cap)
    }
}

/// `None` → -1 sentinel (proto3 int64 has no null; see stats.proto).
fn opt_i64(v: Option<i64>) -> i64 {
    v.unwrap_or(-1)
}

fn opt_f64(v: Option<f64>) -> f64 {
    v.unwrap_or(-1.0)
}

fn ts(dt: chrono::DateTime<chrono::Utc>) -> Option<prost_types::Timestamp> {
    Some(prost_types::Timestamp {
        seconds: dt.timestamp(),
        nanos: 0,
    })
}

fn platform_str(p: Platform) -> String {
    format!("{p:?}").to_lowercase()
}

fn account_to_proto(a: &AccountStats) -> ProtoAccountStats {
    ProtoAccountStats {
        platform: platform_str(a.platform),
        captured_at: ts(a.captured_at),
        followers: opt_i64(a.followers),
        following: opt_i64(a.following),
        post_count: opt_i64(a.post_count),
        total_views: opt_i64(a.total_views),
        total_income: opt_f64(a.total_income),
        yesterday_followers: opt_i64(a.yesterday_followers),
        yesterday_views: opt_i64(a.yesterday_views),
        yesterday_income: opt_f64(a.yesterday_income),
    }
}

fn post_to_proto(p: &PostStats) -> ProtoPostStats {
    ProtoPostStats {
        platform: platform_str(p.platform),
        post_id: p.post_id.clone(),
        captured_at: ts(p.captured_at),
        title: p.title.clone(),
        post_type: p.post_type.clone(),
        created_at: p.created_at.and_then(ts),
        impressions: opt_i64(p.impressions),
        reads: opt_i64(p.reads),
        likes: opt_i64(p.likes),
        comments: opt_i64(p.comments),
        shares: opt_i64(p.shares),
        bookmarks: opt_i64(p.bookmarks),
        plays: opt_i64(p.plays),
    }
}
