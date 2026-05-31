//! multipost server binary.

mod auth;
mod services;
mod state;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use tonic::transport::Server;
use tracing::{Level, info};

use multipost_core::Platform;
use multipost_crawlers_toutiao::{ToutiaoCrawler, ToutiaoUserCrawler};
use multipost_crawlers_twitter::{TwitterCrawler, TwitterUserCrawler};
use multipost_proto::accounts::accounts_server::AccountsServer;
use multipost_proto::crawl::crawl_server::CrawlServer;
use multipost_proto::media::media_server::MediaServer;
use multipost_proto::posts::posts_server::PostsServer;
use multipost_proto::stats::stats_server::StatsServer;
use multipost_publishers_douyin::DouyinPublisher;
use multipost_publishers_toutiao::{ToutiaoPublisher, ToutiaoStatsCollector};
use multipost_publishers_twitter::{TwitterPublisher, TwitterStatsCollector};
use multipost_publishers_wx_gzh::WxGzhPublisher;
use multipost_publishers_youtube::{OAuthCredentials, YouTubePublisher};
use multipost_storage::accounts::FileBackedAccountRepository;
use multipost_storage::discovered::SqliteDiscoveredRepository;
use multipost_storage::jobs::FileBackedJobRepository;
use multipost_storage::media::FileBackedMediaRepository;
use multipost_storage::stats::SqliteStatsRepository;
use multipost_storage::tenants::FileBackedTenantRepository;

use crate::auth::AuthInterceptor;
use crate::services::accounts::AccountsService;
use crate::services::crawl::CrawlService;
use crate::services::media::MediaService;
use crate::services::posts::PostsService;
use crate::services::stats::StatsService;
use crate::state::{AppState, OAuthConfig};

#[derive(Parser, Debug)]
#[command(name = "multipost-server", version)]
struct Args {
    /// gRPC listen address.
    #[arg(long, env = "MULTIPOST_GRPC_ADDR", default_value = "0.0.0.0:8188")]
    grpc_addr: SocketAddr,

    /// HTTP listen address (OAuth callback + /healthz).
    #[arg(long, env = "MULTIPOST_HTTP_ADDR", default_value = "0.0.0.0:8189")]
    http_addr: SocketAddr,

    /// Root directory for persistent state (accounts, jobs, media metadata).
    #[arg(long, env = "MULTIPOST_DATA_DIR", default_value = "~/.multipost")]
    data_dir: String,
}

fn load_youtube_oauth() -> Option<OAuthCredentials> {
    let client_id = std::env::var("MULTIPOST_YOUTUBE_CLIENT_ID").ok()?;
    let client_secret = std::env::var("MULTIPOST_YOUTUBE_CLIENT_SECRET").ok()?;
    let redirect_uri = std::env::var("MULTIPOST_YOUTUBE_REDIRECT_URI")
        .unwrap_or_else(|_| "http://localhost:8189/oauth/callback/youtube".to_string());
    Some(OAuthCredentials {
        client_id,
        client_secret,
        redirect_uri,
    })
}

