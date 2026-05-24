//! `Accounts` gRPC service implementation.

use std::sync::Arc;

use chrono::Utc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use multipost_core::{AuthStatus, Platform};
use multipost_proto::accounts::accounts_server::Accounts;
use multipost_proto::accounts::{
    Account, CheckAuthRequest, CheckAuthResponse, CompleteAuthRequest, GetAccountRequest,
    ListAccountsRequest, ListAccountsResponse, RegisterDeveloperRequest, RevokeRequest,
    StartAuthRequest, StartAuthResponse, complete_auth_request, start_auth_response,
};
use multipost_proto::common::Platform as ProtoPlatform;
use multipost_publishers_douyin::DouyinCredentials;
use multipost_publishers_toutiao::ToutiaoCredentials;
use multipost_publishers_twitter::TwitterCredentials;
use multipost_publishers_wx_gzh::{check_account_info, ensure_access_token, WxGzhCredentials};
use multipost_publishers_youtube::{exchange_code, start_oauth_url};
use multipost_storage::accounts::AccountRecord;

use crate::auth::tenant_id_from_request;
use crate::state::{AppState, PendingAuth};

/// Account service backed by the shared `AppState`.
pub struct AccountsService {
    state: Arc<AppState>,
}

impl AccountsService {
    /// Construct a service from shared state.
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

fn random_state_token() -> String {
    // 32 bytes of randomness, base64-url encoded. Stdlib only.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{:032x}{:08x}", nanos, pid)
}

fn proto_to_core(p: ProtoPlatform) -> Result<Platform, Status> {
    match p {
        ProtoPlatform::Youtube => Ok(Platform::YouTube),
        ProtoPlatform::WxGzh => Ok(Platform::WxGzh),
        ProtoPlatform::Twitter => Ok(Platform::Twitter),
        ProtoPlatform::Douyin => Ok(Platform::Douyin),
        ProtoPlatform::Toutiao => Ok(Platform::Toutiao),
        ProtoPlatform::Unspecified => Err(Status::invalid_argument("platform unspecified")),
    }
}

fn core_to_proto(p: Platform) -> i32 {
    let v: ProtoPlatform = match p {
        Platform::YouTube => ProtoPlatform::Youtube,
        Platform::WxGzh => ProtoPlatform::WxGzh,
        Platform::Twitter => ProtoPlatform::Twitter,
        Platform::Douyin => ProtoPlatform::Douyin,
        Platform::Toutiao => ProtoPlatform::Toutiao,
    };
    v as i32
}

fn record_to_proto(r: &AccountRecord) -> Account {
    let auth = match r.auth_status {
        AuthStatus::Active => multipost_proto::common::AuthStatus::Active,
        AuthStatus::Expired => multipost_proto::common::AuthStatus::Expired,
        AuthStatus::Revoked => multipost_proto::common::AuthStatus::Revoked,
        AuthStatus::Pending => multipost_proto::common::AuthStatus::Pending,
    };
    Account {
        id: r.id.to_string(),
        platform: core_to_proto(r.platform),
        display_name: r.display_name.clone(),
        external_id: r.external_id.clone(),
        auth_status: auth as i32,
        created_at: Some(prost_types::Timestamp {
            seconds: r.created_at.timestamp(),
            nanos: 0,
        }),
        last_used_at: Some(prost_types::Timestamp {
            seconds: r.last_used_at.timestamp(),
            nanos: 0,
        }),
    }
}

#[tonic::async_trait]
impl Accounts for AccountsService {
    async fn start_auth(
        &self,
        req: Request<StartAuthRequest>,
    ) -> Result<Response<StartAuthResponse>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let r = req.into_inner();
        let platform = proto_to_core(
            ProtoPlatform::try_from(r.platform).map_err(|_| Status::invalid_argument("bad platform"))?
        )?;

        let pending_id = Uuid::new_v4();
        let state_token = random_state_token();

