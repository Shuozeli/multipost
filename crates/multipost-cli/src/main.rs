//! multipost CLI.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio_stream::wrappers::ReceiverStream;
use tonic::codegen::InterceptedService;
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::transport::Channel;
use tonic::{Request, Status};
use tracing::Level;

use multipost_proto::accounts::accounts_client::AccountsClient;
use multipost_proto::accounts::{
    CompleteAuthRequest, ListAccountsRequest, RegisterDeveloperRequest, StartAuthRequest,
    complete_auth_request, start_auth_response,
};
use multipost_proto::common::{Platform as ProtoPlatform, Visibility};
use multipost_proto::crawl::crawl_client::CrawlClient;
use multipost_proto::crawl::{
    CrawlJobState as ProtoCrawlJobState, GetCrawlJobRequest, ListItemsRequest, SubmitCrawlRequest,
    SubmitUserCrawlRequest,
};
use multipost_proto::media::media_client::MediaClient;
use multipost_proto::media::{UploadChunk, UploadMeta, upload_chunk};
use multipost_proto::posts::posts_client::PostsClient;
use multipost_proto::posts::{Content, GetJobRequest, JobRef, ListJobsRequest, SubmitRequest};
use multipost_proto::stats::stats_client::StatsClient;
use multipost_proto::stats::{
    AccountStats as ProtoAccountStats, CollectStatsRequest, GetAccountStatsRequest,
    ListPostStatsRequest, PostStats as ProtoPostStats,
};
use multipost_storage::tenants::FileBackedTenantRepository;

const UPLOAD_CHUNK: usize = 1024 * 1024; // 1 MiB

#[derive(Parser, Debug)]
#[command(name = "multipost", version)]
struct Cli {
    /// Server URL.
    #[arg(
        long,
        env = "MULTIPOST_SERVER",
        default_value = "http://localhost:8188"
    )]
    server: String,

    /// API key for authenticating against the server. Sent as
    /// `Authorization: Bearer <api_key>` on every RPC. Required unless
    /// the server is running with `MULTIPOST_DEV_NO_AUTH=1`.
    #[arg(long, env = "MULTIPOST_API_KEY", hide_env_values = true)]
    api_key: Option<String>,

    /// Data dir used by the `tenants` subcommand (operates directly on
    /// `<data_dir>/tenants.json`, no gRPC). Must match the server's
    /// `--data-dir`. Other subcommands ignore this flag.
    #[arg(long, env = "MULTIPOST_DATA_DIR", default_value = "~/.multipost")]
    data_dir: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Tenant management — operates directly on `<data_dir>/tenants.json`.
    /// Does NOT talk to the running server, so it works against the file
    /// even when the server is down.
    Tenants {
        #[command(subcommand)]
        action: TenantsAction,
    },
    /// Account management.
    Accounts {
        #[command(subcommand)]
        action: AccountsAction,
    },
    /// List jobs submitted via this server.
    Jobs {
        /// Maximum number of jobs to list.
        #[arg(long, default_value = "20")]
        limit: u32,
    },
    /// Cancel a job — for Confirmed jobs this calls the publisher's
    /// delete() to remove the post from the platform.
    Cancel {
        /// Job ID to cancel.
        job_id: String,
    },
    /// Watch a job's state transitions until it reaches a terminal state
    /// (Confirmed | Failed | Cancelled).
    Watch {
        /// Job ID to subscribe to.
        job_id: String,
    },
    /// Fetch a single job. With `--wait <secs>`, long-polls the server:
    /// block up to N seconds (capped server-side at 60) waiting for the
    /// next state transition, then return.
    GetJob {
        /// Job ID.
        job_id: String,
        /// Long-poll timeout, in seconds. 0 = return immediately.
        #[arg(long, default_value = "0")]
        wait: i32,
    },
    /// Crawl a platform's recommendation feed and capture popular
    /// content + engagement metrics. Submits + long-polls. Results are
    /// also persisted to `~/.multipost/discovered.sqlite` on the server.
    Crawl {
        /// Platform to crawl: toutiao | twitter | youtube.
        #[arg(long)]
        platform: String,
        /// How long the crawler should listen + scroll, in seconds.
        /// Server clamps to [5, 300].
        #[arg(long, default_value = "30")]
        duration: u32,
        /// Source URL/page to crawl. Repeatable. Required for YouTube
        /// unless the server has MULTIPOST_YOUTUBE_CRAWL_URLS set.
        #[arg(long = "url")]
        urls: Vec<String>,
        /// Return captured items in the job response without writing
        /// them to the server's discovered.sqlite.
        #[arg(long)]
        skip_persist: bool,
    },
    /// Crawl the recent posts of one or more specific accounts (vs. the
    /// anonymous feed). Submits + long-polls one job per handle. Results
    /// are persisted to the server's `discovered.sqlite` like `crawl`.
    CrawlUser {
        /// Platform: toutiao | twitter.
        #[arg(long)]
        platform: String,
        /// Account to crawl. Repeatable for several accounts. For
        /// twitter this is the screen name (e.g. `Tesla`, no `@`); for
        /// toutiao the user token from the profile URL (`MS4wLj…`).
        #[arg(long = "handle", required = true)]
        handles: Vec<String>,
        /// Target number of recent posts per account. Server clamps to
        /// [1, 300].
        #[arg(long, default_value = "100")]
        max: u32,
    },
    /// List recently captured items for a platform from the server's
    /// SQLite store (across all crawl jobs).
    Discovered {
        /// Platform: toutiao | twitter | youtube.
        #[arg(long)]
        platform: String,
        /// Max items.
        #[arg(long, default_value = "30")]
        limit: u32,
    },
    /// Profile-stats collection for a connected account (followers,
    /// income, per-post impressions/reads/likes…). Richer than `crawl`,
    /// which only sees the public recommendation feed.
    Stats {
        #[command(subcommand)]
        action: StatsAction,
    },
    /// Submit content for publishing.
    Post {
        /// One platform name per --to: youtube, wx-gzh, twitter, douyin.
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        to: Vec<String>,
        /// Explicit account ID to publish to. Repeatable. Use this when
        /// multiple accounts exist on the same platform.
        #[arg(long = "account-id")]
        account_ids: Vec<String>,
        /// Path to a video file (required for video platforms).
        #[arg(long)]
        video: Option<PathBuf>,
        /// Path to a custom thumbnail / cover image for video platforms.
        /// Uploaded after --video and currently consumed by YouTube.
        #[arg(long)]
        thumbnail: Option<PathBuf>,
        /// Path to an image to attach. Repeatable:
        /// `--image a.png --image b.jpg`. Routes to the platform's
        /// image-post flow (Twitter tweet with photos, Toutiao 微头条
        /// with images). Mutually exclusive with --video.
        #[arg(long)]
        image: Vec<PathBuf>,
        /// Title. Required for long-form posts (YouTube videos, WeChat
        /// MP articles, Toutiao articles). Omit for short-form posts
        /// (Toutiao 微头条, future tweets) — server then routes the
        /// content to the platform's short-post editor.
        #[arg(long, default_value = "")]
        title: String,
        /// Description / body.
        #[arg(long, default_value = "")]
        description: String,
        /// Hashtags, comma-separated (no `#`).
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        /// Visibility: public, unlisted, private.
        #[arg(long, default_value = "private")]
        privacy: String,
        /// Shortcut for `--privacy public`.
        #[arg(long)]
        public: bool,
    },
    /// Verify whether a published platform URL is publicly available.
    Verify {
        #[command(subcommand)]
        action: VerifyAction,
    },
}

