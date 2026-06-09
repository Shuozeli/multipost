<!-- agent-updated: 2026-06-09T05:02:03Z -->
# multipost — Tasks

Phase tracking. See `docs/design.md` §18 for phase definitions, `docs/architecture.md` for the system shape.

## Recently landed (post-Phase 5, 2026-05)

- [x] **Twitter / X publisher** — inline-composer tweet via CDP, caret→Delete.
- [x] **Image posting** — Toutiao 微头条 (≤9) + Twitter (≤4); bytes streamed into the
  remote Chrome over CDP (`DataTransfer` + `atob`, CSP-safe). CLI `post --image`.
- [x] **Crawl / discovery** — `Crawl` gRPC service + Toutiao/Twitter crawlers
  (pwright network-listen → `DiscoveredItem` → SQLite); CLI `crawl` / `discovered`.
- [x] **YouTube discovery crawler** — `multipost-crawlers-youtube` crawls channel/video
  pages via pwright DOM extraction; `Crawl.Submit` and CLI `crawl` now accept repeatable
  source URLs for page-based crawling.
- [x] **Crawl scheduler service mode** — `multipost-server` can run configured
  platform crawls on an interval, serializing pwright access and upserting into
  `discovered.sqlite`.
- [x] **Crawl service health** — `/healthz` returns registered crawl platforms,
  permit/job state, and the latest scheduled crawl result per platform.
- [x] **Profile stats** — `Stats` gRPC service + Toutiao/Twitter collectors;
  timestamped account + per-post snapshots (SQLite); CLI `stats collect/account/posts`.
  Toutiao pages the works feed (offset index, dedup by id); Twitter scrapes the profile.
- [x] **YouTube cover + public video CLI** — `post --thumbnail` uploads a custom
  YouTube thumbnail after the video lands; `post --public` is a shortcut for
  `--privacy public`.
- [x] **CI** — GitHub Actions: Build & Test / Clippy (-D warnings) / Format / Doc.
- [x] **CLAUDE.md** — agent onboarding guide.

## Phase 0 — Foundation

Goal: `multipost-cli accounts list` returns `[]` against a running `multipost-server`. No real publishers yet; just the plumbing.

### Workspace setup

- [ ] Create root `Cargo.toml` with `[workspace]`
- [ ] Add `rust-toolchain.toml` pinning to a known-good toolchain
- [ ] `.gitignore` (target/, .env, tokens, profiles/)
- [ ] `.cargo/config.toml` (optional: rustflags, profile tweaks)
- [ ] `docker-compose.yml` with postgres + rustfs + multipost-server placeholder

### Crate skeletons (empty but compiling)

- [ ] `crates/multipost-core` — `lib.rs` with `Publisher` trait + `Content` struct stubs
- [ ] `crates/multipost-proto` — `.proto` files + `build.rs` calling `tonic_build`
- [ ] `crates/multipost-storage` — `lib.rs` with repository traits, real impls deferred
- [ ] `crates/multipost-orchestrator` — `lib.rs` with `JobState` enum
- [ ] `crates/multipost-server` — `main.rs` binds tonic + axum, no real handlers
- [ ] `crates/multipost-cli` — `main.rs` with `clap` subcommands, calls server

### Proto definitions

- [ ] `accounts.proto` — `StartAuth`, `CompleteAuth`, `List`, `Revoke`, `CheckAuth`
- [ ] `posts.proto` — `Submit`, `Schedule`, `GetJob`, `ListJobs`, `Cancel`, `Retry`, `Watch`
- [ ] `media.proto` — `Upload` (client streaming), `Get`, `Delete`
- [ ] `webhooks.proto` — `Receive`
- [ ] `common.proto` — `Platform` enum, `AuthStatus` enum, `JobState` enum

### Core abstractions

- [ ] `Publisher` trait (5 methods: `platform`, `prepare`, `check_auth`, `publish`, `confirm`, `delete`, `capabilities`)
- [ ] `Content` struct + serde
- [ ] `Capabilities` struct
- [ ] Error type (`thiserror`)
- [ ] `PublishContext` struct (passes account, media store handle, http client to publisher)

### Storage skeleton

- [ ] Postgres schema migrations (SQL or sea-orm migration files): `accounts`, `jobs`, `contents`, `media`, `profiles`, `oauth_states`
- [ ] `AccountRepository` trait + Postgres impl (returns empty for now)
- [ ] `JobRepository` trait stub
- [ ] `MediaRepository` trait stub
- [ ] Connection pool setup using `sqlx` / `quiver-orm`

### Server

