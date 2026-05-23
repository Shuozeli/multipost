//! `Posts` gRPC service implementation + synchronous orchestrator.
//!
//! Phase 1c version: `Submit` runs the publish pipeline inline before
//! returning. No queue, no retries. Phase 5 replaces this with a real
//! async orchestrator backed by the JobRepository.

use std::sync::Arc;

use chrono::Utc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use multipost_core::{
    AuthStatus, ConfirmStatus, Content, ContentKind, MediaPayload, PublishContext, PublishError,
    PublishHandle, Visibility,
};
use multipost_orchestrator::JobState;
use multipost_proto::common::JobState as ProtoJobState;
use multipost_proto::posts::posts_server::Posts as PostsTrait;
use multipost_proto::posts::{
    Content as ProtoContent, GetJobRequest, Job as ProtoJob, JobEvent, JobRef, ListJobsRequest,
    ListJobsResponse,
    SubmitRequest, SubmitResponse,
};
use multipost_storage::accounts::AccountRecord;
use multipost_storage::jobs::JobRecord;
use multipost_storage::media::MediaRepository;

use crate::auth::tenant_id_from_request;
use crate::state::{AppState, JobEventInternal};

const MAX_CONFIRM_POLLS: u32 = 12;
const CONFIRM_DELAY_SECS: u64 = 5;
/// Server-side ceiling on `GetJobRequest.wait_seconds`. Callers asking
/// for more get clamped — a single RPC shouldn't tie up a connection
/// longer than this.
const MAX_LONG_POLL_SECS: u64 = 60;
/// §22.5: how far back content-hash dedup looks. A second Submit with
/// identical content within this window returns the existing Job
/// instead of double-posting.
const DEDUP_WINDOW_SECS: u64 = 3600;

pub struct PostsService {
    state: Arc<AppState>,
}

impl PostsService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

fn job_state_to_proto(s: JobState) -> i32 {
    let v: ProtoJobState = match s {
        JobState::Queued => ProtoJobState::Queued,
        JobState::Validating => ProtoJobState::Validating,
        JobState::Uploading => ProtoJobState::Uploading,
        JobState::Submitting => ProtoJobState::Submitting,
        JobState::Confirming => ProtoJobState::Confirming,
        JobState::Confirmed => ProtoJobState::Confirmed,
        JobState::Failed => ProtoJobState::Failed,
        JobState::Cancelled => ProtoJobState::Cancelled,
    };
    v as i32
}

fn record_to_proto(r: &JobRecord) -> ProtoJob {
    ProtoJob {
        id: r.id.to_string(),
        content_id: String::new(), // denormalized: content lives inside the row in Phase 1c
        account_id: r.account_id.to_string(),
        state: job_state_to_proto(r.state),
        attempts: r.attempts,
        last_error: r.last_error.clone().unwrap_or_default(),
        external_id: r.external_id.clone().unwrap_or_default(),
        permalink: r.permalink.clone().unwrap_or_default(),
        created_at: Some(prost_types::Timestamp {
            seconds: r.created_at.timestamp(),
            nanos: 0,
        }),
        updated_at: Some(prost_types::Timestamp {
            seconds: r.updated_at.timestamp(),
            nanos: 0,
        }),
    }
}

fn proto_visibility(v: i32) -> Visibility {
    use multipost_proto::common::Visibility as ProtoVis;
    match ProtoVis::try_from(v).unwrap_or(ProtoVis::Unspecified) {
        ProtoVis::Public => Visibility::Public,
        ProtoVis::Followers => Visibility::Followers,
        ProtoVis::Unlisted => Visibility::Unlisted,
        ProtoVis::Private | ProtoVis::Unspecified => Visibility::Private,
    }
}

fn infer_content_kind(media_count: usize, mime: Option<&str>) -> ContentKind {
    if media_count == 0 {
        ContentKind::Text
    } else if mime.map_or(false, |m| m.starts_with("video/")) {
        ContentKind::LongVideo
    } else {
        ContentKind::Image
    }
}