#[derive(Subcommand, Debug)]
enum VerifyAction {
    /// Verify a YouTube watch URL or video id without using the multipost server.
    Youtube {
        /// YouTube video id, e.g. NfbjHERIyRE.
        #[arg(long)]
        video_id: Option<String>,
        /// YouTube watch / youtu.be URL.
        #[arg(long)]
        url: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum StatsAction {
    /// Drive the account's dashboard, capture a fresh snapshot, store it,
    /// and print it. May take tens of seconds for many posts.
    Collect {
        /// Platform: toutiao | twitter.
        #[arg(long)]
        platform: String,
        /// Max recent posts to pull stats for.
        #[arg(long, default_value = "100")]
        max_posts: u32,
    },
    /// Show stored account-level snapshots over time (newest first).
    Account {
        /// Platform: toutiao | twitter.
        #[arg(long)]
        platform: String,
        /// How many snapshots to show.
        #[arg(long, default_value = "30")]
        limit: u32,
    },
    /// Show the latest stored per-post stats for the account.
    Posts {
        /// Platform: toutiao | twitter.
        #[arg(long)]
        platform: String,
        /// How many posts to show.
        #[arg(long, default_value = "50")]
        limit: u32,
    },
}

#[derive(Subcommand, Debug)]
enum TenantsAction {
    /// Create a new tenant. Prints the API key once — copy it now.
    Create {
        /// Human-readable name (only for `tenants list` output).
        #[arg(long)]
        name: String,
    },
    /// List all tenants and their key fingerprints.
    List,
    /// Add a new API key to an existing tenant (rotation). The old key
    /// keeps working until you `revoke-key` it.
    AddKey {
        /// Tenant ID.
        tenant_id: String,
    },
    /// Revoke a key by hash prefix. Refuses if it would leave the
    /// tenant with no keys — call `add-key` first in that case.
    RevokeKey {
        /// Tenant ID.
        tenant_id: String,
        /// Prefix of the key hash to revoke (typically the first 8-16 chars
        /// of the `sha256:...` printed by `list`).
        hash_prefix: String,
    },
}

#[derive(Subcommand, Debug)]
enum AccountsAction {
    /// List all accounts.
    List,
    /// Start OAuth flow for a platform. Prints the URL to visit.
    Login {
        /// One of: youtube, wx-gzh, twitter, douyin.
        platform: String,
    },
    /// Complete a started OAuth flow with the authorization code.
    Complete {
        /// The pending_auth_id returned by `login`.
        pending_id: String,
        /// The `?code=...` value from the OAuth callback URL.
        code: String,
    },
    /// Register a WeChat MP account by appid + app_secret.
    RegisterWechat {
        /// WeChat MP appid (looks like `wxXXXXXXXXXXXXXXXX`).
        #[arg(long)]
        appid: String,
        /// WeChat MP app_secret.
        #[arg(long)]
        secret: String,
    },
    /// Register a Douyin account by the CDP HTTP endpoint of the Chrome
    /// profile that's already logged into creator.douyin.com.
    RegisterDouyin {
        /// Chrome DevTools Protocol HTTP endpoint
        /// (e.g. `http://localhost:9333` via SSH tunnel).
        #[arg(long)]
        cdp_url: String,
        /// SSH host where the Chrome runs. Required when the Chrome lives
        /// on a different machine than multipost-server (upload uses SCP).
        /// Leave empty for a Chrome on the same host.
        #[arg(long, default_value = "")]
        ssh_host: String,
        /// SSH username on `--ssh-host`. Defaults to current user.
        #[arg(long, default_value = "")]
        ssh_user: String,
        /// SSH port on `--ssh-host`. Omit for 22.
        #[arg(long)]
        ssh_port: Option<u16>,
        /// Directory on the Chrome host for staged video uploads.
        #[arg(long, default_value = "/tmp/multipost-uploads")]
        remote_temp_dir: String,
        /// Optional cached display name.
        #[arg(long, default_value = "")]
        nickname: String,
        /// Optional cached Douyin user ID (抖音号).
        #[arg(long, default_value = "")]
        douyin_id: String,
    },
    /// Register a Toutiao account by the CDP endpoint of the Chrome
    /// profile that's already logged into mp.toutiao.com.
    RegisterToutiao {
        /// Chrome DevTools Protocol HTTP endpoint.
        #[arg(long)]
        cdp_url: String,
        /// SSH host where Chrome runs. Required for remote video upload
        /// staging; optional for article/微头条.
        #[arg(long, default_value = "")]
        ssh_host: String,
        /// SSH username on `--ssh-host`.
        #[arg(long, default_value = "")]
        ssh_user: String,
        /// Optional SSH password. If set, staging uses `sshpass`.
        #[arg(long, default_value = "")]
        ssh_password: String,
        /// SSH port on `--ssh-host`. Omit for 22.
        #[arg(long)]
        ssh_port: Option<u16>,
        /// Directory on the Chrome host for staged video uploads.
        #[arg(long, default_value = "C:/Users/cyuan/Videos/multipost-uploads")]
        remote_temp_dir: String,
        /// Optional cached display name.
        #[arg(long, default_value = "")]
        nickname: String,
        /// Optional cached Toutiao user ID (头条号 ID).
        #[arg(long, default_value = "")]
        toutiao_id: String,
    },
    /// Register a Bilibili account. Requires cookies extracted from a
    /// Chrome profile that's already logged into bilibili.com.
    RegisterBilibili {
        /// Chrome DevTools Protocol HTTP endpoint.
        #[arg(long)]
        cdp_url: String,
        /// `SESSDATA` cookie value.
        #[arg(long)]
        sessdata: String,
        /// `bili_jct` cookie value (CSRF token).
        #[arg(long)]
        bili_jct: String,
        /// `buvid3` cookie value.
        #[arg(long, default_value = "")]
        buvid3: String,
        /// `DedeUserID` cookie value.
        #[arg(long, default_value = "")]
        dedeuserid: String,
        /// Optional cached display name.
        #[arg(long, default_value = "")]
        nickname: String,
        /// Optional cached Bilibili user ID (mid).
        #[arg(long, default_value = "")]
        bilibili_uid: String,
    },
    /// Register a Twitter / X account by the CDP endpoint of a Chrome
    /// profile that's already logged into x.com.
    RegisterTwitter {
        /// Chrome DevTools Protocol HTTP endpoint.
        #[arg(long)]
        cdp_url: String,
        /// Twitter handle without the leading `@` (e.g. `multipost_dev`).
        /// Needed so delete() can navigate to /<handle> to find tweets.
        #[arg(long)]
        handle: String,
        /// Optional cached display name.
        #[arg(long, default_value = "")]
        display_name: String,
    },
    /// Register a YouTube Studio account by the CDP endpoint of a Chrome
    /// profile that's already logged into studio.youtube.com.
    RegisterYoutubeStudio {
        /// Chrome DevTools Protocol HTTP endpoint.
        #[arg(long)]
        cdp_url: String,
        /// SSH host where Chrome runs. Required when Chrome is remote so
        /// video files can be staged before DOM.setFileInputFiles.
        #[arg(long, default_value = "")]
        ssh_host: String,
        /// SSH username on `--ssh-host`.
        #[arg(long, default_value = "")]
        ssh_user: String,
        /// Optional SSH password. If set, staging uses `sshpass`.
        #[arg(long, default_value = "")]
        ssh_password: String,
        /// SSH port on `--ssh-host`. Omit for 22.
        #[arg(long)]
        ssh_port: Option<u16>,
        /// Directory on the Chrome host for staged uploads.
        #[arg(long, default_value = "C:/Users/cyuan/Videos/multipost-uploads")]
        remote_temp_dir: String,
        /// Optional cached channel display name.
        #[arg(long, default_value = "")]
        display_name: String,
        /// Optional cached channel handle, e.g. `@newfinnews`.
        #[arg(long, default_value = "")]
        handle: String,
    },
}

fn parse_platform(s: &str) -> anyhow::Result<ProtoPlatform> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "youtube" | "yt" => ProtoPlatform::Youtube,
        "wx-gzh" | "wxgzh" | "wechat" => ProtoPlatform::WxGzh,
        "twitter" | "x" => ProtoPlatform::Twitter,
        "douyin" => ProtoPlatform::Douyin,
        "toutiao" => ProtoPlatform::Toutiao,
        "bilibili" | "bili" => ProtoPlatform::Bilibili,
        other => anyhow::bail!("unknown platform {other:?}"),
    })
}

