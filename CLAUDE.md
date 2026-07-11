<!-- agent-updated: 2026-06-11T04:59:04Z -->
# multipost — agent guide

Pure-Rust gRPC service for **cross-posting one piece of content to many social
platforms**, plus read-only **content discovery** (crawl) and **profile-stats**
collection. Submit once → it publishes everywhere you've connected; collect
your own dashboard analytics back.

Read this first, then `README.md` (quick start + gRPC surface) and
`docs/design.md` (deep design; §5 abstractions, §8 browser pattern, §22 the
thin-executor redesign). Phase/roadmap status lives in `docs/tasks.md`.

## Multipost identity / when to use this repo

When a user says **"Multipost"**, **"multipost"**, or asks for the social
video uploader under Shuozeli, this is the repo:

- Local path: `/home/cyuan/projects/multipost`
- GitHub remote: `https://github.com/shuozeli/multipost.git`
- CLI binary: `multipost`
- Server binary: `multipost-server`

Do not use `dragb/experimental/yuanchenxi/strict_poster` as the implementation
source for platform publishing. That project is only a caller/client of
Multipost for text/image posts. Do not create new one-off Douyin upload scripts
when Multipost already has the platform publisher; extend this repo instead.

For generated video pipelines, the correct boundary is:

1. The video project produces a reviewed `publish_package.json` plus local
   assets such as final MP4, thumbnail, title, caption, hashtags, and visibility.
2. Multipost handles platform upload, confirmation, and deletion through its
   `Posts`, `Media`, and `Accounts` services.
3. The video project records the resulting Multipost job id / platform receipt
   back into its publish journal or delivery receipt.

Current Douyin video path:

```bash
# Server, usually on the machine that can reach the logged-in Chrome.
MULTIPOST_DEV_NO_AUTH=1 cargo run -p multipost-server -- --data-dir /tmp/mp

# Register the logged-in Douyin Chrome once. For Alienware Chrome over CDP:
multipost accounts register-douyin \
  --cdp-url http://alienware-win-yuacx:9222 \
  --ssh-host alienware-win-yuacx \
  --ssh-user cyuan \
  --remote-temp-dir 'C:/Users/cyuan/Videos/multipost-uploads'

# Publish a reviewed local video. Use --account-id if more than one Douyin
# account is connected. --public maps to Visibility::Public.
multipost post \
  --to douyin \
  --video /path/to/final.mp4 \
  --title "视频标题" \
  --description "视频文案 #标签" \
  --public

multipost watch <job-id>
```

Douyin-specific guardrails:

- The Douyin publisher is in `crates/multipost-publishers/douyin/`.
- It SCP-stages the local video to the Chrome host, then uses
  `DOM.setFileInputFiles`; do not assume the server filesystem is visible to
  Chrome.
- It must wait for Douyin upload completion (`重新上传` / `替换视频`) before filling
  the form or clicking `发布`; otherwise Douyin can silently drop the incomplete
  upload.
- `confirm()` verifies the creator manage page row by title and treats `审核中`
  as pending. Do not report a Douyin upload as successful solely because
  `Submit` returned `Confirming`; use `watch`, `GetJob`, or the manage-page
  confirmation result.
- Douyin custom cover upload is not part of the current publisher contract.
  If a video pipeline needs a Douyin cover, prefer burning the large cover text
  into the first seconds of the MP4 until a real cover asset flow is added here.

## Workspace map

```
crates/
  multipost-core          shared types + the 3 traits (Publisher, Crawler, StatsCollector)
  multipost-proto         .proto files + tonic-generated bindings (build.rs)
  multipost-storage       file-backed repos (accounts/jobs/media/tenants JSON) + SQLite (discovered, stats)
  multipost-orchestrator  JobState machine types
  multipost-publishers/
    youtube               API  — OAuth2+PKCE, resumable video upload
    wx-gzh                API  — stable_token, draft/add + freepublish
    douyin                CDP  — SCP-stage video + DOM.setFileInputFiles (video)
    toutiao               CDP  — 微头条 + article editor + images; + stats collector
    twitter               CDP  — tweet + images; + stats collector
  multipost-crawlers/
    toutiao, twitter      pwright network-listen → decode recommendation-feed JSON
  multipost-server        binary: tonic gRPC + axum OAuth-callback HTTP
  multipost-cli           binary: CLI client
```

## The three traits (all in `multipost-core`)

- **`Publisher`** (`publisher.rs`) — write side. `publish` / `confirm` / `delete`
  / `check_auth`. One impl per platform in `multipost-publishers/*`.
- **`Crawler`** (`discovery.rs`) — read side, *public* recommendation feed. Drives
  the `pwright` CLI's `network-listen` to capture feed XHR, decodes to
  `DiscoveredItem`. Impls in `multipost-crawlers/*`.
- **`StatsCollector`** (`stats.rs`) — read side, the account's *own* dashboard.
  Returns `AccountStats` + per-post `PostStats` (richer than the feed: 展现/阅读,
  income, followers). Impls live next to the publishers (reuse their CDP client).

gRPC services (see `multipost-proto/proto/`): `Tenants`, `Accounts`, `Media`,
`Posts`, `Crawl`, `Stats`. The server registers one `Publisher` / `Crawler` /
`StatsCollector` per platform in `AppState` (`server/src/main.rs`).