        match platform {
            Platform::YouTube => {
                let creds = self
                    .state
                    .oauth
                    .youtube
                    .as_ref()
                    .ok_or_else(|| Status::failed_precondition(
                        "YouTube OAuth not configured: set MULTIPOST_YOUTUBE_CLIENT_ID etc."
                    ))?;
                let url = start_oauth_url(creds, &state_token)
                    .map_err(|e| Status::internal(format!("build oauth url: {e}")))?;

                // Stash pending auth so CompleteAuth can find it.
                tracing::info!(
                    %pending_id,
                    state = %state_token,
                    platform = ?platform,
                    "started oauth flow",
                );
                self.state
                    .pending
                    .lock()
                    .expect("pending mutex poisoned")
                    .insert(
                        pending_id,
                        PendingAuth {
                            pending_id,
                            state: state_token,
                            user_id: tenant_id,
                            platform,
                        },
                    );

                Ok(Response::new(StartAuthResponse {
                    pending_auth_id: pending_id.to_string(),
                    method: Some(start_auth_response::Method::AuthUrl(url.to_string())),
                }))
            }
            Platform::WxGzh | Platform::Twitter | Platform::Douyin | Platform::Toutiao => {
                Err(Status::unimplemented(format!(
                    "{} StartAuth lands in a later phase",
                    platform.as_str()
                )))
            }
        }
    }

    async fn complete_auth(
        &self,
        req: Request<CompleteAuthRequest>,
    ) -> Result<Response<Account>, Status> {
        let r = req.into_inner();
        let pending_id: Uuid = r
            .pending_auth_id
            .parse()
            .map_err(|_| Status::invalid_argument("pending_auth_id is not a UUID"))?;

        // Look up + remove the pending entry atomically.
        let pending = {
            let mut p = self.state.pending.lock().expect("pending mutex poisoned");
            p.remove(&pending_id)
                .ok_or_else(|| Status::not_found("no pending auth with that id"))?
        };
        tracing::info!(
            pending_id = %pending.pending_id,
            state = %pending.state,
            platform = ?pending.platform,
            "completing oauth flow",
        );

        let code = match r.completion {
            Some(complete_auth_request::Completion::Code(c)) => c,
            Some(complete_auth_request::Completion::BrowserLoggedIn(_)) => {
                return Err(Status::unimplemented("browser-login completion is for Phase 3"));
            }
            None => return Err(Status::invalid_argument("missing completion oneof")),
        };

        match pending.platform {
            Platform::YouTube => {
                let creds = self
                    .state
                    .oauth
                    .youtube
                    .as_ref()
                    .ok_or_else(|| Status::failed_precondition("YouTube OAuth not configured"))?;
                let tokens = exchange_code(&self.state.http, creds, &code)
                    .await
                    .map_err(|e| Status::internal(format!("token exchange: {e}")))?;

                // Pull channel info to populate display_name + external_id.
                let chan = self
                    .state
                    .http
                    .get(format!("{}/channels", multipost_publishers_youtube::API_BASE))
                    .query(&[("part", "snippet"), ("mine", "true")])
                    .bearer_auth(&tokens.access_token)
                    .send()
                    .await
                    .map_err(|e| Status::internal(format!("youtube /channels: {e}")))?;
                let chan_json: serde_json::Value = chan
                    .json()
                    .await
                    .map_err(|e| Status::internal(format!("parse /channels: {e}")))?;
                let first = chan_json
                    .get("items")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .cloned();
                let (external_id, display_name) = match first {
                    Some(c) => {
                        let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let title = c
                            .get("snippet")
                            .and_then(|s| s.get("title"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("(unknown)")
                            .to_string();
                        (id, title)
                    }
                    None => (String::new(), "(no channel)".to_string()),
                };

                let now = Utc::now();
                let record = AccountRecord {
                    id: Uuid::new_v4(),
                    user_id: pending.user_id,
                    platform: Platform::YouTube,
                    display_name,
                    external_id,
                    auth_status: AuthStatus::Active,
                    credentials: serde_json::json!({
                        "access_token": tokens.access_token,
                        "refresh_token": tokens.refresh_token,
                        "expires_at": tokens.expires_at,
                        "scope": tokens.scope,
                    }),
                    created_at: now,
                    last_used_at: now,
                };
                self.state
                    .accounts
                    .upsert(record.clone())
                    .await
                    .map_err(|e| Status::internal(format!("persist account: {e}")))?;
                Ok(Response::new(record_to_proto(&record)))
            }
            _ => Err(Status::unimplemented("non-YouTube CompleteAuth lands later")),
        }
    }

    async fn list(
        &self,
        req: Request<ListAccountsRequest>,
    ) -> Result<Response<ListAccountsResponse>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let r = req.into_inner();
        let filter = if r.platform != ProtoPlatform::Unspecified as i32 {
            let p = ProtoPlatform::try_from(r.platform)
                .map_err(|_| Status::invalid_argument("bad platform"))?;
            Some(proto_to_core(p)?)
        } else {
            None
        };

        let accounts = self
            .state
            .accounts
            .list(tenant_id, filter)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(ListAccountsResponse {
            accounts: accounts.iter().map(record_to_proto).collect(),
        }))
    }