fn parse_visibility(s: &str) -> anyhow::Result<Visibility> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "public" => Visibility::Public,
        "unlisted" => Visibility::Unlisted,
        "private" => Visibility::Private,
        "followers" => Visibility::Followers,
        other => anyhow::bail!("unknown visibility {other:?}"),
    })
}

fn mime_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|s| s.to_str()) {
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        _ => "application/octet-stream",
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_max_level(Level::WARN).init();
    let cli = Cli::parse();
    let auth = AuthInterceptor::new(cli.api_key.clone());

    match cli.command {
        Command::Tenants { action } => handle_tenants(&cli.data_dir, action),
        Command::Accounts { action } => handle_accounts(&cli.server, auth, action).await,
        Command::Jobs { limit } => handle_jobs_list(&cli.server, auth, limit).await,
        Command::Cancel { job_id } => handle_cancel(&cli.server, auth, job_id).await,
        Command::Watch { job_id } => handle_watch(&cli.server, auth, job_id).await,
        Command::GetJob { job_id, wait } => handle_get_job(&cli.server, auth, job_id, wait).await,
        Command::Crawl {
            platform,
            duration,
            urls,
            skip_persist,
        } => handle_crawl(&cli.server, auth, platform, duration, urls, skip_persist).await,
        Command::CrawlUser {
            platform,
            handles,
            max,
        } => handle_crawl_user(&cli.server, auth, platform, handles, max).await,
        Command::Discovered { platform, limit } => {
            handle_discovered(&cli.server, auth, platform, limit).await
        }
        Command::Stats { action } => handle_stats(&cli.server, auth, action).await,
        Command::Verify { action } => handle_verify(action).await,
        Command::Post {
            to,
            account_ids,
            video,
            thumbnail,
            image,
            title,
            description,
            tags,
            privacy,
            public,
        } => {
            handle_post(
                &cli.server,
                auth,
                PostArgs {
                    to,
                    account_ids,
                    video,
                    thumbnail,
                    images: image,
                    title,
                    description,
                    tags,
                    privacy,
                    public,
                },
            )
            .await
        }
    }
}

async fn handle_verify(action: VerifyAction) -> anyhow::Result<()> {
    match action {
        VerifyAction::Youtube { video_id, url } => {
            let id = match (video_id, url) {
                (Some(id), None) => id,
                (None, Some(url)) => extract_youtube_video_id(&url)?,
                (Some(_), Some(_)) => anyhow::bail!("pass only one of --video-id or --url"),
                (None, None) => anyhow::bail!("pass --video-id or --url"),
            };
            let http = reqwest::Client::new();
            let result = multipost_publishers_youtube::verify_public_video(&http, &id)
                .await
                .map_err(|e| anyhow::anyhow!("youtube verify failed: {e}"))?;
            println!("video_id:    {}", result.video_id);
            println!("url:         {}", result.url);
            println!("public:      {}", result.is_publicly_available());
            println!("playable:    {}", result.playable);
            println!("is_private:  {}", fmt_opt_bool(result.is_private));
            if let Some(status) = &result.playability_status {
                println!("status:      {status}");
            }
            if let Some(title) = &result.title {
                println!("title:       {title}");
            }
            if let Some(owner) = &result.owner_channel_name {
                println!("channel:     {owner}");
            }
            if let Some(reason) = &result.reason {
                println!("reason:      {reason}");
            }
            if !result.is_publicly_available() {
                anyhow::bail!("YouTube video is not publicly playable");
            }
            Ok(())
        }
    }
}

fn fmt_opt_bool(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    }
}

fn extract_youtube_video_id(url: &str) -> anyhow::Result<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty YouTube URL");
    }
    if let Some((_, rest)) = trimmed.split_once("youtu.be/") {
        let id = take_youtube_id(rest);
        if !id.is_empty() {
            return Ok(id);
        }
    }
    if let Some((_, query)) = trimmed.split_once('?') {
        for part in query.split('&') {
            if let Some((key, value)) = part.split_once('=')
                && key == "v"
            {
                let id = take_youtube_id(value);
                if !id.is_empty() {
                    return Ok(id);
                }
            }
        }
    }
    if trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Ok(trimmed.to_string());
    }
    anyhow::bail!("could not extract YouTube video id from {url:?}");
}

fn take_youtube_id(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}