fn build_content_from_proto(
    user_id: Uuid,
    p: ProtoContent,
    media_count: usize,
    mime_hint: Option<&str>,
) -> Content {
    let media_ids: Vec<Uuid> = p
        .media_ids
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    Content {
        id: Uuid::new_v4(),
        user_id,
        kind: infer_content_kind(media_count, mime_hint),
        text: p.text,
        hashtags: p.hashtags,
        media: media_ids
            .into_iter()
            .map(multipost_core::MediaRef)
            .collect(),
        schedule_at: p.schedule_at.and_then(|ts| {
            chrono::DateTime::from_timestamp(ts.seconds, ts.nanos as u32)
        }),
        visibility: proto_visibility(p.visibility),
        overrides: Default::default(),
        created_at: Utc::now(),
    }
}

#[tonic::async_trait]
impl PostsTrait for PostsService {
    async fn submit(
        &self,
        req: Request<SubmitRequest>,
    ) -> Result<Response<SubmitResponse>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let r = req.into_inner();
        let proto_content = r
            .content
            .ok_or_else(|| Status::invalid_argument("missing content"))?;
        if r.account_ids.is_empty() {
            return Err(Status::invalid_argument("at least one account_id required"));
        }

        // Pre-resolve media once; same payloads are reused across accounts.
        let media_ids: Vec<Uuid> = proto_content
            .media_ids
            .iter()
            .map(|s| s.parse::<Uuid>())
            .collect::<Result<_, _>>()
            .map_err(|_| Status::invalid_argument("media_id not a UUID"))?;
        let mut media_payloads = Vec::with_capacity(media_ids.len());
        let mut media_sha256 = Vec::with_capacity(media_ids.len());
        let mut first_mime: Option<String> = None;
        for mid in &media_ids {
            let rec = self
                .state
                .media
                .get(tenant_id, *mid)
                .await
                .map_err(|e| Status::internal(e.to_string()))?
                .ok_or_else(|| Status::not_found(format!("media {mid} not found")))?;
            let bytes = tokio::fs::read(&rec.path)
                .await
                .map_err(|e| Status::internal(format!("read media: {e}")))?;
            if first_mime.is_none() {
                first_mime = Some(rec.mime_type.clone());
            }
            media_sha256.push(rec.sha256.clone());
            media_payloads.push(MediaPayload {
                filename: rec.filename,
                mime_type: rec.mime_type,
                bytes,
            });
        }

        let mut content = build_content_from_proto(
            tenant_id,
            proto_content.clone(),
            media_payloads.len(),
            first_mime.as_deref(),
        );
        content.media = media_ids.iter().copied().map(multipost_core::MediaRef).collect();

        let mut out = Vec::with_capacity(r.account_ids.len());
        for acc_str in r.account_ids {
            let account_id: Uuid = acc_str
                .parse()
                .map_err(|_| Status::invalid_argument("account_id not a UUID"))?;

            // §22.5 dedup: if a non-Failed job for this (tenant, hash)
            // already exists within DEDUP_WINDOW, short-circuit and return
            // it. Caller crash-looping Submit doesn't double-post.
            let hash = compute_content_hash(account_id, &content, &media_sha256);
            let cutoff = Utc::now() - chrono::Duration::seconds(DEDUP_WINDOW_SECS as i64);
            if let Some(existing) = self
                .state
                .jobs
                .find_recent_by_hash(tenant_id, &hash, cutoff)
                .await
                .map_err(|e| Status::internal(format!("dedup lookup: {e}")))?
            {
                tracing::info!(
                    job_id = %existing.id,
                    account_id = %account_id,
                    "Submit deduplicated against existing job (content-hash match within DEDUP_WINDOW)"
                );
                out.push(record_to_proto(&existing));
                continue;
            }

            let mut record = self
                .create_job_record(tenant_id, account_id, &content, &media_ids, hash)
                .await?;
            // Sync portion: validate → refresh creds → check_auth → publish.
            // Returns Some(handoff) on success (job is now Confirming).
            if let Some(handoff) = self
                .run_to_confirming(tenant_id, &mut record, &content, &media_payloads)
                .await
            {
                // Async portion: confirm-poll runs in a detached task. The
                // task self-removes from `state.confirm_tasks` on completion
                // so the map only holds in-flight work.
                let state = self.state.clone();
                let job_id = record.id;
                state.spawn_confirm(
                    job_id,
                    poll_confirm_until_terminal(state.clone(), tenant_id, job_id, handoff),
                );
            }
            out.push(record_to_proto(&record));
        }