fn expand_path(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(p)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    let args = Args::parse();
    let data_dir = expand_path(&args.data_dir);
    std::fs::create_dir_all(&data_dir)?;
    info!(
        grpc = %args.grpc_addr,
        http = %args.http_addr,
        data = %data_dir.display(),
        "starting multipost-server"
    );

    let accounts = Arc::new(
        FileBackedAccountRepository::open(data_dir.join("accounts.json"))
            .context("open accounts.json")?,
    );
    let media_dir = data_dir.join("media");
    let media = Arc::new(
        FileBackedMediaRepository::open(data_dir.join("media.json"), &media_dir)
            .context("open media.json")?,
    );
    let jobs = Arc::new(
        FileBackedJobRepository::open(data_dir.join("jobs.json")).context("open jobs.json")?,
    );
    let tenants = Arc::new(
        FileBackedTenantRepository::open(data_dir.join("tenants.json"))
            .context("open tenants.json")?,
    );
    let dev_no_auth = std::env::var("MULTIPOST_DEV_NO_AUTH").is_ok_and(|v| v == "1" || v == "true");
    if dev_no_auth {
        tracing::warn!(
            "MULTIPOST_DEV_NO_AUTH=1 \u{2014} ALL requests will be bound to tenant_id=00000000-0000-0000-0000-000000000000 with no key check. Use this only for local development."
        );
    } else {
        let registered = tenants.list().map(|v| v.len()).unwrap_or(0);
        info!(
            tenants = registered,
            "auth interceptor enabled (set MULTIPOST_DEV_NO_AUTH=1 to disable for local dev)"
        );
    }

    let oauth = OAuthConfig {
        youtube: load_youtube_oauth(),
    };

    // Build publishers. Each one is created once per platform and reused
    // across all accounts on that platform.
    let mut publishers: HashMap<Platform, Arc<dyn multipost_core::Publisher>> = HashMap::new();
    if let Some(yt_oauth) = oauth.youtube.clone() {
        publishers.insert(
            Platform::YouTube,
            Arc::new(YouTubePublisher::new(reqwest::Client::new(), yt_oauth)),
        );
        info!("YouTube publisher registered");
    } else {
        info!(
            "YouTube OAuth NOT configured \
             (set MULTIPOST_YOUTUBE_CLIENT_ID + MULTIPOST_YOUTUBE_CLIENT_SECRET to enable)"
        );
    }
    // WxGzh has no global config — credentials come from each account.
    publishers.insert(
        Platform::WxGzh,
        Arc::new(WxGzhPublisher::new(reqwest::Client::new())),
    );
    info!("WxGzh publisher registered");

    // Douyin: also per-account (the credentials are just a CDP URL).
    publishers.insert(Platform::Douyin, Arc::new(DouyinPublisher::new()));
    info!("Douyin publisher registered");

    // Toutiao: same per-account CDP shape as Douyin.
    publishers.insert(Platform::Toutiao, Arc::new(ToutiaoPublisher::new()));
    info!("Toutiao publisher registered");

    // Twitter / X: per-account CDP + handle.
    publishers.insert(Platform::Twitter, Arc::new(TwitterPublisher::new()));
    info!("Twitter publisher registered");

    // Crawlers (read-only; drive pwright via subprocess).
    let mut crawlers: HashMap<Platform, Arc<dyn multipost_core::Crawler>> = HashMap::new();
    crawlers.insert(Platform::Toutiao, Arc::new(ToutiaoCrawler::new()));
    info!("Toutiao crawler registered");
    crawlers.insert(Platform::Twitter, Arc::new(TwitterCrawler::new()));
    info!("Twitter crawler registered");

    // Per-user crawlers (read-only; crawl one account's posts via pwright).
    let mut user_crawlers: HashMap<Platform, Arc<dyn multipost_core::UserCrawler>> = HashMap::new();
    user_crawlers.insert(Platform::Toutiao, Arc::new(ToutiaoUserCrawler::new()));
    user_crawlers.insert(Platform::Twitter, Arc::new(TwitterUserCrawler::new()));
    info!("Toutiao + Twitter user crawlers registered");

    let discovered = Arc::new(
        SqliteDiscoveredRepository::open(data_dir.join("discovered.sqlite"))
            .context("open discovered.sqlite")?,
    );

    // Stats collectors (read-only; drive the account's logged-in Chrome
    // over CDP). Toutiao + Twitter share the cookie-auth CDP shape.
    let mut stats_collectors: HashMap<Platform, Arc<dyn multipost_core::StatsCollector>> =
        HashMap::new();
    stats_collectors.insert(Platform::Toutiao, Arc::new(ToutiaoStatsCollector::new()));
    stats_collectors.insert(Platform::Twitter, Arc::new(TwitterStatsCollector::new()));
    info!("Toutiao + Twitter stats collectors registered");

    let stats = Arc::new(
        SqliteStatsRepository::open(data_dir.join("stats.sqlite")).context("open stats.sqlite")?,
    );

    let app_state = Arc::new(AppState::new(
        accounts,
        media.clone(),
        media_dir.clone(),
        jobs,
        publishers,
        oauth,
        crawlers,
        user_crawlers,
        discovered,
        stats_collectors,
        stats,
    ));

    let accounts_svc = AccountsService::new(app_state.clone());
    let media_svc = MediaService::new(app_state.clone());
    let posts_svc = PostsService::new(app_state.clone());
    let crawl_svc = CrawlService::new(app_state.clone());
    let stats_svc = StatsService::new(app_state.clone());

    // §22.7 startup recovery: re-attach confirm-poll tasks for any
    // Confirming jobs that survived a crash/restart in the last 24h.
    // Done before binding gRPC so the in-memory `confirm_tasks` map
    // reflects reality the moment we start accepting traffic.
    recover_confirming_jobs(&app_state).await;

    let interceptor = AuthInterceptor::new(tenants, dev_no_auth);

    // Spawn HTTP listener on a side task.
    let http_addr = args.http_addr;
    let http_task = tokio::spawn(async move {
        let app = axum::Router::new()
            .route("/healthz", axum::routing::get(healthz))
            .route(
                "/oauth/callback/:platform",
                axum::routing::get(oauth_callback),
            );
        let listener = tokio::net::TcpListener::bind(http_addr).await?;
        info!(addr = %http_addr, "HTTP listener bound");
        axum::serve(listener, app).await?;
        Ok::<(), anyhow::Error>(())
    });

    info!(addr = %args.grpc_addr, "gRPC listener bound");
    let shutdown = shutdown_signal();
    Server::builder()
        .add_service(AccountsServer::with_interceptor(
            accounts_svc,
            interceptor.clone(),
        ))
        .add_service(MediaServer::with_interceptor(
            media_svc,
            interceptor.clone(),
        ))
        .add_service(PostsServer::with_interceptor(
            posts_svc,
            interceptor.clone(),
        ))
        .add_service(CrawlServer::with_interceptor(
            crawl_svc,
            interceptor.clone(),
        ))
        .add_service(StatsServer::with_interceptor(stats_svc, interceptor))
        .serve_with_shutdown(args.grpc_addr, shutdown)
        .await
        .context("serve grpc")?;

    // gRPC has stopped accepting. Drain background confirm-poll tasks with
    // a 30s deadline — anything still in flight gets aborted and the
    // job stays in Confirming for the next startup's recovery scan.
    drain_confirm_tasks(&app_state, std::time::Duration::from_secs(30)).await;

    http_task.abort();
    Ok(())
}