/// Outgoing auth interceptor — injects `Authorization: Bearer <api_key>`
/// into every request's metadata. Cloneable so we can attach it to each
/// client (Accounts/Media/Posts).
#[derive(Clone)]
struct AuthInterceptor {
    header: Option<MetadataValue<tonic::metadata::Ascii>>,
}

impl AuthInterceptor {
    fn new(api_key: Option<String>) -> Self {
        let header = api_key.and_then(|k| MetadataValue::try_from(format!("Bearer {k}")).ok());
        Self { header }
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if let Some(h) = &self.header {
            req.metadata_mut().insert("authorization", h.clone());
        }
        Ok(req)
    }
}

type AccountsCli = AccountsClient<InterceptedService<Channel, AuthInterceptor>>;
type MediaCli = MediaClient<InterceptedService<Channel, AuthInterceptor>>;
type PostsCli = PostsClient<InterceptedService<Channel, AuthInterceptor>>;
type CrawlCli = CrawlClient<InterceptedService<Channel, AuthInterceptor>>;

async fn build_accounts(server: &str, auth: AuthInterceptor) -> anyhow::Result<AccountsCli> {
    let channel = Channel::from_shared(server.to_string())?
        .connect()
        .await
        .with_context(|| format!("connecting to {server}"))?;
    Ok(AccountsClient::with_interceptor(channel, auth))
}

async fn build_media(server: &str, auth: AuthInterceptor) -> anyhow::Result<MediaCli> {
    let channel = Channel::from_shared(server.to_string())?
        .connect()
        .await
        .with_context(|| format!("connecting to {server}"))?;
    Ok(MediaClient::with_interceptor(channel, auth))
}

async fn build_posts(server: &str, auth: AuthInterceptor) -> anyhow::Result<PostsCli> {
    let channel = Channel::from_shared(server.to_string())?
        .connect()
        .await
        .with_context(|| format!("connecting to {server}"))?;
    Ok(PostsClient::with_interceptor(channel, auth))
}

async fn build_crawl(server: &str, auth: AuthInterceptor) -> anyhow::Result<CrawlCli> {
    let channel = Channel::from_shared(server.to_string())?
        .connect()
        .await
        .with_context(|| format!("connecting to {server}"))?;
    Ok(CrawlClient::with_interceptor(channel, auth))
}

type StatsCli = StatsClient<InterceptedService<Channel, AuthInterceptor>>;

async fn build_stats(server: &str, auth: AuthInterceptor) -> anyhow::Result<StatsCli> {
    let channel = Channel::from_shared(server.to_string())?
        .connect()
        .await
        .with_context(|| format!("connecting to {server}"))?;
    Ok(StatsClient::with_interceptor(channel, auth))
}

/// Resolve a platform name to the single connected account's ID. Errors if
/// there are zero or more than one accounts for that platform.
async fn resolve_account_id(
    server: &str,
    auth: AuthInterceptor,
    platform: &str,
) -> anyhow::Result<String> {
    let p = parse_platform(platform)?;
    let mut accounts_client = build_accounts(server, auth).await?;
    let accounts = accounts_client
        .list(ListAccountsRequest::default())
        .await
        .context("Accounts.List")?
        .into_inner()
        .accounts;
    let matching: Vec<&_> = accounts.iter().filter(|a| a.platform == p as i32).collect();
    match matching.as_slice() {
        [] => anyhow::bail!(
            "no connected account for platform {p:?}. Run `multipost accounts ...` first"
        ),
        [a] => Ok(a.id.clone()),
        many => anyhow::bail!(
            "multiple {p:?} accounts; pass an explicit account when the picker lands. Connected: {:?}",
            many.iter().map(|a| &a.id).collect::<Vec<_>>()
        ),
    }
}

async fn handle_crawl(
    server: &str,
    auth: AuthInterceptor,
    platform: String,
    duration: u32,
    urls: Vec<String>,
    skip_persist: bool,
) -> anyhow::Result<()> {
    let mut client = build_crawl(server, auth).await?;
    let source_url_count = urls.len().max(1) as u32;
    let wait_budget_secs = duration
        .saturating_mul(source_url_count)
        .saturating_add(30)
        .max(30);
    let submitted = client
        .submit(SubmitCrawlRequest {
            platform: platform.clone(),
            duration_secs: duration,
            source_urls: urls,
            skip_persist,
        })
        .await?
        .into_inner();
    println!(
        "submitted crawl job {} ({}, {}s)",
        submitted.id, submitted.platform, submitted.duration_secs
    );
    println!("waiting up to {}s for completion ...", wait_budget_secs);

    let deadline = Instant::now() + Duration::from_secs(wait_budget_secs as u64);
    let final_job = loop {
        let now = Instant::now();
        let wait_seconds = if now >= deadline {
            0
        } else {
            let remaining = deadline.saturating_duration_since(now).as_secs();
            remaining.clamp(1, 60) as u32
        };
        let job = client
            .get_job(GetCrawlJobRequest {
                id: submitted.id.clone(),
                wait_seconds,
            })
            .await?
            .into_inner();
        let state = ProtoCrawlJobState::try_from(job.state)
            .unwrap_or(ProtoCrawlJobState::CrawlStateUnspecified);
        if matches!(
            state,
            ProtoCrawlJobState::CrawlStateCompleted | ProtoCrawlJobState::CrawlStateFailed
        ) || Instant::now() >= deadline
        {
            break job;
        }
        println!("state: {:?}   items: {}", state, job.items_captured);
    };

    let state = ProtoCrawlJobState::try_from(final_job.state)
        .unwrap_or(ProtoCrawlJobState::CrawlStateUnspecified);
    println!("state: {:?}   items: {}", state, final_job.items_captured);
    if !final_job.last_error.is_empty() {
        println!("error: {}", final_job.last_error);
        return Ok(());
    }
    print_items(&final_job.items);
    Ok(())
}

async fn handle_crawl_user(
    server: &str,
    auth: AuthInterceptor,
    platform: String,
    handles: Vec<String>,
    max: u32,
) -> anyhow::Result<()> {
    let mut client = build_crawl(server, auth).await?;
    // Pace between accounts: hammering a platform with back-to-back
    // profile loads trips a soft rate-limit (timelines stop hydrating for
    // a stretch). A short gap between accounts keeps the batch under it.
    const PACE_SECS: u64 = 8;
    // One job per handle, sequentially (each drives the same browser).
    for (i, handle) in handles.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(PACE_SECS)).await;
        }
        let submitted = client
            .submit_user_crawl(SubmitUserCrawlRequest {
                platform: platform.clone(),
                handle: handle.clone(),
                max_posts: max,
            })
            .await?
            .into_inner();
        println!(
            "\n=== {} @{} — job {} (max {}) ===",
            submitted.platform, handle, submitted.id, max
        );

        // User crawls are bounded by post count; give the safety-stop
        // duration plus slack for the long-poll.
        let wait = submitted.duration_secs + 30;
        let final_job = client
            .get_job(GetCrawlJobRequest {
                id: submitted.id.clone(),
                wait_seconds: wait,
            })
            .await?
            .into_inner();

        let state = ProtoCrawlJobState::try_from(final_job.state)
            .unwrap_or(ProtoCrawlJobState::CrawlStateUnspecified);
        println!("state: {:?}   posts: {}", state, final_job.items_captured);
        if !final_job.last_error.is_empty() {
            println!("error: {}", final_job.last_error);
            continue;
        }
        print_items(&final_job.items);
    }
    Ok(())
}