        Ok(Response::new(SubmitResponse { jobs: out }))
    }

    async fn get_job(&self, req: Request<GetJobRequest>) -> Result<Response<ProtoJob>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let r = req.into_inner();
        let id: Uuid = r
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("id not a UUID"))?;
        // Cap server-side so a misbehaving caller can't tie up a
        // connection forever.
        let wait = r.wait_seconds.clamp(0, MAX_LONG_POLL_SECS as i32) as u64;

        let job = self
            .state
            .jobs
            .get(tenant_id, id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("no such job"))?;

        // Fast paths: immediate return when caller didn't ask to wait
        // OR the job is already terminal.
        if wait == 0 || is_terminal(job.state) {
            return Ok(Response::new(record_to_proto(&job)));
        }

        // Long-poll: subscribe to the bus, wait for ANY event for this
        // (job_id, tenant_id), then reload and return. Times out at
        // `wait_seconds` and returns the current state.
        let mut rx = self.state.events.subscribe();
        let deadline = std::time::Duration::from_secs(wait);
        let waited = tokio::time::timeout(deadline, async {
            loop {
                match rx.recv().await {
                    Ok(ev) if ev.job_id == id && ev.tenant_id == tenant_id => return,
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        })
        .await
        .is_ok();
        tracing::debug!(%id, woke_on_event = waited, "long-poll resolved");

        // Reload so the response reflects the latest persisted state,
        // regardless of whether we woke on the bus or hit the deadline.
        let job = self
            .state
            .jobs
            .get(tenant_id, id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("no such job"))?;
        Ok(Response::new(record_to_proto(&job)))
    }

    async fn list_jobs(
        &self,
        req: Request<ListJobsRequest>,
    ) -> Result<Response<ListJobsResponse>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let r = req.into_inner();
        let limit = if r.page_size <= 0 {
            50
        } else {
            r.page_size as usize
        };
        let jobs = self
            .state
            .jobs
            .list(tenant_id, limit)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(ListJobsResponse {
            jobs: jobs.iter().map(record_to_proto).collect(),
            next_page_token: String::new(),
        }))
    }

    async fn cancel(&self, req: Request<JobRef>) -> Result<Response<ProtoJob>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let id: Uuid = req
            .into_inner()
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("id is not a UUID"))?;
        let mut job = self
            .state
            .jobs
            .get(tenant_id, id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("no such job"))?;

        if matches!(
            job.state,
            JobState::Confirmed | JobState::Confirming | JobState::Submitting
        ) {
            let external_id = job.external_id.clone().ok_or_else(|| {
                Status::failed_precondition("job has no external_id; nothing to delete")
            })?;
            let account = self
                .state
                .accounts
                .get(tenant_id, job.account_id)
                .await
                .map_err(|e| Status::internal(e.to_string()))?
                .ok_or_else(|| Status::not_found("account no longer exists"))?;
            let publisher = self
                .state
                .publishers
                .get(&account.platform)
                .cloned()
                .ok_or_else(|| {
                    Status::internal(format!(
                        "no publisher registered for {:?}",
                        account.platform
                    ))
                })?;
            let ctx = PublishContext {
                account_id: account.id,
                user_id: account.user_id,
                credentials: &account.credentials,
                media: vec![],
            };
            let handle = PublishHandle {
                external_id,
                permalink: job.permalink.clone(),
            };
            publisher
                .delete(&ctx, &handle)
                .await
                .map_err(|e| Status::internal(format!("delete: {e}")))?;
        }

        job.state = JobState::Cancelled;
        job.updated_at = Utc::now();
        self.state
            .jobs
            .update(job.clone())
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        self.state.emit_event(JobEventInternal {
            job_id: job.id,
            tenant_id,
            state: JobState::Cancelled,
            detail: "cancelled".to_string(),
            at: job.updated_at,
        });
        Ok(Response::new(record_to_proto(&job)))
    }

    type WatchStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<JobEvent, Status>> + Send + 'static>,
    >;

    /// Stream `JobEvent`s for a single job until it reaches a terminal
    /// state (Confirmed | Failed | Cancelled). First event is always the
    /// job's *current* state — handles the race where the job already
    /// transitioned before the caller subscribed.
    async fn watch(&self, req: Request<JobRef>) -> Result<Response<Self::WatchStream>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let id: Uuid = req
            .into_inner()
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("id not a UUID"))?;
        let job = self
            .state
            .jobs
            .get(tenant_id, id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("no such job"))?;

        let mut rx = self.state.events.subscribe();
        let (tx, output_rx) = tokio::sync::mpsc::channel::<Result<JobEvent, Status>>(32);

        // First event: current state (closes the race where the job
        // already terminated before subscribe).
        let initial = job_record_to_event(&job, "current state");
        let initial_state = job.state;
        if tx.send(Ok(initial)).await.is_err() {
            // Caller dropped before receiving the first event — bail.
            return Ok(Response::new(empty_stream()));
        }
        if is_terminal(initial_state) {
            return Ok(Response::new(Box::pin(
                tokio_stream::wrappers::ReceiverStream::new(output_rx),
            )));
        }

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) if ev.job_id == id && ev.tenant_id == tenant_id => {
                        let proto = job_event_internal_to_proto(&ev);
                        if tx.send(Ok(proto)).await.is_err() {
                            return; // caller hung up
                        }
                        if is_terminal(ev.state) {
                            return;
                        }
                    }
                    Ok(_) => continue, // some other job/tenant
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        let _ = tx
                            .send(Err(Status::resource_exhausted(format!(
                                "watch event bus lagged: {skipped} events dropped — re-subscribe"
                            ))))
                            .await;
                        return;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        Ok(Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(output_rx),
        )))
    }
}