- [ ] tonic listener on `:8088` (gRPC + tonic-web)
- [ ] axum listener on `:8089` for `/oauth/callback/{platform}` and `/healthz`
- [ ] Shared `AppState` (DB pool, master key, http client)
- [ ] Bootstrap-token auth interceptor (`MULTIPOST_BOOTSTRAP_TOKEN`)
- [ ] `/healthz` returns `{"status": "ok", "db": "ok"}`
- [ ] `Accounts.List` returns `[]` for the bootstrap user_id

### CLI

- [ ] `clap` subcommands stubbed: `auth`, `media`, `post`, `job`, `server`
- [ ] Config: `~/.multipost/config.toml` + env-var overrides
- [ ] `accounts list` calls `Accounts.List` and prints empty table
- [ ] Server URL defaults to MagicDNS hostname per `infra-defaults` rule 8

### Verification

- [ ] `cargo test --workspace` passes (smoke tests only)
- [ ] `cargo clippy --workspace` clean
- [ ] `cargo doc --workspace --no-deps` clean
- [ ] `docker compose up -d` brings up postgres + rustfs + server
- [ ] From host: `multipost-cli accounts list` returns `[]`

---

## Phase 1 — YouTube publisher (API)

- [ ] `crates/multipost-publishers/youtube/` skeleton
- [ ] OAuth 2.0 helper (start, callback, refresh) — port from `scripts/youtube/05*.py` logic
- [ ] `YouTubePublisher::check_auth` via `/channels?mine=true`
- [ ] `YouTubePublisher::publish` via resumable upload to `/upload/youtube/v3/videos`
- [ ] `YouTubePublisher::confirm` polls `processingStatus`
- [ ] `YouTubePublisher::delete`
- [ ] Capabilities matrix entry
- [ ] End-to-end: `multipost-cli post --to youtube --video clip.mp4 --title "..."` works

## Phase 2 — WxGzh publisher (API)

- [ ] `crates/multipost-publishers/wx-gzh/` skeleton
- [ ] `WxGzhAuth` (appid + secret, IP whitelist check)
- [ ] `stable_token` fetch + caching
- [ ] Media upload (temp + permanent)
- [ ] Draft creation
- [ ] `freepublish/submit` + status polling
- [ ] End-to-end: `multipost-cli post --to wx-gzh --article ...` works

## Phase 3 — Twitter publisher (pwright)

- [ ] `crates/multipost-publishers/twitter/` skeleton
- [ ] pwright session attached via CDP
- [ ] Per-account profile management (`profiles/twitter/<uuid>/`)
- [ ] `is_logged_in` check via URL probe
- [ ] Re-auth flow (headed pwright window for QR scan)
- [ ] Compose via `document.execCommand('insertText', ...)` (validated 2026-05-15)
- [ ] Click `[data-testid="tweetButtonInline"]`
- [ ] Capture posted tweet URL
- [ ] End-to-end: `multipost-cli post --to twitter --text "..."` works

## Phase 4 — Douyin publisher (pwright)

- [ ] Discovery script (`scripts/douyin/01_explore.py`) — login flow, upload selectors
- [ ] `crates/multipost-publishers/douyin/` skeleton
- [ ] First-time QR-code login flow
- [ ] Video file upload via the `creator.douyin.com` web UI
- [ ] Caption + hashtag fill
- [ ] Capture published video URL
- [ ] End-to-end: `multipost-cli post --to douyin --video clip.mp4` works

## Phase 5+

Tracked separately when we get there. See `docs/design.md` §18 for the full ladder.

---

## Known issues / blockers

- [ ] **Bug**: pwright `snapshot` crashes on Twitter's home feed with `JSON error: missing field 'value'`. Workaround via `eval` + querySelector works. Upstream issue should be filed against `Shuozeli/pwright`.
- [ ] **Bug**: WeChat MP `freepublish/batchget` returns 48001 for individual subscription accounts. Listing published articles needs browser automation fallback — out of scope for initial 4-platform build, but worth noting.
- [ ] **Decision**: How does multipost handle YouTube's 6-uploads/day quota when a user submits a batch of 10? Reject with `RESOURCE_EXHAUSTED`, or queue and trickle?

## Validation log (prototype evidence)

| Platform | Script dir | Status | Date |
|---|---|---|---|
| YouTube | `scripts/youtube/` | ✓ Full lifecycle: upload → privacy update → delete | 2026-05-15 |
| WeChat MP | `scripts/wechat-mp/` | ✓ Draft creation; freepublish/submit not yet test-fired | 2026-05-15 |
| Twitter | `scripts/twitter/` | ✓ Compose-and-post via pwright; final post click unfired | 2026-05-15 |
| Douyin | (none yet) | ⏳ Phase 4 starts with discovery scripts | — |