async fn handle_discovered(
    server: &str,
    auth: AuthInterceptor,
    platform: String,
    limit: u32,
) -> anyhow::Result<()> {
    let mut client = build_crawl(server, auth).await?;
    let resp = client
        .list_items(ListItemsRequest { platform, limit })
        .await?
        .into_inner();
    print_items(&resp.items);
    Ok(())
}

fn print_items(items: &[multipost_proto::crawl::DiscoveredItem]) {
    if items.is_empty() {
        println!("(no items)");
        return;
    }
    for (i, it) in items.iter().enumerate() {
        let m = it.metrics.unwrap_or_default();
        let text: String = it
            .body
            .chars()
            .take(60)
            .collect::<String>()
            .replace('\n', " ");
        let handle: String = it.author_handle.chars().take(16).collect();
        let item_id: String = it.item_id.chars().take(20).collect();
        println!(
            "  [{:>3}] {:<20} {:<16} len={:<5} read={:>6} like={:>5} cmt={:>4} sh={:>4} bm={:>4} v={:>7} | {}",
            i + 1,
            item_id,
            handle,
            it.body.chars().count(),
            m.read_count,
            m.like_count,
            m.comment_count,
            m.share_count,
            m.bookmark_count,
            m.view_count,
            text
        );
    }
}

fn expand_path(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(p)
}