impl PostsService {
    async fn create_job_record(
        &self,
        tenant_id: Uuid,
        account_id: Uuid,
        content: &Content,
        media_ids: &[Uuid],
        content_hash: String,
    ) -> Result<JobRecord, Status> {
        let now = Utc::now();
        let record = JobRecord {
            id: Uuid::new_v4(),
            user_id: tenant_id,
            content: serde_json::to_value(content)
                .map_err(|e| Status::internal(format!("serialize content: {e}")))?,
            account_id,
            media_ids: media_ids.to_vec(),
            state: JobState::Queued,
            attempts: 0,
            last_error: None,
            external_id: None,
            permalink: None,
            content_hash: Some(content_hash),
            created_at: now,
            updated_at: now,
        };
        self.state
            .jobs
            .insert(record.clone())
            .await
            .map_err(|e| Status::internal(format!("persist job: {e}")))?;
        Ok(record)
    }

    /// Drive a job from Queued through Submitting → publish() → Confirming.
    /// On success, persists the new state and returns a handoff so the
    /// caller can spawn the background confirm-poll. On failure, marks
    /// the job Failed inline and returns None.
    async fn run_to_confirming(
        &self,
        tenant_id: Uuid,
        job: &mut JobRecord,
        content: &Content,
        media: &[MediaPayload],
    ) -> Option<ConfirmHandoff> {
        let user_id = tenant_id;
        let account_id = job.account_id;

        let acc = match self.state.accounts.get(user_id, account_id).await {
            Ok(Some(a)) => a,
            Ok(None) => {
                self.fail(job, "account not found").await;
                return None;
            }
            Err(e) => {
                self.fail(job, &format!("load account: {e}")).await;
                return None;
            }
        };

        let publisher = match self.state.publishers.get(&acc.platform) {
            Some(p) => p.clone(),
            None => {
                self.fail(job, &format!("no publisher for {:?}", acc.platform))
                    .await;
                return None;
            }
        };

        self.transition(job, JobState::Validating).await;
        let caps = publisher.capabilities();
        if let Some(max) = caps.max_text_chars {
            if content.text.chars().count() > max {
                self.fail(job, &format!("text exceeds {max} chars")).await;
                return None;
            }
        }
        if matches!(content.kind, ContentKind::LongVideo | ContentKind::ShortVideo)
            && !caps.video_supported
        {
            self.fail(job, "platform does not support video").await;
            return None;
        }

        let credentials = match publisher.refresh_credentials(&acc.credentials).await {
            Ok(Some(new_creds)) => {
                let mut updated = acc.clone();
                updated.credentials = new_creds.clone();
                updated.last_used_at = Utc::now();
                if let Err(e) = self.state.accounts.upsert(updated).await {
                    self.fail(job, &format!("persist refreshed creds: {e}")).await;
                    return None;
                }
                tracing::info!(account_id = %account_id, "refreshed credentials");
                new_creds
            }
            Ok(None) => acc.credentials.clone(),
            Err(PublishError::AuthExpired(_)) => {
                self.fail(job, "auth expired").await;
                return None;
            }
            Err(e) => {
                self.fail(job, &format!("refresh: {e}")).await;
                return None;
            }
        };

        let mut ctx = PublishContext {
            account_id,
            user_id,
            credentials: &credentials,
            media: media.to_vec(),
        };

        match publisher.check_auth(&ctx).await {
            Ok(AuthStatus::Active) => {}
            Ok(AuthStatus::Expired) => {
                self.fail(job, "auth expired (pre-flight)").await;
                return None;
            }
            Ok(other) => {
                self.fail(job, &format!("auth not active: {:?}", other)).await;
                return None;
            }
            Err(e) => {
                self.fail(job, &format!("check_auth: {e}")).await;
                return None;
            }
        }

        self.transition(job, JobState::Uploading).await;
        self.transition(job, JobState::Submitting).await;
        let handle: PublishHandle = match publisher.publish(&mut ctx, content).await {
            Ok(h) => h,
            Err(e) => {
                self.fail(job, &format!("publish: {e}")).await;
                return None;
            }
        };
        job.external_id = Some(handle.external_id.clone());
        job.permalink = handle.permalink.clone();
        self.transition(job, JobState::Confirming).await;

        Some(ConfirmHandoff {
            credentials,
            handle,
            account_id,
        })
    }

