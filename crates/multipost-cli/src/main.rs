//! multipost CLI.

use std::path::PathBuf;

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
    complete_auth_request, start_auth_response, CompleteAuthRequest, ListAccountsRequest,
    RegisterDeveloperRequest, StartAuthRequest,
};
use multipost_proto::common::{Platform as ProtoPlatform, Visibility};
use multipost_proto::crawl::crawl_client::CrawlClient;
use multipost_proto::crawl::{
    CrawlJobState as ProtoCrawlJobState, GetCrawlJobRequest, ListItemsRequest, SubmitCrawlRequest,
};
use multipost_proto::media::media_client::MediaClient;
use multipost_proto::media::{upload_chunk, UploadChunk, UploadMeta};
use multipost_proto::posts::posts_client::PostsClient;
use multipost_proto::posts::{Content, GetJobRequest, JobRef, ListJobsRequest, SubmitRequest};
use multipost_storage::tenants::FileBackedTenantRepository;

const UPLOAD_CHUNK: usize = 1 * 1024 * 1024; // 1 MiB

#[derive(Parser, Debug)]
#[command(name = "multipost", version)]
struct Cli {
    /// Server URL.
    #[arg(long, env = "MULTIPOST_SERVER", default_value = "http://localhost:8188")]
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
        /// Platform to crawl: toutiao | twitter.
        #[arg(long)]
        platform: String,
        /// How long the crawler should listen + scroll, in seconds.
        /// Server clamps to [5, 300].
        #[arg(long, default_value = "30")]
        duration: u32,
    },
    /// List recently captured items for a platform from the server's
    /// SQLite store (across all crawl jobs).
    Discovered {
        /// Platform: toutiao | twitter.
        #[arg(long)]
        platform: String,
        /// Max items.
        #[arg(long, default_value = "30")]
        limit: u32,
    },
    /// Submit content for publishing.
    Post {
        /// One platform name per --to: youtube, wx-gzh, twitter, douyin.
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        to: Vec<String>,
        /// Path to a video file (required for video platforms).
        #[arg(long)]
        video: Option<PathBuf>,
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
        /// Optional cached display name.
        #[arg(long, default_value = "")]
        nickname: String,
        /// Optional cached Toutiao user ID (头条号 ID).
        #[arg(long, default_value = "")]
        toutiao_id: String,
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
}

fn parse_platform(s: &str) -> anyhow::Result<ProtoPlatform> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "youtube" | "yt" => ProtoPlatform::Youtube,
        "wx-gzh" | "wxgzh" | "wechat" => ProtoPlatform::WxGzh,
        "twitter" | "x" => ProtoPlatform::Twitter,
        "douyin" => ProtoPlatform::Douyin,
        "toutiao" => ProtoPlatform::Toutiao,
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
        Command::Crawl { platform, duration } => {
            handle_crawl(&cli.server, auth, platform, duration).await
        }
        Command::Discovered { platform, limit } => {
            handle_discovered(&cli.server, auth, platform, limit).await
        }
        Command::Post {
            to,
            video,
            title,
            description,
            tags,
            privacy,
        } => handle_post(&cli.server, auth, to, video, title, description, tags, privacy).await,
    }
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
        let header = api_key.and_then(|k| {
            MetadataValue::try_from(format!("Bearer {k}")).ok()
        });
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

