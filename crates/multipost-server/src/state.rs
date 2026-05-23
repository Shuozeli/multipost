//! Shared application state.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use multipost_core::{Platform, Publisher};
use multipost_orchestrator::JobState;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use multipost_publishers_youtube::OAuthCredentials;
use multipost_storage::accounts::AccountRepository;
use multipost_storage::jobs::JobRepository;
use multipost_storage::media::FileBackedMediaRepository;
use uuid::Uuid;

/// Per-platform OAuth client config, loaded from env at startup.
#[derive(Debug, Clone, Default)]
pub struct OAuthConfig {
    /// YouTube OAuth client. `None` if MULTIPOST_YOUTUBE_CLIENT_ID is unset.
    pub youtube: Option<OAuthCredentials>,
}

/// In-flight OAuth state.
#[derive(Debug, Clone)]
pub struct PendingAuth {
    pub pending_id: Uuid,
    pub state: String,
    pub user_id: Uuid,
    pub platform: Platform,
}

/// Internal event payload emitted on every job state transition. The
/// `Watch` RPC subscribes to a `broadcast::Receiver<JobEventInternal>`,
/// filters by `job_id`, and maps to the proto wire type.
#[derive(Debug, Clone)]
pub struct JobEventInternal {
    /// Job ID this event is about.
    pub job_id: Uuid,
    /// Tenant scope — Watch subscribers from a different tenant must not
    /// see this event even if they guess the job_id.
    pub tenant_id: Uuid,
    /// New state.
    pub state: JobState,
    /// Free-text annotation (transition reason, error message, etc.).
    pub detail: String,
    /// When the transition happened.
    pub at: DateTime<Utc>,
}

/// Channel capacity for the job-event bus. Old events are dropped on
/// overflow — slow consumers see `RecvError::Lagged` and Watch closes
/// their stream with ResourceExhausted.
pub const JOB_EVENT_BUS_CAPACITY: usize = 256;

/// Application state shared across request handlers.
pub struct AppState {
    pub accounts: Arc<dyn AccountRepository>,
    pub media: Arc<FileBackedMediaRepository>,
    pub media_dir: PathBuf,
    pub jobs: Arc<dyn JobRepository>,
    pub publishers: HashMap<Platform, Arc<dyn Publisher>>,
    pub http: reqwest::Client,
    pub oauth: OAuthConfig,
    pub pending: Mutex<HashMap<Uuid, PendingAuth>>,
    /// In-flight confirm-poll tasks, keyed by job_id. Submit spawns into
    /// this map; the task self-removes its entry on completion; shutdown
    /// drains the map with a deadline (see §22.7 of design.md).
    pub confirm_tasks: Mutex<HashMap<Uuid, JoinHandle<()>>>,
    /// Broadcast channel for job state transitions. `Posts.Watch` clients
    /// subscribe via `events.subscribe()`. Senders just call `events.send(_)`;
    /// when there are zero subscribers the message is dropped silently.
    pub events: broadcast::Sender<JobEventInternal>,
}

impl AppState {
    /// Construct shared state.
    pub fn new(
        accounts: Arc<dyn AccountRepository>,
        media: Arc<FileBackedMediaRepository>,
        media_dir: PathBuf,
        jobs: Arc<dyn JobRepository>,
        publishers: HashMap<Platform, Arc<dyn Publisher>>,
        oauth: OAuthConfig,
    ) -> Self {
        Self {
            accounts,
            media,
            media_dir,
            jobs,
            publishers,
            http: reqwest::Client::new(),
            oauth,
            pending: Mutex::new(HashMap::new()),
            confirm_tasks: Mutex::new(HashMap::new()),
            events: broadcast::channel(JOB_EVENT_BUS_CAPACITY).0,
        }
    }

    /// Publish a job-state event to the bus. No-op if there are zero
    /// subscribers (`broadcast::Sender::send` returns `Err` in that case
    /// which we deliberately ignore — events are best-effort).
    pub fn emit_event(&self, ev: JobEventInternal) {
        let _ = self.events.send(ev);
    }

    /// Spawn a confirm-poll task and register its JoinHandle so graceful
    /// shutdown can wait for it. The task self-removes its entry from
    /// `confirm_tasks` on completion so the map only holds in-flight work.
    pub fn spawn_confirm(self: &Arc<Self>, job_id: Uuid, fut: impl Future<Output = ()> + Send + 'static) {
        let me = Arc::clone(self);
        let handle = tokio::spawn(async move {
            fut.await;
            if let Ok(mut t) = me.confirm_tasks.lock() {
                t.remove(&job_id);
            }
        });
        if let Ok(mut t) = self.confirm_tasks.lock() {
            t.insert(job_id, handle);
        }
    }
}
