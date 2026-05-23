# multipost

A pure-Rust gRPC server for **posting content to social media platforms automatically**.

> One sentence: submit a piece of content once → it lands on every platform you've connected.

## Status

Phase 5 thin-executor design is implemented and exercised end-to-end. The lib supports posting + confirming + deleting on the four target platforms today. The server is a multi-tenant API; downstream callers submit posts via gRPC and either long-poll `GetJob` or open a streaming `Watch` to learn when a job lands.

| Platform | Auth | Publish | Confirm | Delete | Tested live |
|---|---|---|---|---|---|
| **YouTube** | OAuth 2.0 + PKCE | Video upload (Data API v3) | Polling | API delete | ✓ |
| **WeChat MP** (公众号) | `stable_token` (appid + secret) | Article draft + `freepublish/submit` | `freepublish/get` (partial) | API delete | ✓ draft path |
| **Douyin** (抖音) | Chrome profile cookies | Browser-automated video upload | Polls manage page | Clicks 删除作品 | ✓ |
| **Toutiao** (头条号) | Chrome profile cookies | Browser-automated article editor | Auto-saved on type | Drafts UI 删除 | ✓ |

WeChat MP individual subscription accounts: `freepublish/submit` is gated by Tencent's 48001 — drafts land, final publish has to be clicked in MP admin.

## Crate layout

```
multipost/
├── Cargo.toml          workspace
└── crates/
    ├── multipost-core         shared types: Publisher trait, Content, Capabilities
    ├── multipost-proto        .proto files + tonic-generated bindings
    ├── multipost-storage      file-backed repositories (accounts, jobs, media, tenants)
    ├── multipost-orchestrator job state machine types
    ├── multipost-publishers/
    │   ├── youtube            API   (OAuth + resumable upload)
    │   ├── wx-gzh             API   (stable_token + draft/add + freepublish)
    │   ├── douyin             CDP   (SCP staging + DOM.setFileInputFiles)
    │   └── toutiao            CDP   (execCommand insertText into the editor)
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

# 4. Post.
./target/release/multipost post --to wx-gzh \
  --title "Hello world" --description "..."

# 5. Watch the job to terminal.
./target/release/multipost watch <job-id>
```

For local development, set `MULTIPOST_DEV_NO_AUTH=1` on the server to bypass the API-key check; all requests are then bound to `tenant_id=00000000-0000-0000-0000-000000000000`. Production deploys must not set it.

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
