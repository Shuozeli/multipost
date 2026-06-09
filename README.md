<!-- agent-updated: 2026-06-09T05:02:03Z -->
# multipost

A pure-Rust gRPC service for **cross-posting to social platforms**, plus
read-only **content discovery** (crawl) and **profile-stats** collection.

> One sentence: submit a piece of content once → it lands on every platform
> you've connected; pull your own dashboard analytics back.

> Working on this repo as an agent? Start with [`CLAUDE.md`](CLAUDE.md) — it has
> the crate map, the browser-automation gotchas, and the CI gates.

## Status

Phase 5 thin-executor design is implemented and exercised end-to-end. Write side
(publish + confirm + delete) covers five platforms; read side adds a recommendation-
feed **crawler** and an owner-dashboard **stats collector** (both per platform).
The server is a multi-tenant gRPC API; callers submit posts and either long-poll
`GetJob` or open a streaming `Watch` to learn when a job lands.

| Platform | Auth | Publish | Images | Confirm | Delete | Tested live |
|---|---|---|---|---|---|---|
| **YouTube** | OAuth 2.0 + PKCE | Video upload (Data API v3) | Custom thumbnail | Polling | API delete | ✓ |
| **WeChat MP** (公众号) | `stable_token` (appid + secret) | Article draft + `freepublish/submit` | — | `freepublish/get` (partial) | API delete | ✓ draft path |
| **Douyin** (抖音) | Chrome profile cookies | Browser-automated video upload | — | Polls manage page | Clicks 删除作品 | ✓ |
| **Toutiao** (头条号) | Chrome profile cookies | 微头条 + article editor (CDP) | ✓ 微头条 (≤9) | Auto-saved / dashboard poll | Drafts UI / 微头条 删除 | ✓ |
| **Twitter / X** | Chrome profile cookies | Inline composer (CDP) | ✓ tweet (≤4) | Immediate | caret → Delete | ✓ |

Images on the cookie-auth platforms are streamed into the remote Chrome over CDP
(no host filesystem access) — see [`CLAUDE.md`](CLAUDE.md). WeChat MP individual
subscription accounts: `freepublish/submit` is gated by Tencent's 48001 — drafts
land, final publish has to be clicked in MP admin.

**Read side:**

| Service | Platforms | What it does |
|---|---|---|
| **Crawl** | Toutiao, Twitter | Drives `pwright` to capture the public recommendation feed → `DiscoveredItem`s (SQLite). |
| **Stats** | Toutiao, Twitter | Drives the account's own dashboard → account totals + per-post metrics (展现/阅读/likes/views…), stored as timestamped snapshots. Richer than the feed. |

## Crate layout

```
multipost/
├── Cargo.toml          workspace
└── crates/
    ├── multipost-core         shared types + 3 traits: Publisher, Crawler, StatsCollector
    ├── multipost-proto        .proto files + tonic-generated bindings
    ├── multipost-storage      JSON repos (accounts/jobs/media/tenants) + SQLite (discovered, stats)
    ├── multipost-orchestrator job state machine types
    ├── multipost-publishers/
    │   ├── youtube            API   (OAuth + resumable upload)
    │   ├── wx-gzh             API   (stable_token + draft/add + freepublish)
    │   ├── douyin             CDP   (SCP staging + DOM.setFileInputFiles)
    │   ├── toutiao            CDP   (editor + 微头条 + images; + stats collector)
    │   └── twitter            CDP   (inline composer + images; + stats collector)
    ├── multipost-crawlers/
    │   ├── toutiao            pwright network-listen → decode 推荐 feed
    │   └── twitter            pwright network-listen → decode For-You feed
    ├── multipost-server       binary: gRPC + OAuth callback HTTP
    └── multipost-cli          binary: CLI client
```

## gRPC surface

```proto
service Tenants  { ... }                       // CLI-only management
service Accounts {
  rpc StartAuth          (StartAuthRequest)          returns (StartAuthResponse);
  rpc CompleteAuth       (CompleteAuthRequest)       returns (Account);
  rpc RegisterDeveloperCredentials(RegisterRequest)  returns (Account);
  rpc List               (ListAccountsRequest)       returns (ListAccountsResponse);
  rpc Get / Revoke / CheckAuth
}
service Media    { rpc Upload (stream UploadChunk) returns (MediaAsset); rpc Get / Delete }
service Posts {
  rpc Submit   (SubmitRequest)       returns (SubmitResponse);   // blocks until publish() returns
  rpc GetJob   (GetJobRequest)       returns (Job);              // wait_seconds long-poll (≤60s)
  rpc Watch    (JobRef)              returns (stream JobEvent);  // streaming alternative
  rpc Cancel   (JobRef)              returns (Job);              // calls publisher.delete()
  rpc ListJobs (ListJobsRequest)     returns (ListJobsResponse);
}
service Crawl {                                                 // read-only: public feed
  rpc Submit (SubmitCrawlRequest) returns (CrawlJob);           // background; poll GetJob
  rpc GetJob / ListItems
}
service Stats {                                                 // read-only: own dashboard
  rpc Collect         (CollectStatsRequest)   returns (StatsSnapshot);    // drives browser, persists
  rpc GetAccountStats (GetAccountStatsRequest) returns (AccountStatsSeries); // growth over time
  rpc ListPostStats   (ListPostStatsRequest)   returns (PostStatsList);   // latest per-post
}
```