    async fn transition(&self, job: &mut JobRecord, to: JobState) {
        tracing::info!(job_id = %job.id, from = ?job.state, to = ?to, "job transition");
        job.state = to;
        job.updated_at = Utc::now();
        let _ = self.state.jobs.update(job.clone()).await;
        self.state.emit_event(JobEventInternal {
            job_id: job.id,
            tenant_id: job.user_id,
            state: job.state,
            detail: format!("{:?}", to),
            at: job.updated_at,
        });
    }

    async fn fail(&self, job: &mut JobRecord, msg: &str) {
        tracing::warn!(job_id = %job.id, msg, "job failed");
        job.state = JobState::Failed;
        job.last_error = Some(msg.to_string());
        job.attempts += 1;
        job.updated_at = Utc::now();
        let _ = self.state.jobs.update(job.clone()).await;
        self.state.emit_event(JobEventInternal {
            job_id: job.id,
            tenant_id: job.user_id,
            state: JobState::Failed,
            detail: msg.to_string(),
            at: job.updated_at,
        });
    }
}

// Help dead_code analysis: AccountRecord is part of the storage API surface;
// we use it transitively via the repos. Suppress here without #[allow].
#[doc(hidden)]
pub fn _link_account_record(_: &AccountRecord) {}

fn is_terminal(s: JobState) -> bool {
    matches!(s, JobState::Confirmed | JobState::Failed | JobState::Cancelled)
}