async fn handle_crawl(
    server: &str,
    auth: AuthInterceptor,
    platform: String,
    duration: u32,
) -> anyhow::Result<()> {
    let mut client = build_crawl(server, auth).await?;
    let submitted = client
        .submit(SubmitCrawlRequest {
            platform: platform.clone(),
            duration_secs: duration,
        })
        .await?
        .into_inner();
    println!("submitted crawl job {} ({}, {}s)", submitted.id, submitted.platform, submitted.duration_secs);
    println!("waiting up to {}s for completion ...", duration + 30);

    // Long-poll until terminal.
    let final_job = client
        .get_job(GetCrawlJobRequest {
            id: submitted.id.clone(),
            wait_seconds: duration + 30,
        })
        .await?
        .into_inner();

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
        let m = it.metrics.clone().unwrap_or_default();
        let text: String = it.body.chars().take(60).collect::<String>().replace('\n', " ");
        let handle: String = it.author_handle.chars().take(16).collect();
        println!(
            "  [{:>3}] {:<16} read={:>6} like={:>5} cmt={:>4} sh={:>4} bm={:>4} v={:>7} | {}",
            i + 1,
            handle,
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
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
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
            println!("{:<38} {:<24} {}", "ID", "NAME", "KEY_HASHES");
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
                    "{:<38} {:<10} {:<24} {}",
                    "ID", "PLATFORM", "EXTERNAL_ID", "DISPLAY"
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
                    println!("  multipost accounts complete {} <code>", resp.pending_auth_id);
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
            nickname,
            toutiao_id,
        } => {
            let creds = serde_json::json!({
                "cdp_url": cdp_url,
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
    }
    Ok(())
}

async fn handle_post(
    server: &str,
    auth: AuthInterceptor,
    to: Vec<String>,
    video: Option<PathBuf>,
    title: String,
    description: String,
    tags: Vec<String>,
    privacy: String,
) -> anyhow::Result<()> {
    if to.is_empty() {
        anyhow::bail!("--to is required");
    }
    let target_platforms: Vec<ProtoPlatform> = to
        .iter()
        .map(|s| parse_platform(s))
        .collect::<anyhow::Result<_>>()?;
    let vis = parse_visibility(&privacy)?;

    // Look up accounts to find IDs matching the requested platforms.
    let mut accounts_client = build_accounts(server, auth.clone()).await?;
    let accounts = accounts_client
        .list(ListAccountsRequest::default())
        .await
        .context("Accounts.List")?
        .into_inner()
        .accounts;

    let mut account_ids: Vec<String> = Vec::new();
    for p in &target_platforms {
        let matching: Vec<&_> = accounts.iter().filter(|a| a.platform == *p as i32).collect();
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

    // Upload media if --video was provided.
    let mut media_ids: Vec<String> = Vec::new();
    if let Some(path) = video {
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("read {path:?}"))?;
        let mime = mime_for(&path).to_string();
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("upload.bin")
            .to_string();
        let total = bytes.len() as i64;
        println!(
            "Uploading {} bytes ({}) as {} ...",
            total,
            mime,
            filename
        );

        let mut media_client = build_media(server, auth.clone()).await?;
        let (tx, rx) = tokio::sync::mpsc::channel::<UploadChunk>(8);

        // Send first chunk (meta) + data chunks in a side task.
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
        media_ids.push(asset.id);
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
        let state =
            multipost_proto::common::JobState::try_from(job.state).unwrap_or_default();
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
        let state =
            multipost_proto::common::JobState::try_from(job.state).unwrap_or_default();
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

async fn handle_watch(
    server: &str,
    auth: AuthInterceptor,
    job_id: String,
) -> anyhow::Result<()> {
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
                let state = multipost_proto::common::JobState::try_from(ev.state)
                    .unwrap_or_default();
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

async fn handle_cancel(
    server: &str,
    auth: AuthInterceptor,
    job_id: String,
) -> anyhow::Result<()> {
    let mut client = build_posts(server, auth).await?;
    let resp = client
        .cancel(JobRef { id: job_id.clone() })
        .await
        .context("Posts.Cancel rpc")?
        .into_inner();
    let state =
        multipost_proto::common::JobState::try_from(resp.state).unwrap_or_default();
    println!("✓ cancelled");
    println!("  id:     {}", resp.id);
    println!("  state:  {state:?}");
    if !resp.external_id.is_empty() {
        println!("  ext_id: {}", resp.external_id);
    }
    Ok(())
}
