//! gRPC auth interceptor.
//!
//! Every request must carry `authorization: Bearer <key>` in its
//! metadata. The interceptor sha256-hashes the key, looks it up in the
//! tenant repository, and injects a [`TenantContext`] into the request's
//! extensions. Handlers read `TenantContext.tenant_id` and use it where
//! the old `bootstrap_user` constant lived.
//!
//! Set `MULTIPOST_DEV_NO_AUTH=1` to disable the check entirely. In that
//! mode every request is mapped to `Uuid::nil()` so existing data
//! created before auth was wired stays accessible.

use std::sync::Arc;

use multipost_storage::tenants::{hash_key, FileBackedTenantRepository};
use tonic::service::Interceptor;
use tonic::{Request, Status};
use uuid::Uuid;

/// The auth identity attached to each request after the interceptor runs.
/// Handlers extract this from request extensions via [`tenant_id_from_request`].
#[derive(Debug, Clone, Copy)]
pub struct TenantContext {
    /// Tenant ID this request is scoped to.
    pub tenant_id: Uuid,
}

/// Interceptor that resolves a Bearer token to a `TenantContext`.
/// Cloneable — required by tonic so each service wrapper can hold its own copy.
#[derive(Clone)]
pub struct AuthInterceptor {
    /// Shared file-backed tenant store. Lookup is a single in-memory
    /// scan since the file is loaded into a Mutex<HashMap> at open time.
    tenants: Arc<FileBackedTenantRepository>,
    /// When true, skip key checks entirely and bind every request to
    /// `Uuid::nil()`. Controlled by `MULTIPOST_DEV_NO_AUTH=1`.
    dev_no_auth: bool,
}

impl AuthInterceptor {
    /// Build an interceptor against `tenants`. `dev_no_auth=true` is for
    /// local dev only — production deploys must leave it false.
    pub fn new(tenants: Arc<FileBackedTenantRepository>, dev_no_auth: bool) -> Self {
        Self {
            tenants,
            dev_no_auth,
        }
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if self.dev_no_auth {
            req.extensions_mut().insert(TenantContext {
                tenant_id: Uuid::nil(),
            });
            return Ok(req);
        }
        let token = req
            .metadata()
            .get("authorization")
            .ok_or_else(|| Status::unauthenticated("missing authorization metadata"))?
            .to_str()
            .map_err(|_| Status::unauthenticated("authorization metadata is not ASCII"))?
            .strip_prefix("Bearer ")
            .ok_or_else(|| {
                Status::unauthenticated("authorization must be of form 'Bearer <key>'")
            })?;
        let hash = hash_key(token);
        let tenant_id = self
            .tenants
            .resolve_key(&hash)
            .ok_or_else(|| Status::unauthenticated("api key not recognized"))?;
        req.extensions_mut().insert(TenantContext { tenant_id });
        Ok(req)
    }
}

/// Pull `tenant_id` off a request's extensions. Returns `Internal` if
/// the interceptor wasn't wired (programming error) — `Unauthenticated`
/// failures are surfaced at the interceptor layer, not here.
pub fn tenant_id_from_request<T>(req: &Request<T>) -> Result<Uuid, Status> {
    req.extensions()
        .get::<TenantContext>()
        .map(|c| c.tenant_id)
        .ok_or_else(|| {
            Status::internal("TenantContext missing — AuthInterceptor not wired")
        })
}
