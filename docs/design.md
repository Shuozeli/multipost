# multipost — Design Doc

**Status**: Draft v0.2 (2026-05-15)
**Scope owner**: TBD

---

## 1. Goal

A pure-Rust service for **posting content to social media platforms automatically**. Use official APIs where the platform exposes a publishing endpoint; fall back to browser automation via [pwright](https://github.com/Shuozeli/pwright) where it does not.

**One sentence:** "Submit a piece of content once → it lands on every platform you've connected."

Inspired by [AiToEarn](https://github.com/yikart/AiToEarn) but **stripped of everything except posting automation**.

## 2. Non-goals

We explicitly do NOT build:

- ❌ AI content generation (bring your own text / images / video)
- ❌ Monetization features, payment, content marketplace
- ❌ Engagement (reading comments, auto-replies, DMs)
- ❌ Social graph / following / unfollowing
- ❌ Deep analytics (basic per-post `views/likes` if free; no dashboards)
- ❌ Mobile/Electron desktop app (web portal is sufficient)
- ❌ Account farming, fake engagement, anti-detection arms race

Anything beyond "you give us content, we put it on the platform" is out of scope.

## 3. Supported platforms

**Scope: 4 platforms** (narrowed 2026-05-15 from the original 15). The remaining 11 from AiToEarn's set are deferred — design still accommodates them, but not in the initial build.

### Official-API platforms (2)

| Platform | API base | Auth | Notes |
|---|---|---|---|
| YouTube | `googleapis.com/youtube/v3` | OAuth 2.0 | Resumable upload. Validated 2026-05-15 via `scripts/youtube/` — full publish → privacy update → delete round trip. |
| WeChat MP (公众号) | `api.weixin.qq.com/cgi-bin` | `stable_token` + IP whitelist | Validated 2026-05-15 via `scripts/wechat-mp/` — draft creation works; freepublish/submit untested. Individual subscription accounts cannot use `freepublish/batchget` for reading history (48001 unauthorized). |

### Browser-automation platforms (2)

| Platform | Why automation? |
|---|---|
| Twitter (X) | API requires paid tier (Basic+: $200/mo). Web automation via pwright on `x.com/home` inline composer is free. Validated 2026-05-15 via `scripts/twitter/` — compose flow works end-to-end up to the final Post click. |
| Douyin (抖音) | No public publishing API. Only the Open Platform deep-link `snssdk1128://openplatform/share` (requires installed app). We drive the web upload UI. Not yet validated. |

### Deferred platforms

The remaining 11 from AiToEarn's set — Xiaohongshu, WeChat Channels, Kuaishou, Bilibili, TikTok, Meta family, Pinterest, LinkedIn, Google Business — fit one of the two integration patterns. The `Publisher` trait is general enough; adding any of them later is a contained change. They are NOT in the initial build's scope.

## 4. Architecture

```
┌──────────────────────┐
│  Web Portal          │  React + Vite + connect-es (gRPC-Web client)
│  (browser)           │  (separate repo or workspace member)
└─────────┬────────────┘
          │ gRPC-Web (HTTPS, Connect protocol)
          ▼
┌─────────────────────────────────────────────────────────────────┐
│  multipost-server                                                │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │  gRPC API layer (tonic + prost)                            │ │
│  │  Services: Accounts, Posts, Jobs, Media, Webhooks          │ │
│  └────────────────────┬───────────────────────────────────────┘ │
│                       │                                         │
│  ┌────────────────────▼───────────────────────────────────────┐ │
│  │  Job Orchestrator                                          │ │
│  │  - State machine: Queued → Validating → Uploading →        │ │
│  │      Submitting → Confirmed | Failed | Cancelled           │ │
│  │  - Retry with backoff, dead-letter after N failures        │ │
│  │  - Scheduled-at queue (sleep-and-wake worker)              │ │
│  │  - Per-platform rate-limit token buckets                   │ │
│  └────────────────────┬───────────────────────────────────────┘ │
│                       │                                         │
│  ┌────────────────────▼───────────────────────────────────────┐ │
│  │  Publisher Dispatcher                                      │ │
│  │  HashMap<Platform, Box<dyn Publisher>>                     │ │
│  └─────┬──────────────────────────────────────┬───────────────┘ │
│        │                                      │                 │
│  ┌─────▼─────────────────────┐    ┌──────────▼──────────────┐  │
│  │  ApiPublisher impls       │    │  BrowserPublisher impls │  │
│  │  (reqwest + token mgmt)   │    │  (pwright + per-account │  │
│  │                           │    │   persistent profiles)  │  │
│  │  • YouTubePublisher       │    │  • TwitterPublisher     │  │
│  │  • WxGzhPublisher         │    │  • DouyinPublisher      │  │
│  │                           │    │                         │  │
│  │  (8 more deferred:        │    │  (3 more deferred:      │  │
│  │   TikTok, Meta x3,        │    │   Xhs, WxSph, Kwai,     │  │
│  │   Pinterest, LinkedIn,    │    │   Bilibili)             │  │
│  │   GoogleBiz)              │    │                         │  │
│  └───────────────────────────┘    └─────────────────────────┘  │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │  Storage layer (quiver-orm + Postgres)                     │ │
│  │  - accounts, jobs, posts, media, oauth_state               │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │  Media store: S3-compatible API → rustfs (local docker)    │ │
│  │  or any S3 provider (AWS, R2, MinIO) via the same crate    │ │
│  └────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────┘
          ▲                                          ▲
          │ gRPC (TLS)                               │ HTTP callbacks
          │                                          │ (OAuth, webhooks)
┌─────────┴──────────┐                       ┌───────┴────────────┐
│  multipost-cli     │                       │  Public callback   │
│  (clap, tonic)     │                       │  router (separate  │
│                    │                       │  HTTP server)      │
└────────────────────┘                       └────────────────────┘
```

### Crate layout (single Cargo workspace)

```
multipost/
├── Cargo.toml          # workspace
├── crates/
│   ├── multipost-core         # shared types, Publisher trait, Content model
│   ├── multipost-proto        # .proto files + generated code
│   ├── multipost-storage      # quiver-orm schema + repositories
│   ├── multipost-orchestrator # job state machine, scheduler
│   ├── multipost-publishers   # platform implementations (4 in scope)
│   │   ├── youtube/           # API   — Phase 1
│   │   ├── wx-gzh/            # API   — Phase 2
│   │   ├── twitter/           # pwright — Phase 3
│   │   └── douyin/            # pwright — Phase 4
│   │   # Deferred: tiktok, meta (fb/ig/threads), pinterest, linkedin,
│   │   # google-biz, xhs, wx-sph, kwai, bilibili (11 more)
│   ├── multipost-server       # binary: gRPC server + OAuth callback
│   └── multipost-cli          # binary: CLI client
└── web/                       # React frontend, separate npm/pnpm workspace
```

## 5. Core abstraction

```rust
// multipost-core/src/publisher.rs

#[async_trait]
pub trait Publisher: Send + Sync + 'static {
    fn platform(&self) -> Platform;

    /// One-time setup before publishing (e.g., refresh token).
    /// Idempotent; called before every job.
    async fn prepare(&self, ctx: &mut PublishContext<'_>) -> Result<()>;

    /// Validate an account's credentials are usable.
    async fn check_auth(&self, account: &Account) -> Result<AuthStatus>;

    /// Submit the content. May be async on the platform side —
    /// returns a platform-specific post handle.
    async fn publish(
        &self,
        ctx: &mut PublishContext<'_>,
        content: &Content,
    ) -> Result<PublishHandle>;

    /// Poll publish status (for platforms with async confirmation).
    async fn confirm(
        &self,
        account: &Account,
        handle: &PublishHandle,
    ) -> Result<PostStatus>;

    /// Delete a published post (best-effort).
    async fn delete(&self, account: &Account, handle: &PublishHandle) -> Result<()>;

    /// Optional: list capabilities so we can reject incompatible content early.
    fn capabilities(&self) -> Capabilities;
}
```

### The `Content` model

Unified shape; per-platform overrides allowed.

```rust
pub struct Content {
    pub kind: ContentKind,                   // Image | Video | Article | Text
    pub text: String,                        // Universal caption / body
    pub hashtags: Vec<String>,
    pub mentions: Vec<Mention>,
    pub media: Vec<MediaRef>,                // by ID into media store
    pub schedule_at: Option<DateTime<Utc>>,
    pub visibility: Visibility,              // Public | Followers | Unlisted | Draft
    pub location: Option<GeoPoint>,
    pub overrides: HashMap<Platform, PlatformOverride>,
}

pub enum ContentKind { Text, Image, Carousel, ShortVideo, LongVideo, Article }

pub struct PlatformOverride {
    pub text: Option<String>,
    pub hashtags: Option<Vec<String>>,
    pub thumbnail: Option<MediaRef>,         // e.g. YouTube custom thumbnail
    pub raw: Option<serde_json::Value>,      // escape hatch
}
```

### Capabilities

```rust
pub struct Capabilities {
    pub max_text_chars: usize,
    pub max_images: usize,
    pub video_supported: bool,
    pub video_max_seconds: Option<u32>,
    pub schedule_supported: bool,
    pub edit_supported: bool,
    pub delete_supported: bool,
}
```

Enforced at job-validation time; mismatched content fails fast with a clear error before any HTTP/browser call.

## 6. gRPC service surface

```proto
service Accounts {
  rpc StartAuth(StartAuthRequest) returns (StartAuthResponse);  // returns URL or QR code
  rpc CompleteAuth(CompleteAuthRequest) returns (Account);
  rpc List(ListAccountsRequest) returns (ListAccountsResponse);
  rpc Revoke(RevokeRequest) returns (google.protobuf.Empty);
  rpc CheckAuth(CheckAuthRequest) returns (AuthStatus);
}

service Media {
  rpc Upload(stream UploadChunk) returns (Media);    // client streaming
  rpc Get(MediaRef) returns (Media);
  rpc Delete(MediaRef) returns (google.protobuf.Empty);
}

service Posts {
  rpc Submit(SubmitRequest) returns (Job);           // create + enqueue
  rpc Schedule(ScheduleRequest) returns (Job);       // create with future time
  rpc GetJob(JobRef) returns (Job);
  rpc ListJobs(ListJobsRequest) returns (ListJobsResponse);
  rpc Cancel(JobRef) returns (Job);
  rpc Retry(JobRef) returns (Job);
  rpc Watch(JobRef) returns (stream JobEvent);       // server streaming
}

service Webhooks {
  // Receive platform webhooks (e.g. Douyin publish callbacks, Twitter user revocation)
  rpc Receive(WebhookEvent) returns (google.protobuf.Empty);
}
```

A `Job` represents one `(content, account)` pair. A single `Submit` with `targets=[twitter, douyin, xhs]` fans out into 3 `Job`s sharing one `content_id`.

## 7. Authentication

Two distinct authentication axes:

- **A. Platform credentials** — how `multipost` authenticates against Twitter, Douyin, etc. on behalf of a user.
- **B. multipost user auth** — how a user authenticates against the `multipost` server itself (the multi-tenant axis).

### A. Platform credentials

Per-platform acquisition differs wildly. We model 3 patterns:

| Pattern | Used by | Flow |
|---|---|---|
| **OAuth redirect** | Twitter, YouTube, TikTok, Meta, LinkedIn, Pinterest, Google Biz | `StartAuth` returns an auth URL; user opens it, approves, platform redirects to our callback endpoint, we store tokens |
| **Developer credentials** | WeChat MP | User supplies `appid` + `app_secret`; we mint `stable_token` ourselves |
| **Browser login** | Douyin, Xhs, WxSph, Kwai, Bilibili | `StartAuth` launches a headed pwright session into the per-account profile; user logs in (password or QR); cookies/localStorage persist in the profile dir (§8) |

#### Token storage

- `accounts.credentials_encrypted` — sealed with a server-side master key (env: `MULTIPOST_MASTER_KEY`)
- AES-256-GCM via `ring` or `aes-gcm` crate
- Refresh-token rotation logged for audit
- A background task refreshes tokens proactively at 80% of their TTL

#### OAuth callback host

The server **must be reachable from the public internet** for OAuth redirects. Two modes:

1. **Tailscale-only**: bind to Tailscale IP (per `infra-defaults` rule 8), accept redirects from a tunneled `tailscale serve` URL — works only for personal use.
2. **Public**: bind behind a reverse proxy with TLS (Caddy or nginx). Required for multi-user deployments.

OAuth state (`state` param) is a signed CSRF token tied to a 10-minute pending-auth row.

### B. multipost user auth — **deferred to Phase 7 (locked §20.2)**

Multi-tenancy is in the schema from day 0 (every row carries `user_id`), but the **interactive login UI is deferred**. Phases 0–6 run in a single-admin bootstrap mode:

- A long-lived **bootstrap bearer token** is configured via `MULTIPOST_BOOTSTRAP_TOKEN` env var (or generated on first launch into `~/.multipost/bootstrap.token`)
- Every gRPC call must present `Authorization: Bearer <token>`; missing/wrong → `UNAUTHENTICATED`
- The token resolves to a synthetic `user_id` (configurable: `MULTIPOST_BOOTSTRAP_USER_ID`, default `00000000-0000-0000-0000-000000000001`)
- All inserts use this `user_id`; all queries filter by it — so schemas and code paths are real multi-tenant from day 1
- CLI reads the token from `~/.multipost/config.toml` or env

Phase 7 then adds the login UI without any schema migration:

- `Users` service: `Signup`, `Login`, `Logout`, `RefreshToken`, `ResetPassword`
- Sessions issued as JWTs (HS256 or RS256), backed by `user_sessions` table
- Web portal gets a login page; existing bootstrap token continues to work as an "admin-impersonate" mechanism for ops
- Optional: pluggable IdP via the cloned `simple-idm` fork in `thirdparty/`, if it suits — decided in Phase 7

This keeps Phases 0–6 free of any login/UX work while preserving the multi-tenant abstraction.

## 8. Browser-automation pattern (using pwright)

### Persistent profile per account (locked §17.2)

Each `(platform, account)` pair gets its own **persistent browser profile directory** on disk. The profile carries cookies, localStorage, IndexedDB, service workers, and the browser's fingerprint state. Sessions are short-lived; the profile is the durable artifact.

```
~/.multipost/profiles/
├── douyin/
│   ├── <account-uuid-1>/         # full Chrome user-data-dir
│   ├── <account-uuid-2>/
│   └── <account-uuid-3>/
├── xhs/
│   └── <account-uuid-1>/
├── wx-sph/
└── ...
```

Why per-profile instead of per-session-cookie-injection:
- Anti-bot detection relies heavily on browser fingerprint, localStorage flags, and slow-build trust signals. A reused profile *is* the same browser to the platform.
- Login-once, post-many: profiles outlive any single pwright session.
- One Chrome user-data-dir cannot be opened by two processes simultaneously, so this naturally enforces per-account concurrency=1.

```rust
// multipost-publishers/src/douyin/mod.rs

pub struct DouyinPublisher {
    pwright: Arc<PwrightClient>,    // CDP endpoint at chrome-cdp:9000 per infra-defaults
    profiles: Arc<ProfileStore>,    // ~/.multipost/profiles/douyin/<uuid>/
    storage: Arc<Storage>,
}

#[async_trait]
impl Publisher for DouyinPublisher {
    async fn publish(
        &self,
        ctx: &mut PublishContext<'_>,
        content: &Content,
    ) -> Result<PublishHandle> {
        // 1. Attach to the per-account persistent profile
        let profile = self.profiles.get_or_create(ctx.account.id).await?;
        let session = self.pwright
            .attach_with_profile(&profile.user_data_dir)
            .await?;

        // 2. Check we're still logged in; fail fast if not
        session.goto("https://creator.douyin.com").await?;
        if !is_logged_in(&session).await? {
            return Err(PublishError::AuthExpired);
        }

        // 3. Navigate + snapshot-act-snapshot
        session.goto("https://creator.douyin.com/creator-micro/content/upload").await?;
        let snap = session.snapshot().await?;
        session.click_by_role(&snap, "button", "上传视频").await?;
        session.set_input_files(/* video */).await?;
        // ...

        // 4. Capture post URL
        let url = session.url().await?;
        let post_id = parse_douyin_url(&url)?;
        Ok(PublishHandle::external(post_id))
    }
}
```

### Profile lifecycle

- **Created** lazily on first login for a new account
- **Locked** by an in-process mutex per profile UUID (prevents concurrent open)
- **Backed up** periodically: tar+encrypt the user-data-dir into media-store under `profile-backups/<account>/<date>.tar.gz` so accidental corruption is recoverable
- **Quarantined** on detected login expiry — pwright keeps the dir, surfaces an `AuthExpired` event, orchestrator schedules a re-auth task that opens the dir in headed mode for the user to log in

### Key rules for automation publishers

1. **Always attach, never launch.** pwright's CLAUDE.md is explicit on this — browser is already running at the configured CDP endpoint. We pass a `--user-data-dir` argument when launching the browser **once**; subsequent sessions attach.
2. **One process per profile dir.** Mutex-guarded; reject concurrent jobs for the same account.
3. **Snapshot-act-snapshot.** Every action is preceded by a fresh snapshot for ARIA-role-based targeting (resilient to CSS class churn).
4. **Detect login expiry early.** Each session begins with an `is_logged_in()` check; failure short-circuits to `AuthExpired` so the orchestrator routes to re-auth instead of retrying.
5. **No mutation of profile on read paths.** `check_auth` opens the profile read-only-ish (still uses CDP attach, but does not navigate to non-auth pages); minimizes accidental tracker writes.
6. **Per-platform concurrency budget.** Beyond per-account, a platform-level semaphore caps total concurrent browser instances (memory ceiling).

## 9. Job orchestration

State machine:

```
        ┌────────┐
        │ Queued │
        └───┬────┘
            │
       ┌────▼─────────┐
       │ Validating   │  ── capability check, content size, scheduled-at
       └────┬─────────┘
            │
       ┌────▼─────────┐
       │ Uploading    │  ── pre-upload media (if platform requires it)
       └────┬─────────┘
            │
       ┌────▼─────────┐
       │ Submitting   │  ── call Publisher::publish
       └────┬─────────┘
            │
       ┌────▼─────────┐                    ┌─────────────┐
       │ Confirming   │── timeout/poll ───▶│  Confirmed  │ ◀── permalink stored
       └────┬─────────┘                    └─────────────┘
            │
            ├── transient error ──▶ Backoff ──┐
            │                                  │
            └── permanent error ───▶ Failed   │
                                              │
                                              └─▶ Queued (retry)
```

- Retries: exponential backoff, jitter, max 5 attempts (configurable per platform).
- Auth errors (`AuthExpired`) skip retry, surface to user immediately.
- Rate-limit errors honor `Retry-After`.
- Dead-letter queue: jobs failed beyond max-attempts go to a separate table for manual inspection.

## 10. Storage schema (quiver-orm)

**Multi-tenant from day 0 (locked §17.3).** Every row except `oauth_states` carries `user_id`; every gRPC method derives `user_id` from the auth bearer token and filters by it. No "system" tables share data across tenants.

```
table accounts {
  id            uuid pk
  user_id       uuid       # multi-tenant from day 1
  platform      enum
  display_name  string     # e.g. @handle
  external_id   string     # platform-side account ID
  credentials   bytes      # AES-GCM encrypted JSON
  auth_status   enum       # Active | Expired | Revoked
  capabilities  json       # cached from /me endpoint
  created_at    timestamptz
  updated_at    timestamptz
  last_used_at  timestamptz
}

table contents {
  id            uuid pk
  user_id       uuid
  kind          enum
  text          text
  hashtags      string[]
  media_refs    uuid[]
  metadata      json
  created_at    timestamptz
}

table jobs {
  id            uuid pk
  content_id    uuid fk
  account_id    uuid fk
  state         enum
  scheduled_at  timestamptz nullable
  attempts      int
  last_error    text nullable
  external_id   string nullable  # platform post ID after submission
  permalink     string nullable
  created_at    timestamptz
  updated_at    timestamptz
}

table media {
  id            uuid pk
  user_id       uuid
  uri           string       # s3://<bucket>/<key> — points at rustfs or external S3
  mime_type     string
  size_bytes    bigint
  width, height int nullable
  duration_ms   int nullable
  sha256        string
  created_at    timestamptz
}

table profiles {
  id            uuid pk
  user_id       uuid
  account_id    uuid fk
  platform      enum
  user_data_dir string                  # absolute path on host
  backup_uri    string nullable         # s3://.../profile-backups/<account>/...
  last_backup_at timestamptz nullable
  status        enum                    # Active | Quarantined | Corrupted
  created_at    timestamptz
}

table oauth_states {
  state         string pk        # signed CSRF token
  user_id       uuid
  platform      enum
  expires_at    timestamptz
  created_at    timestamptz
}
```

DB: PostgreSQL at `docker.yuacx.com:5432` (per `infra-defaults`). Migrations via quiver-orm's migration engine if available, else `sqlx migrate`.

## 11. CLI surface (multipost-cli)

```
multipost auth login <platform>           # interactive: OAuth url or browser launch
multipost auth list
multipost auth check <account-id>
multipost auth revoke <account-id>

multipost media upload <file>             # returns media-id
multipost media list

multipost post \
  --to twitter,douyin,xhs \
  --text "..." \
  --image /path/to/a.jpg \
  --image /path/to/b.jpg \
  --schedule 2026-05-15T09:00:00Z

multipost job list [--state Queued|Running|Failed]
multipost job get <job-id>
multipost job watch <job-id>              # streams JobEvent
multipost job cancel <job-id>
multipost job retry <job-id>

multipost server start                    # convenience: spawn the server locally
```

Server URL defaults to `http://multipost.local:8088` (per `infra-defaults` rule 8 on MagicDNS), overridable via `--server` or `MULTIPOST_SERVER` env var.

## 12. Web portal

Minimal feature set:

| Page | Purpose |
|---|---|
| `/` Dashboard | Recent jobs (state-grouped), account health summary |
| `/compose` | Cross-platform composer: pick accounts, write text, drag-drop media, per-platform preview, schedule |
| `/jobs` | Searchable/filterable list of all jobs, click for detail + retry/cancel |
| `/accounts` | List accounts, "Connect new account" launches platform-specific auth flow |
| `/media` | Browse uploaded media, delete unused |

Stack: React + Vite + connect-es (Buf's gRPC-Web client) + Tailwind. Auth via session cookie issued by server after a simple username/password (multi-user from day 1).

## 13. Capabilities matrix (rough)

| Platform | Text limit | Images | Video | Schedule | Hashtags | Note |
|---|---:|---:|---|---|---|---|
| Twitter | 280 (4K paid) | 4 | 2:20 | ❌ (paid only via API v2) | ✅ | Threading supported |
| YouTube | 5000 (desc) | 1 thumb | required | ✅ | ✅ | Shorts vs long form differ |
| TikTok | 2200 | — | ≤3 min API | ❌ | ✅ | Content posting API in beta |
| Facebook | 63206 | 80 | 240 min | ✅ | ✅ | Page only, not personal |
| Instagram | 2200 | 10 carousel | 60s Reels API | ✅ | ✅ | Business accounts only |
| Threads | 500 | 10 | 5 min | ❌ | manual | API just launched |
| Pinterest | 800 | 1 | 15 min | ✅ | ✅ | Pin board required |
| LinkedIn | 3000 | 9 | 10 min | ✅ | ✅ | Personal or Company page |
| WeChat MP | unlimited body | inline | inline | ❌ via API | ❌ | Drafts → freepublish; **does not appear on homepage** (platform limit) |
| Bilibili | 250 (title) | thumb | required | ✅ | ✅ tags | Long-video focused |
| Douyin | 55 | 35 carousel | ≤15 min | ✅ via UI | ✅ | Automation |
| Xhs | 1000 | 18 | 15 min | ❌ | ✅ | Automation |
| WxSph | 600 | 9 | 60 min | ❌ | manual | Automation |
| Kwai | 1000 | 9 | 10 min | ✅ via UI | ✅ | Automation |
| Google Biz | 1500 | 10 per post | — | ✅ | ❌ | Posts = events/updates |

Numbers from public docs as of design date; verify in each publisher's `capabilities()` at impl time.

## 14. Observability

- Tracing: `tracing` + `tracing-subscriber`, JSON logs in prod.
- Metrics: prom-exporter (job counts by state, publish latency, error rate per platform).
- Job-level audit log: every state transition written to `jobs.events` JSONB column.
- Per-platform health endpoint: `GET /healthz?platform=douyin` runs a synthetic auth check.

## 15. Security

- All platform credentials encrypted at rest (AES-GCM, server master key in env)
- Webhook signature verification per platform (Twitter, Meta provide HMAC; WeChat has its own crypto)
- Server gRPC requires bearer token (issued by the server's user-management surface)
- OAuth callback uses signed `state` to prevent CSRF
- No credentials in logs; structured log redaction for tokens/cookies

## 16. Stack & dependencies

- **gRPC**: `tonic` + `prost` (locked §17.1)
- **Browser automation**: [`pwright`](https://github.com/Shuozeli/pwright) — CDP endpoint `chrome-cdp:9000`
- **HTTP client**: `reqwest` (rustls)
- **Async runtime**: `tokio`
- **DB / ORM**: `quiver-orm` against Postgres at `docker.yuacx.com:5432`
- **Media storage**: S3 API client (`aws-sdk-s3` or `rust-s3`) → backed by [`rustfs`](https://github.com/rustfs/rustfs) in local docker-compose; swappable for AWS/R2/MinIO in prod (locked §17.6)
- **CLI**: `clap` v4
- **Encryption**: `aes-gcm` + `ring`
- **Time**: `chrono` + `chrono-tz`
- **Web**: React 18 + Vite + connect-es + Tailwind (separate `web/` workspace)

### docker-compose (local dev)

```yaml
# docker-compose.yml
services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_USER: multipost
      POSTGRES_PASSWORD: multipost
      POSTGRES_DB: multipost
    ports:
      - "${TAILSCALE_IP:-0.0.0.0}:5433:5432"     # 5433 to avoid clash with shared 5432
    volumes:
      - pgdata:/var/lib/postgresql/data

  rustfs:
    image: rustfs/rustfs:latest
    environment:
      RUSTFS_ROOT_USER: multipost
      RUSTFS_ROOT_PASSWORD: multipost-dev-secret
    ports:
      - "${TAILSCALE_IP:-0.0.0.0}:9100:9000"      # S3 API
      - "${TAILSCALE_IP:-0.0.0.0}:9101:9001"      # rustfs console
    volumes:
      - rustfs-data:/data

  multipost-server:
    build: .
    depends_on: [postgres, rustfs]
    environment:
      MULTIPOST_DB_URL: postgres://multipost:multipost@postgres:5432/multipost
      MULTIPOST_S3_ENDPOINT: http://rustfs:9000
      MULTIPOST_S3_BUCKET: multipost-media
      MULTIPOST_S3_ACCESS_KEY: multipost
      MULTIPOST_S3_SECRET_KEY: multipost-dev-secret
      MULTIPOST_MASTER_KEY_FILE: /run/secrets/master_key
      PWRIGHT_CDP_URL: http://chrome-cdp:9000
    ports:
      - "${TAILSCALE_IP:-0.0.0.0}:8088:8088"      # gRPC + OAuth callback (same listener)
    secrets:
      - master_key
    volumes:
      - profiles:/var/lib/multipost/profiles      # persistent browser profiles

volumes:
  pgdata:
  rustfs-data:
  profiles:

secrets:
  master_key:
    file: ./secrets/master_key.bin
```

Port mappings follow `infra-defaults` rule 8 (bind to `$TAILSCALE_IP`). Service-to-service uses the docker network DNS; user-facing URLs use MagicDNS.

### OAuth callback host (locked §17.5)

The callback lives on the **same tonic server** but on a separate listener (HTTP/1.1, since OAuth providers redirect with `GET` and don't speak gRPC). Implementation:

- `multipost-server` binary spawns two listeners on startup:
  - `0.0.0.0:8088` — tonic gRPC (+ gRPC-Web for the browser portal via `tonic-web`)
  - `0.0.0.0:8089` — Axum HTTP for `/oauth/callback/{platform}` and webhook receivers
- Both share the same `AppState` (DB pool, encryption key, OAuth state store).
- Single binary, single docker container, single TLS termination point (Caddy in front).

### Quotas (configurable, locked §20.3)

Per-tenant resource caps are **defined in server config, not hardcoded**. No fixed limits ship in the binary; the deployer picks values.

Config surface (`/etc/multipost/config.toml` or env-var overrides):

```toml
[quotas.default]                    # applied to every user unless overridden
max_accounts_per_user        = 50
max_accounts_per_platform    = 10
max_jobs_per_day             = 1000
max_jobs_concurrent          = 20
max_media_bytes              = 10_737_418_240   # 10 GiB
max_media_count              = 5000
max_scheduled_jobs           = 200
max_content_text_chars       = 100_000
max_profile_count            = 50               # browser profiles across all platforms

[quotas.per_user."user-uuid-here"]   # optional per-user override
max_jobs_per_day             = 5000
max_media_bytes              = 107_374_182_400  # 100 GiB
```

Enforcement points:
- **Job submission**: orchestrator rejects with `RESOURCE_EXHAUSTED` + which quota tripped
- **Media upload**: streaming handler aborts past `max_media_bytes`
- **Account creation**: `Accounts.StartAuth` checks `max_accounts_per_user` and `max_accounts_per_platform`

`quotas.default` of `0` for any key = unlimited. So a single-user homelab deploy can just set everything to 0 and forget about it; a multi-tenant SaaS deploy can tighten per-user.

Live reload: `SIGHUP` re-reads config without restart (quota changes take effect for new requests; in-flight jobs not affected). Hot-edit per-user quotas via gRPC `Admin.SetUserQuota`.

## 17. Locked decisions

All 6 open questions resolved 2026-05-14:

1. **gRPC framework**: **`tonic` + `prost`**. Mature OAuth ecosystem, large crate compatibility, `tonic-web` provides browser support directly.
2. **Browser session strategy**: **Persistent profile per account**. Each `(platform, account_id)` owns a Chrome user-data-dir on disk. Sessions attach to it short-lived; the profile carries cookies, localStorage, and fingerprint state across reboots. Per-account concurrency naturally capped at 1 by user-data-dir locking. See §8 for the full pattern.
3. **Multi-tenancy**: **Multi-tenant from day 0**. Every row carries `user_id`; auth bearer token derives tenant scope.
4. **Bilibili**: **Automation, not API**. The official video-upload endpoint is restricted to enterprise/contracted partners; we treat all personal accounts uniformly via pwright. Lives under `multipost-publishers/bilibili/` (5 automation platforms total).
5. **OAuth callback host**: **Same server, separate listener**. `multipost-server` runs both a tonic gRPC listener (8088) and an Axum HTTP listener (8089) sharing the same process and state. One binary, one container, one TLS termination point. See §16.
6. **Media storage**: **S3-compatible via [rustfs](https://github.com/rustfs/rustfs)** in the docker-compose default. The storage layer is trait-based behind `aws-sdk-s3`, so AWS S3, Cloudflare R2, or MinIO are drop-in replacements in production. See §16 for the compose snippet.

## 18. Phases / milestones

Restructured 2026-05-15 to reflect the 4-platform scope. Each platform has a validated Python prototype that proves the integration is feasible (see `scripts/{youtube,wechat-mp,twitter}/`).

| Phase | Scope | Done = |
|---|---|---|
| **0. Foundation** | Cargo workspace, proto schema, `Publisher` trait, storage skeleton, dispatcher, bootstrap-token auth | `multipost-cli accounts list` returns `[]` against running `multipost-server` |
| **1. YouTube publisher (API)** | OAuth flow, resumable upload, privacy update, delete | Upload→public→delete round-trip via CLI matches what `scripts/youtube/` validated |
| **2. WxGzh publisher (API)** | `stable_token`, media upload, draft creation, `freepublish/submit` | A WeChat MP article published to followers via CLI |
| **3. Twitter publisher (pwright)** | Per-account profile lifecycle, inline-composer compose-and-post | A tweet (text + optional image) posted via CLI |
| **4. Douyin publisher (pwright)** | First-time login flow via QR, video upload via web UI | A short video published to Douyin via CLI |
| **5. Orchestrator hardening** | Retries with backoff, scheduling, per-platform rate-limit buckets, dead-letter queue | Soak test: 1000 jobs across 4 platforms, 0 lost |
| **6. Web portal** | React + connect-es: dashboard, compose, jobs, accounts | Self-hostable; full publish flow works in browser |
| **7. Multi-tenant login UI + observability** | `Users` service, login flow, JWT sessions, metrics, tracing, audit log | Production-ready multi-tenant deploy |

Phases 0–4 are the **MVP**: one binary, one CLI, all four platforms working. Phase 5+ is hardening and surface expansion.

### Time estimate (rough)

| Phase | Estimate |
|---|---|
| 0 — Foundation | 1–2 days |
| 1 — YouTube | 1 day (prototype maps cleanly to Rust) |
| 2 — WxGzh | 1 day |
| 3 — Twitter (pwright) | 2 days (pwright integration is more work than HTTP) |
| 4 — Douyin (pwright) | 2 days (no prototype yet — discovery + UI mapping needed) |
| 5 — Orchestrator | 2–3 days |
| 6 — Web portal | 3–4 days |
| 7 — Multi-tenant + obs | 2–3 days |

Total MVP (Phases 0–4): **~7 days of focused work**. Including 5–7: **~15 days**.

## 19. Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Platform UI churn breaks automation publishers | High | Snapshot-act-snapshot via ARIA roles; per-platform smoke tests in CI; auto-alert on first failure |
| Platform API access revoked (e.g., Twitter / TikTok policy changes) | High | Multiple paid tiers known; fallback to automation as last resort |
| Cookie/login expiry storm across many automation accounts | Medium | Stagger re-auth, proactive renewal at 80% of session lifetime |
| OAuth scope changes break existing tokens | Medium | Monitor each platform's changelog; surface "needs re-auth" status to user |
| Anti-bot detection bans automation accounts | Medium | Realistic timing, residential proxies (optional), one account-per-session |
| Legal: TOS violations on platforms that forbid automation | High | Document per-platform TOS; let user enable automation explicitly; never claim to bypass anti-bot |

## 20. Locked decisions (round 2)

All remaining open items resolved 2026-05-15:

1. **Final project name**: **`multipost`**. Directory renamed `shuozeli/_wip/crosspost-rs/` → `shuozeli/_wip/multipost/`. Crate names: `multipost-core`, `multipost-server`, `multipost-cli`, etc.
2. **User-management surface**: **Deferred to Phase 7**. Phases 0–6 use a single bootstrap bearer token (env: `MULTIPOST_BOOTSTRAP_TOKEN`) that maps to a fixed `user_id`. Schema is multi-tenant from day 0 so no migration needed when login UI lands. See §7.B.
3. **Quota policy**: **Fully configurable**, no hardcoded caps. Server config (`/etc/multipost/config.toml`) defines defaults + per-user overrides; `0` = unlimited. Live-reloadable via `SIGHUP` or `Admin.SetUserQuota` gRPC. See §16 "Quotas" subsection.

---

**Next step (Phase 0 prerequisites):** With both rounds of decisions locked, the project is ready for scaffolding. Per `infra-defaults` rule 3, next docs to create alongside Phase 0 work:

- `docs/architecture.md` — distilled architecture diagram + boot sequence + request lifecycle
- `docs/tasks.md` — Phase 0 work breakdown (workspace setup, proto schema, Publisher trait, storage skeleton, dispatcher)
- `README.md` — project intro + quickstart against the docker-compose
- `docs/codelabs/01-hello-multipost.md` — "post a tweet from the CLI" walkthrough (placeholder until Phase 1 lands)

I won't auto-create these — per the `feedback_no_premature_scaffolding` rule, they wait for your explicit go.

---

## 22. Phase 5 redesign — thin executor for downstream callers

> Decision (2026-05-18): multipost is a **server that downstream callers
> invoke to post**. It does not own a queue, cron, or retry policy.
> Callers schedule and retry. multipost executes one publish attempt
> per Submit and exposes the resulting Job for follow-up.
>
> This supersedes §9 (Job orchestration) and §6 `Posts.Schedule` / `Posts.Retry`.

### 22.1 What multipost owns

- Publisher abstraction — per-platform quirks, browser profiles, OAuth tokens.
- Credential storage (`accounts`, encrypted blobs).
- Media storage — upload-once, post-many.
- Synchronous execution of `Publisher::publish` (one attempt).
- A short-lived background task per Submit that runs `Publisher::confirm`
  polling while the platform finishes its own pipeline (moderation,
  encoding). **Not retries** — purely "wait for the platform to settle".
- Crash-recovery scan on startup: re-attach a confirm-poll task to any
  job left in `Confirming` from within the last 24h. (Not a cron — runs
  once at boot.)
- Job records as observable history (so callers can `GetJob`, `Watch`,
  `Cancel`).

### 22.2 What multipost does NOT own

- Cron / `Posts.Schedule` — caller's scheduler invokes Submit at the right
  time. Remove from proto.
- Retries on `Transient` / `RateLimited` errors — caller re-Submits.
  Remove `Posts.Retry` from proto.
- Workflow chains, conditional posts, A/B variants — caller composes.
- Dead-letter queue — `Failed` is terminal, caller decides what to do.

### 22.3 Submit semantics

Submit blocks **until `Publisher::publish` returns**, then responds. Total
caller-facing latency: roughly the upload + form-fill window per platform
(YouTube ≈10–30 s for a 1 MB video; Douyin ≈15 s including SCP).

```
Caller                            multipost-server
  │                                       │
  │ Posts.Submit ─────────────────────────▶
  │                              publish() runs synchronously
  │                              returns PublishHandle
  │ ◀───────────────────── Job(state=Confirming, external_id, permalink)
  │
  │   (server now spawns a background task that polls
  │    Publisher::confirm() until Confirmed | Failed)
  │
  │ Posts.GetJob ─────────────────────────▶
  │ ◀────────────────────── Job(state=Confirming | Confirmed | Failed)
  │   ... caller polls until terminal, or opens Posts.Watch stream.
```

Why this shape:
- Caller learns immediately whether the upload landed (most failures
  happen during publish, not during platform-side moderation).
- Caller isn't held on a long RPC during moderation that can take
  minutes.
- Background confirm-poll uses the existing `MAX_CONFIRM_POLLS` /
  `CONFIRM_DELAY_SECS` constants but lives in a `tokio::spawn` task, not
  inline in the Submit handler. Server restart loses the task; the
  startup scan (§22.1) re-attaches.

### 22.4 Posts proto delta

Trim and clarify:

```proto
service Posts {
  rpc Submit(SubmitRequest) returns (Job);          // blocks until publish() returns
  rpc GetJob(GetJobRequest) returns (Job);           // optional long-poll: wait_seconds
  rpc ListJobs(ListJobsRequest) returns (ListJobsResponse);
  rpc Cancel(JobRef) returns (Job);                  // calls publisher.delete + marks Cancelled
  rpc Watch(JobRef) returns (stream JobEvent);       // server-streaming alternative to long-poll

  // REMOVED:
  // rpc Schedule(ScheduleRequest) returns (Job);    — caller schedules
  // rpc Retry(JobRef) returns (Job);                — caller re-Submits
}

message SubmitRequest {
  Content content = 1;
  repeated string account_ids = 2;
}

message GetJobRequest {
  string id = 1;
  // Long-poll: when > 0 AND the job is in a non-terminal state, the
  // server blocks for up to `wait_seconds` waiting for the next state
  // transition, then returns the current Job. Returns immediately when
  // `wait_seconds = 0` or the job is already terminal.
  // Caller pattern is a loop until terminal:
  //   while job.state ∉ {Confirmed, Failed, Cancelled} do GetJob(id, wait=30)
  // The server caps `wait_seconds` at MAX_LONG_POLL_SECS (60) to keep
  // misbehaving callers from tying up connections.
  int32 wait_seconds = 2;
}
```

**Why long-poll over server-push webhooks.** An earlier draft of this
doc proposed a `callback_url` field on SubmitRequest where the server
would POST the terminal Job to a caller-supplied URL. That was dropped
because (a) it adds an outbound HTTP retry path that breaks the
"multipost executes one attempt, callers own retries" invariant, and
(b) callers were going to poll anyway — long-poll on GetJob covers the
same need without inverting the dependency direction.

### 22.5 Idempotency via content-hash dedup

When Submit arrives, compute a deterministic hash over the dedup key:

```
hash = sha256(
    account_id,
    content.text,
    content.media_ids (in submitted order),
    content.visibility,
    content.schedule_at | "",
)
```

Then:
- If an existing Job for this `tenant_id` has the same hash AND is in
  any non-Failed state AND `created_at > now() - 1h`: return that job.
  (Same `JobRef`, no new work.)
- Otherwise: persist a new Job with this hash, run publish.

The 1h window is meant to absorb caller retry storms (e.g., crash-loop
calling Submit every few seconds). Beyond 1h, posting the same content
again is treated as a deliberate decision.

This is implicit dedup — callers do not need to send a key. Trade-off
captured: a caller who deliberately wants to post the same content twice
within 1h has to vary something (e.g., a hashtag) or wait. Acceptable
for the launch surface; revisit if customers complain.

### 22.6 Multi-tenant auth — promoted to Phase 5

Was "deferred to Phase 7" in §7B; now load-bearing because downstream
callers exist.

- Auth method: **static API keys**. gRPC metadata
  `authorization: Bearer <key>`. Keys are 32 random bytes, base64-url.
- Storage: `tenants.json` (file-backed for parity with `accounts.json`
  / `jobs.json`; swappable for quiver later):
  ```json
  {
    "tenants": {
      "<tenant_id>": {
        "id": "<tenant_id>",
        "name": "human-readable",
        "key_hashes": ["sha256:..."],
        "created_at": "..."
      }
    }
  }
  ```
- Server-side interceptor:
  1. Extract bearer token from metadata.
  2. Compute sha256, look up among `key_hashes`.
  3. Inject `tenant_id` into the request extensions.
  4. Every gRPC handler reads `tenant_id` from extensions, replacing the
     current `bootstrap_user`.
- Tenant lifecycle (CLI-only for now, not over gRPC):
  - `multipost tenants create --name "..."` → prints API key once.
  - `multipost tenants list`
  - `multipost tenants rotate-key <id>` → adds a new key, leaves old
     working for grace period; `multipost tenants revoke-key <id> <hash-prefix>` removes.
- Backwards-compat: a `MULTIPOST_DEV_NO_AUTH=1` env var bypasses the
  interceptor and binds everything to a `bootstrap_user`. Useful for
  local dev + CLI parity during the transition; production deploys
  must not set it.

### 22.7 Background confirm-poll lifecycle

```rust
// In Posts.Submit handler, after publish() returns Ok(handle):
let job_id = job.id;
let state = self.state.clone();
tokio::spawn(async move {
    poll_confirm_until_terminal(state, job_id).await;
});
return Ok(Response::new(record_to_proto(&job)));
```

`poll_confirm_until_terminal` runs the same `for attempt in 1..=12 { ... }`
loop the inline orchestrator currently uses, but lives in a detached task.
On each transition it:
1. Updates the JobRecord (so `GetJob` reflects state).
2. Emits a `JobEvent` on the in-memory broadcast channel
   (consumed by `Posts.Watch` subscribers and by long-poll `GetJob`).

On server startup, before binding the gRPC port:
```rust
let stuck = jobs.list_in_states(&[Confirming]).filter(|j| j.created_at > now() - 24h);
for j in stuck { spawn_tracked(state.clone(), j.id); }
```

**Graceful shutdown.** Detached `tokio::spawn` tasks would otherwise be
killed on Ctrl-C, leaving jobs orphaned in Confirming and forcing the
next startup scan to recover them. To avoid that, every confirm-poll
task is registered into `AppState::confirm_tasks: Arc<DashMap<JobId, JoinHandle<()>>>`:

```rust
fn spawn_tracked(state: Arc<AppState>, job_id: JobId) {
    let handle = tokio::spawn({
        let state = state.clone();
        async move {
            poll_confirm_until_terminal(state.clone(), job_id).await;
            state.confirm_tasks.remove(&job_id);
        }
    });
    state.confirm_tasks.insert(job_id, handle);
}
```

`main()` installs a SIGINT/SIGTERM listener via `tokio::signal`. On signal,
the gRPC server stops accepting new RPCs, then we `await` each remaining
`JoinHandle` with a 30s deadline:

```rust
let pending: Vec<_> = state.confirm_tasks.iter().map(|e| e.value().abort_handle()).collect();
let deadline = tokio::time::sleep(Duration::from_secs(30));
// race the deadline against draining; whichever wins, exit cleanly.
```

If the deadline trips first, in-flight tasks are aborted and their jobs
remain Confirming for the next boot's recovery scan to re-attach.

### 22.8 Failure semantics callers will see

| `PublishError` | Job ends as | Caller action |
|---|---|---|
| `AuthExpired` | `Failed`, `last_error="auth expired..."` | Re-login the account, then re-Submit. Will not retry. |
| `Rejected` | `Failed`, `last_error="..."` | Adjust content (e.g., shorter title, valid mime), re-Submit. |
| `RateLimited{retry_after_secs}` | `Failed`, `last_error="rate limited..."` | Caller waits `retry_after_secs`, re-Submits. |
| `Transient` | `Failed`, `last_error="..."` | Caller may re-Submit. Multipost did **not** retry internally. |
| `Other` | `Failed`, `last_error="unexpected: ..."` | Caller surfaces to ops. |

The bias is to surface failures fast and put the retry policy in the
caller. We may add a per-tenant config later to opt INTO internal retries
for `Transient` if a class of caller wants it.

### 22.9 Implementation order (proposed)

1. **Tenant auth + interceptor**: smallest blast radius, enables everything else. Add `tenants.json`, CLI commands, gRPC interceptor, swap `bootstrap_user` → `tenant_id` in handlers. ✅ done.
2. **Submit → background confirm-poll**: move the confirm loop out of the inline handler into `tokio::spawn`. Submit returns Confirming. GetJob already works. ✅ done.
3. **Posts.Watch streaming**: in-memory broadcast channel, emit on transition. Implement the RPC. ✅ done.
4. **Long-poll on GetJob**: extend `GetJobRequest` with `wait_seconds`; server blocks on the broadcast bus (same one Watch uses) up to a cap. This is the *pull-based* notification path; Watch is the streaming alternative for clients that can keep a connection open.
5. **Content-hash dedup**: hash function + lookup-on-Submit + 1h window query on JobRepository.
6. **Startup recovery scan**: re-attach confirm-poll to Confirming jobs <24h old.
7. **Proto cleanup**: remove `Schedule` / `Retry` RPCs (or keep stubs that return `unimplemented` + deprecate-in-comment).
8. **docs/architecture.md update**: redraw the sequence diagram with the thin-executor shape.