fn handle_tenants(data_dir: &str, action: TenantsAction) -> anyhow::Result<()> {
    let path = expand_path(data_dir).join("tenants.json");
    let repo = FileBackedTenantRepository::open(&path)
        .with_context(|| format!("open tenants.json at {}", path.display()))?;

    match action {
        TenantsAction::Create { name } => {
            let (rec, plaintext) = repo.create(name)?;
            println!("✓ tenant created");
            println!("  id:         {}", rec.id);
            println!("  name:       {}", rec.name);
            println!("  api_key:    {plaintext}");
            println!("\nCopy the api_key above — it will not be shown again.");
            println!("Use it as `MULTIPOST_API_KEY={plaintext}` for subsequent CLI calls.");
        }
        TenantsAction::List => {
            let tenants = repo.list()?;
            if tenants.is_empty() {
                println!("(no tenants — run `multipost tenants create --name <...>`)");
                return Ok(());
            }
            println!("{:<38} {:<24} KEY_HASHES", "ID", "NAME");
            for t in tenants {
                let hashes = t
                    .key_hashes
                    .iter()
                    .map(|h| {
                        // Show the first 14 chars after "sha256:" for ID purposes.
                        h.strip_prefix("sha256:")
                            .map(|s| format!("sha256:{}…", &s[..s.len().min(14)]))
                            .unwrap_or_else(|| h.clone())
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("{:<38} {:<24} {}", t.id, t.name, hashes);
            }
        }
        TenantsAction::AddKey { tenant_id } => {
            let id = tenant_id.parse().context("tenant_id is not a UUID")?;
            let plaintext = repo.add_key(id)?;
            println!("✓ new key added to tenant {id}");
            println!("  api_key:    {plaintext}");
            println!("\nOld key(s) remain valid. After downstream callers switch,");
            println!("revoke the old one with `multipost tenants revoke-key {id} <hash-prefix>`.");
        }
        TenantsAction::RevokeKey {
            tenant_id,
            hash_prefix,
        } => {
            let id = tenant_id.parse().context("tenant_id is not a UUID")?;
            repo.revoke_key(id, &hash_prefix)?;
            println!("✓ revoked key(s) matching prefix {hash_prefix:?} on tenant {id}");
        }
    }
    Ok(())
}

async fn handle_accounts(
    server: &str,
    auth: AuthInterceptor,
    action: AccountsAction,
) -> anyhow::Result<()> {
    let mut client = build_accounts(server, auth).await?;

    match action {
        AccountsAction::List => {
            let resp = client
                .list(ListAccountsRequest::default())
                .await
                .context("Accounts.List rpc")?;
            let accounts = resp.into_inner().accounts;
            if accounts.is_empty() {
                println!("(no accounts)");
            } else {
                println!(
                    "{:<38} {:<10} {:<24} DISPLAY",
                    "ID", "PLATFORM", "EXTERNAL_ID"
                );
                for a in accounts {
                    let plat =
                        ProtoPlatform::try_from(a.platform).unwrap_or(ProtoPlatform::Unspecified);
                    println!(
                        "{:<38} {:<10?} {:<24} {}",
                        a.id, plat, a.external_id, a.display_name
                    );
                }
            }
        }
        AccountsAction::Login { platform } => {
            let plat = parse_platform(&platform)?;
            let resp = client
                .start_auth(StartAuthRequest {
                    platform: plat as i32,
                })
                .await
                .context("Accounts.StartAuth rpc")?
                .into_inner();
            match resp.method {
                Some(start_auth_response::Method::AuthUrl(url)) => {
                    println!("Open this URL in a browser logged in as the target account:\n");
                    println!("  {url}\n");
                    println!("After approving, the browser will redirect to a localhost URL");
                    println!("containing `?code=...&state=...`. Run:\n");
                    println!(
                        "  multipost accounts complete {} <code>",
                        resp.pending_auth_id
                    );
                }
                Some(start_auth_response::Method::BrowserSession(s)) => {
                    println!("Browser-automation auth session: {}", s.session_id);
                    println!("  multipost accounts complete {} ''", resp.pending_auth_id);
                }
                None => anyhow::bail!("server did not return an auth method"),
            }
        }
        AccountsAction::Complete { pending_id, code } => {
            let resp = client
                .complete_auth(CompleteAuthRequest {
                    pending_auth_id: pending_id,
                    completion: Some(complete_auth_request::Completion::Code(code)),
                })
                .await
                .context("Accounts.CompleteAuth rpc")?
                .into_inner();
            println!("✓ Account connected");
            println!("  id:           {}", resp.id);
            println!(
                "  platform:     {:?}",
                ProtoPlatform::try_from(resp.platform).ok()
            );
            println!("  display_name: {}", resp.display_name);
            println!("  external_id:  {}", resp.external_id);
        }
        AccountsAction::RegisterWechat { appid, secret } => {
            let creds = serde_json::json!({
                "appid": appid,
                "app_secret": secret,
            })
            .to_string();
            let resp = client
                .register_developer_credentials(RegisterDeveloperRequest {
                    platform: ProtoPlatform::WxGzh as i32,
                    credentials_json: creds,
                })
                .await
                .context("Accounts.RegisterDeveloperCredentials rpc")?
                .into_inner();
            println!("✓ WeChat MP account registered");
            println!("  id:           {}", resp.id);
            println!("  appid:        {}", resp.external_id);
            println!("  display_name: {}", resp.display_name);
        }
        AccountsAction::RegisterDouyin {
            cdp_url,
            ssh_host,
            ssh_user,
            ssh_port,
            remote_temp_dir,
            nickname,
            douyin_id,
        } => {
            let creds = serde_json::json!({
                "cdp_url": cdp_url,
                "ssh_host": ssh_host,
                "ssh_user": ssh_user,
                "ssh_port": ssh_port,
                "remote_temp_dir": remote_temp_dir,
                "nickname": nickname,
                "douyin_id": douyin_id,
            })
            .to_string();
            let resp = client
                .register_developer_credentials(RegisterDeveloperRequest {
                    platform: ProtoPlatform::Douyin as i32,
                    credentials_json: creds,
                })
                .await
                .context("Accounts.RegisterDeveloperCredentials rpc")?
                .into_inner();
            println!("✓ Douyin account registered");
            println!("  id:           {}", resp.id);
            println!("  display_name: {}", resp.display_name);
            if !resp.external_id.is_empty() {
                println!("  douyin_id:    {}", resp.external_id);
            }
        }
        AccountsAction::RegisterToutiao {
            cdp_url,
            ssh_host,
            ssh_user,
            ssh_password,
            ssh_port,
            remote_temp_dir,
            nickname,
            toutiao_id,
        } => {
            let creds = serde_json::json!({
                "cdp_url": cdp_url,
                "ssh_host": ssh_host,
                "ssh_user": ssh_user,
                "ssh_password": ssh_password,
                "ssh_port": ssh_port,
                "remote_temp_dir": remote_temp_dir,
                "nickname": nickname,
                "toutiao_id": toutiao_id,
            })
            .to_string();
            let resp = client
                .register_developer_credentials(RegisterDeveloperRequest {
                    platform: ProtoPlatform::Toutiao as i32,
                    credentials_json: creds,
                })
                .await
                .context("Accounts.RegisterDeveloperCredentials rpc")?
                .into_inner();
            println!("✓ Toutiao account registered");
            println!("  id:           {}", resp.id);
            println!("  display_name: {}", resp.display_name);
            if !resp.external_id.is_empty() {
                println!("  toutiao_id:   {}", resp.external_id);
            }
        }
        AccountsAction::RegisterBilibili {
            cdp_url,
            sessdata,
            bili_jct,
            buvid3,
            dedeuserid,
            nickname,
            bilibili_uid,
        } => {
            let creds = serde_json::json!({
                "cdp_url": cdp_url,
                "sessdata": sessdata,
                "bili_jct": bili_jct,
                "buvid3": buvid3,
                "dedeuserid": dedeuserid,
                "nickname": nickname,
                "bilibili_uid": bilibili_uid,
            })
            .to_string();
            let resp = client
                .register_developer_credentials(RegisterDeveloperRequest {
                    platform: ProtoPlatform::Bilibili as i32,
                    credentials_json: creds,
                })
                .await
                .context("Accounts.RegisterDeveloperCredentials rpc")?
                .into_inner();
            println!("✓ Bilibili account registered");
            println!("  id:           {}", resp.id);
            println!("  display_name: {}", resp.display_name);
            if !resp.external_id.is_empty() {
                println!("  bilibili_uid: {}", resp.external_id);
            }
        }
        AccountsAction::RegisterTwitter {
            cdp_url,
            handle,
            display_name,
        } => {
            let creds = serde_json::json!({
                "cdp_url": cdp_url,
                "handle": handle,
                "display_name": display_name,
            })
            .to_string();
            let resp = client
                .register_developer_credentials(RegisterDeveloperRequest {
                    platform: ProtoPlatform::Twitter as i32,
                    credentials_json: creds,
                })
                .await
                .context("Accounts.RegisterDeveloperCredentials rpc")?
                .into_inner();
            println!("✓ Twitter account registered");
            println!("  id:           {}", resp.id);
            println!("  display_name: {}", resp.display_name);
            if !resp.external_id.is_empty() {
                println!("  handle:       @{}", resp.external_id);
            }
        }
        AccountsAction::RegisterYoutubeStudio {
            cdp_url,
            ssh_host,
            ssh_user,
            ssh_password,
            ssh_port,
            remote_temp_dir,
            display_name,
            handle,
        } => {
            let creds = serde_json::json!({
                "kind": "studio_cdp",
                "cdp_url": cdp_url,
                "ssh_host": ssh_host,
                "ssh_user": ssh_user,
                "ssh_password": ssh_password,
                "ssh_port": ssh_port,
                "remote_temp_dir": remote_temp_dir,
                "display_name": display_name,
                "handle": handle,
            })
            .to_string();
            let resp = client
                .register_developer_credentials(RegisterDeveloperRequest {
                    platform: ProtoPlatform::Youtube as i32,
                    credentials_json: creds,
                })
                .await
                .context("Accounts.RegisterDeveloperCredentials rpc")?
                .into_inner();
            println!("✓ YouTube Studio account registered");
            println!("  id:           {}", resp.id);
            println!("  display_name: {}", resp.display_name);
            if !resp.external_id.is_empty() {
                println!("  external_id:  {}", resp.external_id);
            }
        }
    }
    Ok(())
}

/// Upload one media file via streaming `Media.Upload` and return its
/// server-assigned media_id. Shared by the `--video` and `--image` paths.
async fn upload_media(
    server: &str,
    auth: AuthInterceptor,
    path: &std::path::Path,
) -> anyhow::Result<String> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read {path:?}"))?;
    let mime = mime_for(path).to_string();
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload.bin")
        .to_string();
    let total = bytes.len() as i64;
    println!("Uploading {total} bytes ({mime}) as {filename} ...");

    let mut media_client = build_media(server, auth).await?;
    let (tx, rx) = tokio::sync::mpsc::channel::<UploadChunk>(8);

    // Send the meta chunk + data chunks from a side task.
    let send_task = tokio::spawn(async move {
        tx.send(UploadChunk {
            payload: Some(upload_chunk::Payload::Meta(UploadMeta {
                filename,
                mime_type: mime,
                total_bytes: total,
            })),
        })
        .await
        .map_err(|e| anyhow::anyhow!("send meta: {e}"))?;
        for chunk in bytes.chunks(UPLOAD_CHUNK) {
            tx.send(UploadChunk {
                payload: Some(upload_chunk::Payload::Data(chunk.to_vec())),
            })
            .await
            .map_err(|e| anyhow::anyhow!("send data: {e}"))?;
        }
        Ok::<(), anyhow::Error>(())
    });

    let resp = media_client
        .upload(ReceiverStream::new(rx))
        .await
        .context("Media.Upload rpc")?;
    send_task.await??;
    let asset = resp.into_inner();
    println!("  ✓ media_id: {}", asset.id);
    Ok(asset.id)
}

/// Fields for the `post` subcommand, bundled so the handler stays under
/// clippy's argument-count limit.
struct PostArgs {
    to: Vec<String>,
    account_ids: Vec<String>,
    video: Option<PathBuf>,
    thumbnail: Option<PathBuf>,
    images: Vec<PathBuf>,
    title: String,
    description: String,
    tags: Vec<String>,
    privacy: String,
    public: bool,
}

async fn handle_post(server: &str, auth: AuthInterceptor, args: PostArgs) -> anyhow::Result<()> {
    let PostArgs {
        to,
        account_ids: explicit_account_ids,
        video,
        thumbnail,
        images,
        title,
        description,
        tags,
        privacy,
        public,
    } = args;
    if to.is_empty() && explicit_account_ids.is_empty() {
        anyhow::bail!("at least one of --to or --account-id is required");
    }
    if video.is_some() && !images.is_empty() {
        anyhow::bail!("--video and --image are mutually exclusive");
    }
    if thumbnail.is_some() && video.is_none() {
        anyhow::bail!("--thumbnail requires --video");
    }
    let target_platforms: Vec<ProtoPlatform> = to
        .iter()
        .map(|s| parse_platform(s))
        .collect::<anyhow::Result<_>>()?;
    let effective_privacy = if public { "public" } else { privacy.as_str() };
    let vis = parse_visibility(effective_privacy)?;

    // Look up accounts to find IDs matching the requested platforms.
    let mut accounts_client = build_accounts(server, auth.clone()).await?;
    let accounts = accounts_client
        .list(ListAccountsRequest::default())
        .await
        .context("Accounts.List")?
        .into_inner()
        .accounts;

    let mut account_ids: Vec<String> = explicit_account_ids;
    for p in &target_platforms {
        let matching: Vec<&_> = accounts
            .iter()
            .filter(|a| a.platform == *p as i32)
            .collect();
        if matching.is_empty() {
            anyhow::bail!(
                "no connected account for platform {p:?}. \
                 Run `multipost accounts login {}` first",
                format!("{p:?}").to_lowercase()
            );
        }
        if matching.len() > 1 {
            anyhow::bail!(
                "multiple {p:?} accounts; account-picker UI lands in Phase 6. \
                 Connected: {:?}",
                matching.iter().map(|a| &a.id).collect::<Vec<_>>()
            );
        }
        account_ids.push(matching[0].id.clone());
    }

    // Upload media. For video posts, the video stays first and an optional
    // thumbnail follows it so publishers can preserve the existing media order
    // contract without a proto change. Image posts upload in listed order.
    let mut media_ids: Vec<String> = Vec::new();
    if let Some(path) = &video {
        media_ids.push(upload_media(server, auth.clone(), path).await?);
    }
    if let Some(path) = &thumbnail {
        media_ids.push(upload_media(server, auth.clone(), path).await?);
    }
    for path in &images {
        media_ids.push(upload_media(server, auth.clone(), path).await?);
    }

    // Compose the Content body — title + description join into one block,
    // YouTube publisher splits on the first newline.
    // Compose the Content body. Three caller intents:
    //   - title + description   → "TITLE\n\nBODY" (long-form: article/video)
    //   - title only            → just the title (degenerate but legal)
    //   - description only      → just the body (short-form: 微头条 / tweet)
    // Server-side `infer_content_kind` looks at the resulting shape to pick
    // the platform's article vs short-post editor.
    let body = match (title.as_str(), description.as_str()) {
        ("", "") => {
            anyhow::bail!("at least one of --title or --description is required");
        }
        ("", desc) => desc.to_string(),
        (t, "") => t.to_string(),
        (t, desc) => format!("{t}\n\n{desc}"),
    };
    let content = Content {
        text: body,
        hashtags: tags,
        media_ids: media_ids.clone(),
        visibility: vis as i32,
        schedule_at: None,
    };

    println!(
        "Submitting to {} account(s): {}",
        account_ids.len(),
        account_ids.join(", ")
    );
    let mut posts_client = build_posts(server, auth.clone()).await?;
    let resp = posts_client
        .submit(SubmitRequest {
            content: Some(content),
            account_ids,
        })
        .await
        .context("Posts.Submit rpc")?;
    let jobs = resp.into_inner().jobs;

    for job in jobs {
        let state = multipost_proto::common::JobState::try_from(job.state).unwrap_or_default();
        println!("\nJob {}", job.id);
        println!("  account_id:  {}", job.account_id);
        println!("  state:       {state:?}");
        if !job.external_id.is_empty() {
            println!("  external_id: {}", job.external_id);
        }
        if !job.permalink.is_empty() {
            println!("  permalink:   {}", job.permalink);
        }
        if !job.last_error.is_empty() {
            println!("  last_error:  {}", job.last_error);
        }
    }
    Ok(())
}

fn fmt_i(v: i64) -> String {
    if v < 0 {
        "—".to_string()
    } else {
        v.to_string()
    }
}

fn fmt_f(v: f64) -> String {
    if v < 0.0 {
        "—".to_string()
    } else {
        format!("{v:.2}")
    }
}

fn fmt_ts(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| secs.to_string())
}