/// §22.5 content-hash. Deterministic across processes — we feed the
/// dedup inputs into sha256 with fixed delimiters so a repeated
/// identical Submit produces the same hash. Format: `"sha256:<hex>"`
/// (matches the tenant-key convention).
///
/// Important: we hash each media's **content-SHA256** (which is
/// content-addressed and stable across re-uploads of identical bytes),
/// not the media's UUID. Otherwise a caller doing
/// `media.Upload(video.mp4)` twice would get two different UUIDs and
/// the dedup would never fire — defeating the protection against
/// "user accidentally clicked Submit twice".
fn compute_content_hash(account_id: Uuid, content: &Content, media_sha256: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"acct=");
    hasher.update(account_id.as_bytes());
    hasher.update(b"\ntext=");
    hasher.update(content.text.as_bytes());
    hasher.update(b"\nvis=");
    hasher.update(format!("{:?}", content.visibility).as_bytes());
    hasher.update(b"\nsched=");
    hasher.update(
        content
            .schedule_at
            .map(|t| t.to_rfc3339())
            .unwrap_or_default()
            .as_bytes(),
    );
    hasher.update(b"\nmedia=");
    for s in media_sha256 {
        hasher.update(s.as_bytes());
        hasher.update(b",");
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn job_event_internal_to_proto(ev: &JobEventInternal) -> JobEvent {
    JobEvent {
        job_id: ev.job_id.to_string(),
        state: job_state_to_proto(ev.state),
        detail: ev.detail.clone(),
        at: Some(prost_types::Timestamp {
            seconds: ev.at.timestamp(),
            nanos: 0,
        }),
    }
}

fn job_record_to_event(job: &JobRecord, detail: &str) -> JobEvent {
    JobEvent {
        job_id: job.id.to_string(),
        state: job_state_to_proto(job.state),
        detail: detail.to_string(),
        at: Some(prost_types::Timestamp {
            seconds: job.updated_at.timestamp(),
            nanos: 0,
        }),
    }
}

/// An empty Watch stream — used when the caller hangs up before the
/// first event lands.
fn empty_stream() -> std::pin::Pin<
    Box<dyn tokio_stream::Stream<Item = Result<JobEvent, Status>> + Send + 'static>,
> {
    Box::pin(tokio_stream::empty())
}

/// What `run_to_confirming` returns when the job successfully reached
/// Confirming and a background poll task should be spawned.
pub(crate) struct ConfirmHandoff {
    /// The credentials value publish() used (post-refresh). Owned so the
    /// background task can re-build a PublishContext with a borrow.
    pub credentials: serde_json::Value,
    /// The PublishHandle returned from publish().
    pub handle: PublishHandle,
    /// Account ID — used to re-load the account in the background task
    /// (the platform may have rotated credentials again between submit
    /// and the first confirm() call).
    pub account_id: Uuid,
}

/// Run `Publisher::confirm` in a loop until Confirmed | Failed (or the
/// poll budget is exhausted, in which case the job stays in Confirming
/// for a future restart-recovery to re-attach). Lives in a detached
/// `tokio::spawn` task — caller wires it via `AppState::spawn_confirm`.
pub(crate) async fn poll_confirm_until_terminal(
    state: Arc<AppState>,
    tenant_id: Uuid,
    job_id: Uuid,
    handoff: ConfirmHandoff,
) {
    let ConfirmHandoff {
        credentials,
        handle,
        account_id,
    } = handoff;
    let account = match state.accounts.get(tenant_id, account_id).await {
        Ok(Some(a)) => a,
        _ => {
            tracing::warn!(%job_id, %account_id, "background confirm: account disappeared");
            return;
        }
    };
    let publisher = match state.publishers.get(&account.platform) {
        Some(p) => p.clone(),
        None => {
            tracing::warn!(
                %job_id,
                platform = ?account.platform,
                "background confirm: publisher gone"
            );
            return;
        }
    };
    let ctx = PublishContext {
        account_id,
        user_id: tenant_id,
        credentials: &credentials,
        media: vec![],
    };

    for attempt in 1..=MAX_CONFIRM_POLLS {
        match publisher.confirm(&ctx, &handle).await {
            Ok(ConfirmStatus::Confirmed { permalink }) => {
                if let Ok(Some(mut job)) = state.jobs.get(tenant_id, job_id).await {
                    if let Some(p) = permalink {
                        job.permalink = Some(p);
                    }
                    job.state = JobState::Confirmed;
                    job.updated_at = Utc::now();
                    let _ = state.jobs.update(job.clone()).await;
                    tracing::info!(%job_id, from = ?JobState::Confirming, to = ?JobState::Confirmed, "job transition (bg)");
                    state.emit_event(JobEventInternal {
                        job_id: job.id,
                        tenant_id,
                        state: JobState::Confirmed,
                        detail: "confirmed".to_string(),
                        at: job.updated_at,
                    });
                }
                return;
            }
            Ok(ConfirmStatus::Pending) => {
                tracing::debug!(%job_id, attempt, "still pending (bg)");
                tokio::time::sleep(std::time::Duration::from_secs(CONFIRM_DELAY_SECS)).await;
            }
            Err(e) => {
                if let Ok(Some(mut job)) = state.jobs.get(tenant_id, job_id).await {
                    job.state = JobState::Failed;
                    job.last_error = Some(format!("confirm: {e}"));
                    job.attempts += 1;
                    job.updated_at = Utc::now();
                    let _ = state.jobs.update(job.clone()).await;
                    tracing::warn!(%job_id, error = %e, "job failed (bg confirm)");
                    state.emit_event(JobEventInternal {
                        job_id: job.id,
                        tenant_id,
                        state: JobState::Failed,
                        detail: format!("confirm: {e}"),
                        at: job.updated_at,
                    });
                }
                return;
            }
        }
    }
    // Poll budget exhausted — leave the job in Confirming. A later GetJob
    // call will see it, or the startup-recovery scan can re-attach if the
    // server restarts.
    tracing::info!(%job_id, "confirm poll budget exhausted; job left in Confirming");
}