## Browser automation — the non-obvious parts

The cookie-auth platforms (Douyin, Toutiao, Twitter) drive a **real Chrome that
is already logged in**, addressed by a `cdp_url` stored in the account's
credentials. The Chrome is usually on **another machine** (a Chrome host on the
tailnet, or behind an SSH tunnel) — your process has no filesystem access to it.
Consequences agents trip over:

- **Uploading files to a remote Chrome:** `DOM.setFileInputFiles` takes a path on
  the *browser* host — useless here. Douyin works around it by SCP-staging the
  file first (its creds carry SSH info). Toutiao/Twitter have **no SSH**, so the
  image-upload helper `PageSession::upload_files_to_input` streams bytes over the
  CDP WebSocket and rebuilds a `File` in-page via `DataTransfer`.
- **Decode with `atob`, NOT `fetch("data:...")`:** x.com's CSP (`connect-src`)
  blocks fetching `data:` URLs, which silently fails the upload. The helper uses
  `atob` + `Uint8Array` + `Blob` — works on both Twitter and Toutiao.
- **Toutiao stats endpoints** (`publishers/toutiao/src/stats.rs`): account totals
  come from `GET /mp/fe_api/home/merge_v2`; per-post stats from the paginated
  `GET /api/feed/mp_provider/v1/` feed. Both are plain authenticated GETs run via
  CDP `Runtime.evaluate` (cookies ride along — no token signing). Gotchas: the
  feed's `offset` is a **numeric index** (0,20,40…), *not* the timestamp the
  response echoes; and it needs `visited_uid`, read from
  `localStorage["__tea_cache_tokens_1231"].user_unique_id`.
- **Twitter stats** (`publishers/twitter/src/stats.rs`): DOM-scrape of the
  profile — followers from the header links, per-tweet counts from each tweet's
  `[role=group]` aria-label (exact numbers; Twitter omits zero counts).
- **SPAs are slow / flaky.** Poll for the element you need with a generous
  deadline (Toutiao SPA settles in 15s+; the Twitter Chrome can sit on the splash
  20–30s). Don't assume a fresh tab is ready.

### Exploring a new browser flow

Per the shared rule: **explore with Playwright first.** Write a `uv run` script
(PEP 723 inline deps) that attaches to the live Chrome over CDP, dumps the DOM /
captures XHR, and confirms selectors + response shapes *before* writing Rust.
The `scripts/{youtube,wechat-mp,douyin,toutiao,twitter}/` prototypes are the
reference implementations and ad-hoc debug tools.

## Build, test, CI

```bash
cargo build --workspace                          # needs `protoc` on PATH (tonic)
cargo test --workspace                            # no DB required (rusqlite in-memory + JSON repos)
```

CI (`.github/workflows/ci.yml`, mirrors the Shuozeli convention) gates four jobs
— run them locally before pushing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
cargo test --workspace
```

- **No `#[allow(clippy::…)]` bypasses — fix the root cause.** The one documented
  exception is `result_large_err`, allowed in `multipost-server`'s `Cargo.toml`
  only, because every gRPC handler returns `Result<_, tonic::Status>` (a large
  framework type that can't be boxed).
- After pushing, verify CI with `gh run list --limit 1` then
  `gh run watch <id> --exit-status`; fix + re-push if red.

## Running it

```bash
# Server (dev: skip auth, bind tenant 0). Prod must NOT set DEV_NO_AUTH.
MULTIPOST_DEV_NO_AUTH=1 cargo run -p multipost-server -- --data-dir /tmp/mp

# Register a cookie-auth account (Chrome must already be logged in at cdp_url).
multipost accounts register-toutiao --cdp-url http://<chrome-host>:<port>
multipost accounts register-twitter --cdp-url http://<chrome-host>:<port> --handle <handle>

# Post (text / images / video). --image is repeatable; mutually exclusive with --video.
multipost post --to toutiao,twitter --description "…" --image a.png --image b.jpg

# Collect + read profile stats (timestamped snapshots; each collect appends a row).
multipost stats collect --platform toutiao --max-posts 100
multipost stats account --platform toutiao        # growth series, newest first
multipost stats posts   --platform toutiao         # latest per-post stats

# Crawl the public recommendation feed.
multipost crawl --platform twitter --duration 30
```

When surfacing server URLs, bind to / address via the Tailscale IP / MagicDNS
name (set `TAILSCALE_IP`), not `localhost` — see the personal infra defaults.

## Conventions / gotchas

- **Do NOT commit or push unless explicitly asked.** Do not amend or force-push
  unless asked. (CI-fix commits, when asked, are squashed into the original +
  force-pushed — never a trailing `style:`/`fix:` commit.)
- Storage is per-concern: JSON files for accounts/jobs/media/tenants; SQLite for
  `discovered` and `stats` (the latter keeps timestamped snapshots, not upserts).
- `Posts.Submit` blocks through `publish()` and returns at `Confirming`; a
  detached task finishes `confirm()` polling. Content-hash dedup (1h) guards
  retry storms.
- `Option<i64>` metrics cross the wire as `-1` = "platform didn't report it"
  (proto3 has no null); the CLI renders that as `—`.
- Use `pnpm` / `uv` (never `npm` / `pip`) for the Node / Python tooling.