fn print_account(a: Option<&ProtoAccountStats>) {
    let Some(a) = a else {
        println!("(no account stats)");
        return;
    };
    let when = a
        .captured_at
        .as_ref()
        .map(|t| fmt_ts(t.seconds))
        .unwrap_or_default();
    println!("\nAccount stats [{}] @ {}", a.platform, when);
    println!("  followers:          {}", fmt_i(a.followers));
    println!("  following:          {}", fmt_i(a.following));
    println!("  posts:              {}", fmt_i(a.post_count));
    println!("  total reads/plays:  {}", fmt_i(a.total_views));
    println!("  total income:       {}", fmt_f(a.total_income));
    println!("  yesterday fans:     {}", fmt_i(a.yesterday_followers));
    println!("  yesterday reads:    {}", fmt_i(a.yesterday_views));
    println!("  yesterday income:   {}", fmt_f(a.yesterday_income));
}

fn print_posts(posts: &[ProtoPostStats]) {
    println!("\nPosts ({}):", posts.len());
    println!(
        "  {:<6} {:<8} {:>8} {:>7} {:>6} {:>5} {:>6} {:>5}  title",
        "type", "id", "impr", "reads", "likes", "cmt", "shares", "bm"
    );
    for p in posts {
        let id_tail: String = p
            .post_id
            .chars()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let title: String = p.title.chars().take(30).collect();
        println!(
            "  {:<6} …{:<7} {:>8} {:>7} {:>6} {:>5} {:>6} {:>5}  {}",
            p.post_type,
            id_tail,
            fmt_i(p.impressions),
            fmt_i(p.reads),
            fmt_i(p.likes),
            fmt_i(p.comments),
            fmt_i(p.shares),
            fmt_i(p.bookmarks),
            title,
        );
    }
}