    async fn get(&self, req: Request<GetAccountRequest>) -> Result<Response<Account>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let id: Uuid = req
            .into_inner()
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("id is not a UUID"))?;
        let acc = self
            .state
            .accounts
            .get(tenant_id, id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("no such account"))?;
        Ok(Response::new(record_to_proto(&acc)))
    }

    async fn revoke(&self, req: Request<RevokeRequest>) -> Result<Response<()>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let id: Uuid = req
            .into_inner()
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("id is not a UUID"))?;
        self.state
            .accounts
            .delete(tenant_id, id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn check_auth(
        &self,
        _req: Request<CheckAuthRequest>,
    ) -> Result<Response<CheckAuthResponse>, Status> {
        Err(Status::unimplemented("CheckAuth implementation lands in Phase 1b"))
    }

    async fn register_developer_credentials(
        &self,
        req: Request<RegisterDeveloperRequest>,
    ) -> Result<Response<Account>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let r = req.into_inner();
        let proto = ProtoPlatform::try_from(r.platform)
            .map_err(|_| Status::invalid_argument("bad platform"))?;
        let platform = proto_to_core(proto)?;

        match platform {
            Platform::WxGzh => {
                // Parse the credentials JSON.
                let mut creds: WxGzhCredentials = serde_json::from_str(&r.credentials_json)
                    .map_err(|e| {
                        Status::invalid_argument(format!(
                            "credentials_json must be {{appid, app_secret}}: {e}"
                        ))
                    })?;
                if creds.appid.is_empty() || creds.app_secret.is_empty() {
                    return Err(Status::invalid_argument(
                        "appid and app_secret are required",
                    ));
                }
                // Mint an access token and validate the account.
                let refreshed = ensure_access_token(&self.state.http, &creds)
                    .await
                    .map_err(|e| Status::internal(format!("wx-gzh stable_token: {e}")))?;
                if let Some(updated) = refreshed {
                    creds = updated;
                }
                let info =
                    check_account_info(&self.state.http, &creds.access_token)
                        .await
                        .map_err(|e| {
                            Status::failed_precondition(format!(
                                "wx-gzh validation failed: {e}"
                            ))
                        })?;

                let now = Utc::now();
                let display_name = info
                    .nickname
                    .clone()
                    .unwrap_or_else(|| "(unnamed)".to_string());
                let record = AccountRecord {
                    id: Uuid::new_v4(),
                    user_id: tenant_id,
                    platform: Platform::WxGzh,
                    display_name,
                    external_id: info.appid.clone(),
                    auth_status: AuthStatus::Active,
                    credentials: serde_json::to_value(&creds).map_err(|e| {
                        Status::internal(format!("serialize wx-gzh creds: {e}"))
                    })?,
                    created_at: now,
                    last_used_at: now,
                };
                self.state
                    .accounts
                    .upsert(record.clone())
                    .await
                    .map_err(|e| Status::internal(format!("persist account: {e}")))?;
                tracing::info!(appid = %info.appid, account_type = ?info.account_type,
                               "wx-gzh account registered");
                Ok(Response::new(record_to_proto(&record)))
            }
            Platform::Douyin => {
                // Douyin credentials are just the CDP URL of the Chrome
                // profile that's logged into creator.douyin.com.
                let creds: DouyinCredentials = serde_json::from_str(&r.credentials_json)
                    .map_err(|e| {
                        Status::invalid_argument(format!(
                            "credentials_json must be {{cdp_url}}: {e}"
                        ))
                    })?;
                if creds.cdp_url.is_empty() {
                    return Err(Status::invalid_argument("cdp_url is required"));
                }
                let publisher = self
                    .state
                    .publishers
                    .get(&Platform::Douyin)
                    .cloned()
                    .ok_or_else(|| Status::internal("douyin publisher not registered"))?;
                let probe_creds = serde_json::to_value(&creds).map_err(|e| {
                    Status::internal(format!("serialize douyin creds: {e}"))
                })?;
                let probe_ctx = multipost_core::PublishContext {
                    account_id: Uuid::nil(),
                    user_id: tenant_id,
                    credentials: &probe_creds,
                    media: vec![],
                };
                let auth = publisher.check_auth(&probe_ctx).await.map_err(|e| {
                    Status::failed_precondition(format!("douyin check_auth: {e}"))
                })?;
                if !matches!(auth, AuthStatus::Active) {
                    return Err(Status::failed_precondition(format!(
                        "douyin profile at {} is not logged in (status={:?}). \
                         Open creator.douyin.com in that Chrome and scan the QR first.",
                        creds.cdp_url, auth
                    )));
                }

                let now = Utc::now();
                let record = AccountRecord {
                    id: Uuid::new_v4(),
                    user_id: tenant_id,
                    platform: Platform::Douyin,
                    display_name: if creds.nickname.is_empty() {
                        "(douyin)".to_string()
                    } else {
                        creds.nickname.clone()
                    },
                    external_id: creds.douyin_id.clone(),
                    auth_status: AuthStatus::Active,
                    credentials: probe_creds,
                    created_at: now,
                    last_used_at: now,
                };
                self.state
                    .accounts
                    .upsert(record.clone())
                    .await
                    .map_err(|e| Status::internal(format!("persist account: {e}")))?;
                tracing::info!(cdp = %creds.cdp_url, "douyin account registered");
                Ok(Response::new(record_to_proto(&record)))
            }
            Platform::Toutiao => {
                // Same shape as Douyin — cdp_url + cached display name + ID.
                let creds: ToutiaoCredentials = serde_json::from_str(&r.credentials_json)
                    .map_err(|e| {
                        Status::invalid_argument(format!(
                            "credentials_json must be {{cdp_url, nickname?, toutiao_id?}}: {e}"
                        ))
                    })?;
                if creds.cdp_url.is_empty() {
                    return Err(Status::invalid_argument("cdp_url is required"));
                }
                let publisher = self
                    .state
                    .publishers
                    .get(&Platform::Toutiao)
                    .cloned()
                    .ok_or_else(|| Status::internal("toutiao publisher not registered"))?;
                let probe_creds = serde_json::to_value(&creds)
                    .map_err(|e| Status::internal(format!("serialize toutiao creds: {e}")))?;
                let probe_ctx = multipost_core::PublishContext {
                    account_id: Uuid::nil(),
                    user_id: tenant_id,
                    credentials: &probe_creds,
                    media: vec![],
                };
                let auth = publisher.check_auth(&probe_ctx).await.map_err(|e| {
                    Status::failed_precondition(format!("toutiao check_auth: {e}"))
                })?;
                if !matches!(auth, AuthStatus::Active) {
                    return Err(Status::failed_precondition(format!(
                        "toutiao profile at {} is not logged in (status={:?}). \
                         Open mp.toutiao.com in that Chrome and finish the login flow.",
                        creds.cdp_url, auth
                    )));
                }

                let now = Utc::now();
                let record = AccountRecord {
                    id: Uuid::new_v4(),
                    user_id: tenant_id,
                    platform: Platform::Toutiao,
                    display_name: if creds.nickname.is_empty() {
                        "(toutiao)".to_string()
                    } else {
                        creds.nickname.clone()
                    },
                    external_id: creds.toutiao_id.clone(),
                    auth_status: AuthStatus::Active,
                    credentials: probe_creds,
                    created_at: now,
                    last_used_at: now,
                };
                self.state
                    .accounts
                    .upsert(record.clone())
                    .await
                    .map_err(|e| Status::internal(format!("persist account: {e}")))?;
                tracing::info!(cdp = %creds.cdp_url, "toutiao account registered");
                Ok(Response::new(record_to_proto(&record)))
            }
            Platform::Twitter => {
                // Twitter / X — CDP + handle. Same shape as Toutiao /
                // Douyin (browser-cookie auth, no token storage).
                let creds: TwitterCredentials = serde_json::from_str(&r.credentials_json)
                    .map_err(|e| {
                        Status::invalid_argument(format!(
                            "credentials_json must be {{cdp_url, handle, display_name?}}: {e}"
                        ))
                    })?;
                if creds.cdp_url.is_empty() {
                    return Err(Status::invalid_argument("cdp_url is required"));
                }
                if creds.handle.is_empty() {
                    return Err(Status::invalid_argument("handle is required"));
                }
                let publisher = self
                    .state
                    .publishers
                    .get(&Platform::Twitter)
                    .cloned()
                    .ok_or_else(|| Status::internal("twitter publisher not registered"))?;
                let probe_creds = serde_json::to_value(&creds)
                    .map_err(|e| Status::internal(format!("serialize twitter creds: {e}")))?;
                let probe_ctx = multipost_core::PublishContext {
                    account_id: Uuid::nil(),
                    user_id: tenant_id,
                    credentials: &probe_creds,
                    media: vec![],
                };
                let auth = publisher.check_auth(&probe_ctx).await.map_err(|e| {
                    Status::failed_precondition(format!("twitter check_auth: {e}"))
                })?;
                if !matches!(auth, AuthStatus::Active) {
                    return Err(Status::failed_precondition(format!(
                        "twitter profile at {} is not logged in (status={:?}). \
                         Open x.com in that Chrome and finish the login flow.",
                        creds.cdp_url, auth
                    )));
                }
                let now = Utc::now();
                let record = AccountRecord {
                    id: Uuid::new_v4(),
                    user_id: tenant_id,
                    platform: Platform::Twitter,
                    display_name: if creds.display_name.is_empty() {
                        format!("@{}", creds.handle)
                    } else {
                        creds.display_name.clone()
                    },
                    external_id: creds.handle.clone(),
                    auth_status: AuthStatus::Active,
                    credentials: probe_creds,
                    created_at: now,
                    last_used_at: now,
                };
                self.state
                    .accounts
                    .upsert(record.clone())
                    .await
                    .map_err(|e| Status::internal(format!("persist account: {e}")))?;
                tracing::info!(handle = %creds.handle, "twitter account registered");
                Ok(Response::new(record_to_proto(&record)))
            }
            Platform::YouTube => Err(Status::failed_precondition(
                "youtube uses OAuth; use StartAuth/CompleteAuth instead",
            )),
        }
    }
}