/// Resolve when SIGINT (Ctrl-C) or, on Unix, SIGTERM is received.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("SIGINT received, draining"),
        _ = terminate => info!("SIGTERM received, draining"),
    }
}

/// §22.7 startup recovery scan. List Confirming jobs created in the
/// last 24 h, rebuild the `ConfirmHandoff` from each row + its account's
/// current credentials, and spawn a tracked `poll_confirm_until_terminal`
/// task. Best-effort — any account that disappeared, any platform whose
/// publisher isn't registered, etc. just gets skipped with a warning.
async fn recover_confirming_jobs(state: &std::sync::Arc<AppState>) {
    use multipost_core::PublishHandle;
    use multipost_orchestrator::JobState;
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
    let jobs = match state.jobs.find_in_state(JobState::Confirming, cutoff).await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "startup recovery: find_in_state failed");
            return;
        }
    };
    if jobs.is_empty() {
        info!("startup recovery: no Confirming jobs to resume");
        return;
    }
    info!(
        count = jobs.len(),
        "startup recovery: re-attaching confirm-poll tasks"
    );
    for job in jobs {
        let tenant_id = job.user_id;
        let job_id = job.id;
        let account_id = job.account_id;
        let external_id = match job.external_id.clone() {
            Some(x) => x,
            None => {
                tracing::warn!(%job_id, "startup recovery: job has no external_id; skipping");
                continue;
            }
        };
        let account = match state.accounts.get(tenant_id, account_id).await {
            Ok(Some(a)) => a,
            Ok(None) => {
                tracing::warn!(%job_id, %account_id, "startup recovery: account gone; skipping");
                continue;
            }
            Err(e) => {
                tracing::warn!(%job_id, error = %e, "startup recovery: load account failed");
                continue;
            }
        };
        if !state.publishers.contains_key(&account.platform) {
            tracing::warn!(%job_id, platform = ?account.platform, "startup recovery: publisher not registered; skipping");
            continue;
        }
        let handoff = crate::services::posts::ConfirmHandoff {
            credentials: account.credentials.clone(),
            handle: PublishHandle {
                external_id,
                permalink: job.permalink.clone(),
            },
            account_id,
        };
        tracing::info!(%job_id, %account_id, "startup recovery: spawn confirm-poll");
        let st = state.clone();
        state.spawn_confirm(
            job_id,
            crate::services::posts::poll_confirm_until_terminal(st, tenant_id, job_id, handoff),
        );
    }
}

/// Wait up to `deadline` for in-flight confirm-poll tasks to finish.
/// Any task still running when the deadline elapses is aborted.
async fn drain_confirm_tasks(state: &std::sync::Arc<AppState>, deadline: std::time::Duration) {
    let handles: Vec<tokio::task::JoinHandle<()>> = {
        let mut t = state
            .confirm_tasks
            .lock()
            .expect("confirm_tasks mutex poisoned");
        t.drain().map(|(_, h)| h).collect()
    };
    let n = handles.len();
    if n == 0 {
        info!("no confirm tasks in flight");
        return;
    }
    info!(
        in_flight = n,
        deadline_secs = deadline.as_secs(),
        "draining confirm tasks"
    );
    // Wait all, with a single global deadline.
    let drain = async {
        for h in handles {
            let _ = h.await;
        }
    };
    match tokio::time::timeout(deadline, drain).await {
        Ok(_) => info!("all confirm tasks drained"),
        Err(_) => {
            tracing::warn!(
                "drain deadline expired — remaining tasks aborted; their jobs stay in Confirming for next-boot recovery"
            );
            // tokio's abort happens when JoinHandle is dropped — at this
            // point the handles are owned by the spawn tasks themselves
            // (we already moved them out of `confirm_tasks`). Process exit
            // will reap them.
        }
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn oauth_callback(
    axum::extract::Path(platform): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> String {
    let code = params.get("code").map(|s| s.as_str()).unwrap_or("?");
    let state = params.get("state").map(|s| s.as_str()).unwrap_or("?");
    format!(
        "OAuth callback for {platform}\n\n\
         code: {code}\nstate: {state}\n\n\
         Paste the code into:\n\
         multipost accounts complete <pending_auth_id> <code>"
    )
}