async fn handle_stats(
    server: &str,
    auth: AuthInterceptor,
    action: StatsAction,
) -> anyhow::Result<()> {
    match action {
        StatsAction::Collect {
            platform,
            max_posts,
        } => {
            let account_id = resolve_account_id(server, auth.clone(), &platform).await?;
            println!(
                "Collecting {platform} stats (up to {max_posts} posts) — this drives the browser, may take a bit…"
            );
            let mut client = build_stats(server, auth).await?;
            let snap = client
                .collect(CollectStatsRequest {
                    account_id,
                    max_posts,
                })
                .await
                .context("Stats.Collect rpc")?
                .into_inner();
            print_account(snap.account.as_ref());
            print_posts(&snap.posts);
        }
        StatsAction::Account { platform, limit } => {
            let account_id = resolve_account_id(server, auth.clone(), &platform).await?;
            let mut client = build_stats(server, auth).await?;
            let series = client
                .get_account_stats(GetAccountStatsRequest { account_id, limit })
                .await
                .context("Stats.GetAccountStats rpc")?
                .into_inner()
                .snapshots;
            if series.is_empty() {
                println!("(no stored account snapshots — run `stats collect` first)");
                return Ok(());
            }
            for a in &series {
                print_account(Some(a));
            }
        }
        StatsAction::Posts { platform, limit } => {
            let account_id = resolve_account_id(server, auth.clone(), &platform).await?;
            let mut client = build_stats(server, auth).await?;
            let posts = client
                .list_post_stats(ListPostStatsRequest { account_id, limit })
                .await
                .context("Stats.ListPostStats rpc")?
                .into_inner()
                .posts;
            if posts.is_empty() {
                println!("(no stored post stats — run `stats collect` first)");
                return Ok(());
            }
            print_posts(&posts);
        }
    }
    Ok(())
}

async fn handle_jobs_list(server: &str, auth: AuthInterceptor, limit: u32) -> anyhow::Result<()> {
    let mut client = build_posts(server, auth).await?;
    let resp = client
        .list_jobs(ListJobsRequest {
            state: 0,
            page_size: limit as i32,
            page_token: String::new(),
        })
        .await
        .context("Posts.ListJobs rpc")?;
    let jobs = resp.into_inner().jobs;
    if jobs.is_empty() {
        println!("(no jobs)");
        return Ok(());
    }
    for job in jobs {
        let state = multipost_proto::common::JobState::try_from(job.state).unwrap_or_default();
        println!(
            "{}  {state:?}  account={}  external_id={}",
            job.id, job.account_id, job.external_id
        );
    }
    Ok(())
}

async fn handle_get_job(
    server: &str,
    auth: AuthInterceptor,
    job_id: String,
    wait: i32,
) -> anyhow::Result<()> {
    let mut client = build_posts(server, auth).await?;
    let started = std::time::Instant::now();
    let resp = client
        .get_job(GetJobRequest {
            id: job_id.clone(),
            wait_seconds: wait,
        })
        .await
        .context("Posts.GetJob rpc")?
        .into_inner();
    let elapsed = started.elapsed().as_secs_f32();
    let state = multipost_proto::common::JobState::try_from(resp.state).unwrap_or_default();
    println!("(returned after {elapsed:.1}s)");
    println!("  id:          {}", resp.id);
    println!("  state:       {state:?}");
    println!("  attempts:    {}", resp.attempts);
    if !resp.external_id.is_empty() {
        println!("  external_id: {}", resp.external_id);
    }
    if !resp.permalink.is_empty() {
        println!("  permalink:   {}", resp.permalink);
    }
    if !resp.last_error.is_empty() {
        println!("  last_error:  {}", resp.last_error);
    }
    Ok(())
}

async fn handle_watch(server: &str, auth: AuthInterceptor, job_id: String) -> anyhow::Result<()> {
    use tokio_stream::StreamExt;
    let mut client = build_posts(server, auth).await?;
    let mut stream = client
        .watch(JobRef { id: job_id.clone() })
        .await
        .context("Posts.Watch rpc")?
        .into_inner();
    println!("watching {job_id} (Ctrl-C to stop)");
    while let Some(ev) = stream.next().await {
        match ev {
            Ok(ev) => {
                let state =
                    multipost_proto::common::JobState::try_from(ev.state).unwrap_or_default();
                let at = ev
                    .at
                    .as_ref()
                    .map(|t| {
                        chrono::DateTime::<chrono::Utc>::from_timestamp(t.seconds, t.nanos as u32)
                            .map(|d| d.format("%H:%M:%S").to_string())
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                println!("[{at}] {state:?}  {}", ev.detail);
            }
            Err(status) => {
                anyhow::bail!("stream error: {status}");
            }
        }
    }
    println!("(stream closed — job reached terminal state)");
    Ok(())
}

async fn handle_cancel(server: &str, auth: AuthInterceptor, job_id: String) -> anyhow::Result<()> {
    let mut client = build_posts(server, auth).await?;
    let resp = client
        .cancel(JobRef { id: job_id.clone() })
        .await
        .context("Posts.Cancel rpc")?
        .into_inner();
    let state = multipost_proto::common::JobState::try_from(resp.state).unwrap_or_default();
    println!("✓ cancelled");
    println!("  id:     {}", resp.id);
    println!("  state:  {state:?}");
    if !resp.external_id.is_empty() {
        println!("  ext_id: {}", resp.external_id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_youtube_id_from_watch_url() {
        let got = extract_youtube_video_id("https://www.youtube.com/watch?v=NfbjHERIyRE&t=1")
            .expect("watch url should parse");
        assert_eq!(got, "NfbjHERIyRE");
    }

    #[test]
    fn extracts_youtube_id_from_short_url() {
        let got = extract_youtube_video_id("https://youtu.be/NfbjHERIyRE?si=abc")
            .expect("short url should parse");
        assert_eq!(got, "NfbjHERIyRE");
    }

    #[test]
    fn accepts_bare_youtube_id() {
        let got = extract_youtube_video_id("NfbjHERIyRE").expect("bare id should parse");
        assert_eq!(got, "NfbjHERIyRE");
    }
}