Phase 5 design highlights (see [`docs/design.md`](docs/design.md) §22):
- Multi-tenant via static API key (`Authorization: Bearer <key>`); CLI manages tenants directly against `tenants.json`.
- Submit returns at `Confirming`; a tracked `tokio::spawn` task continues `confirm()` polling in the background.
- Content-hash dedup (1 h window) covers caller retry storms without double-posting.
- Startup recovery scan re-attaches confirm-poll tasks to any `Confirming` job ≤24 h old.
- Graceful shutdown drains in-flight tasks for up to 30 s.

## Quick start

```bash
# 0. Build
cargo build --release

# 1. Run the server (binds to 0.0.0.0:8188; set TAILSCALE_IP for tailnet binding).
./target/release/multipost-server &

# 2. Create your first tenant (operates on tenants.json directly).
./target/release/multipost tenants create --name "my-tenant"
# → prints an api_key once; copy it.

export MULTIPOST_API_KEY=<the-key>

# 3. Register a platform account (example: WeChat MP).
./target/release/multipost accounts register-wechat \
  --appid wx... --secret <secret>

# 3b. Cookie-auth platforms point at a Chrome already logged into the account.
./target/release/multipost accounts register-toutiao --cdp-url http://<chrome-host>:<port>
./target/release/multipost accounts register-twitter --cdp-url http://<chrome-host>:<port> --handle <handle>

# 4. Post. Text, images (--image is repeatable), or video.
./target/release/multipost post --to wx-gzh --title "Hello world" --description "..."
./target/release/multipost post --to toutiao,twitter --description "今日速览" --image a.png --image b.jpg
./target/release/multipost post --to youtube --video final.mp4 --thumbnail cover.jpg \
  --title "昨夜星辰" --description "..." --public

# 5. Watch the job to terminal.
./target/release/multipost watch <job-id>

# 6. Read side: crawl the public feed, or collect your own profile stats.
./target/release/multipost crawl --platform twitter --duration 30
./target/release/multipost crawl --platform youtube --url https://www.youtube.com/@flipradio_fearnation/videos --duration 30
./target/release/multipost stats collect --platform toutiao --max-posts 100
./target/release/multipost stats posts   --platform toutiao    # latest per-post numbers
```

## CI

GitHub Actions (`.github/workflows/ci.yml`) gates four jobs on push/PR to `main`:
Build & Test, Clippy (`-D warnings`), Format (`--check`), Documentation. Run them
locally before pushing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
cargo test --workspace
```

For local development, set `MULTIPOST_DEV_NO_AUTH=1` on the server to bypass the API-key check; all requests are then bound to `tenant_id=00000000-0000-0000-0000-000000000000`. Production deploys must not set it.

## Crawl scheduler service

`multipost-server` can also keep the discovery store warm without a caller
submitting crawl jobs. Set:

```bash
PWRIGHT_BIN=/usr/local/bin/pwright
PWRIGHT_CDP=http://alienware-win-yuacx.tail8f3b66.ts.net:9222
MULTIPOST_CRAWL_ENABLED=1
MULTIPOST_CRAWL_PLATFORMS=youtube,toutiao,twitter
MULTIPOST_CRAWL_DURATION_SECS=30
MULTIPOST_CRAWL_INTERVAL_SECS=900
MULTIPOST_YOUTUBE_CRAWL_URLS=https://www.youtube.com/@flipradio_fearnation/videos
```

Scheduled crawls run serially and upsert into
`$MULTIPOST_DATA_DIR/discovered.sqlite`. The same global crawler permit also
serializes manual `Crawl.Submit` requests, because pwright CLI active-tab state
is shared by working directory.

`GET /healthz` returns service and scheduler state:

```json
{
  "status": "ok",
  "db": "ok",
  "crawl": {
    "registered_platforms": ["youtube", "toutiao", "twitter"],
    "available_permits": 1,
    "in_flight_jobs": 0
  },
  "crawl_scheduler": {
    "enabled": true,
    "configured_platforms": ["youtube"],
    "running_platform": null,
    "last_runs": {
      "youtube": {
        "items_captured": 60,
        "last_error": null
      }
    }
  }
}
```

The Docker image builds `multipost-server` and `multipost`, but the runtime must
provide the `pwright` CLI at `$PWRIGHT_BIN` (for example by deriving the image or
mounting the binary).

## Prototype scripts

Each platform was validated end-to-end with a Python prototype before any Rust was written. They live in `scripts/{youtube, wechat-mp, douyin, toutiao, twitter}/` and double as reference implementations + ad-hoc debug tools. Each script is a [`uv run`](https://docs.astral.sh/uv/) standalone (PEP 723 inline deps).

## Non-goals

- AI content generation (bring your own text / images / video)
- Comment / engagement automation
- Monetization, payment, content marketplace
- Mobile / desktop apps (CLI + future web portal only)
- Anti-detection / bot evasion arms race

## License

Dual-licensed under Apache-2.0 OR MIT — your choice.
