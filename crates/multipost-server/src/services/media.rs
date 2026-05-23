//! `Media` gRPC service implementation.

use std::sync::Arc;

use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use multipost_proto::media::media_server::Media as MediaTrait;
use multipost_proto::media::upload_chunk::Payload as ChunkPayload;
use multipost_proto::media::{MediaAsset, MediaRef, UploadChunk};
use multipost_storage::media::{MediaRecord, MediaRepository};

use crate::auth::tenant_id_from_request;
use crate::state::AppState;

/// Media service backed by the shared state.
pub struct MediaService {
    state: Arc<AppState>,
}

impl MediaService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

fn ext_for(mime: &str) -> &'static str {
    match mime {
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        _ => "bin",
    }
}

fn record_to_proto(r: &MediaRecord) -> MediaAsset {
    MediaAsset {
        id: r.id.to_string(),
        uri: format!("file://{}", r.path.display()),
        mime_type: r.mime_type.clone(),
        size_bytes: r.size_bytes,
        width: 0,
        height: 0,
        duration_ms: 0,
        sha256: r.sha256.clone(),
        created_at: Some(prost_types::Timestamp {
            seconds: r.created_at.timestamp(),
            nanos: 0,
        }),
    }
}

#[tonic::async_trait]
impl MediaTrait for MediaService {
    async fn upload(
        &self,
        req: Request<Streaming<UploadChunk>>,
    ) -> Result<Response<MediaAsset>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let mut stream = req.into_inner();

        // First chunk must contain `meta`.
        let first = stream
            .next()
            .await
            .ok_or_else(|| Status::invalid_argument("upload stream was empty"))?
            .map_err(|e| Status::internal(format!("stream error: {e}")))?;
        let meta = match first.payload {
            Some(ChunkPayload::Meta(m)) => m,
            _ => return Err(Status::invalid_argument("first chunk must be UploadMeta")),
        };

        let id = Uuid::new_v4();
        let ext = ext_for(&meta.mime_type);
        let path = self.state.media_dir.join(format!("{id}.{ext}"));
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| Status::internal(format!("open {path:?}: {e}")))?;
        let mut hasher = Sha256::new();
        let mut size: i64 = 0;

        while let Some(item) = stream.next().await {
            let chunk = item.map_err(|e| Status::internal(format!("stream error: {e}")))?;
            match chunk.payload {
                Some(ChunkPayload::Data(bytes)) => {
                    hasher.update(&bytes);
                    size += bytes.len() as i64;
                    file.write_all(&bytes)
                        .await
                        .map_err(|e| Status::internal(format!("write: {e}")))?;
                }
                Some(ChunkPayload::Meta(_)) => {
                    return Err(Status::invalid_argument(
                        "only the first chunk may contain meta",
                    ));
                }
                None => return Err(Status::invalid_argument("chunk with empty payload")),
            }
        }
        file.flush()
            .await
            .map_err(|e| Status::internal(format!("flush: {e}")))?;
        drop(file);

        let sha256 = format!("{:x}", hasher.finalize());
        let now = Utc::now();
        let record = MediaRecord {
            id,
            user_id: tenant_id,
            mime_type: meta.mime_type,
            filename: meta.filename,
            path,
            size_bytes: size,
            sha256,
            created_at: now,
        };
        self.state
            .media
            .insert(record.clone())
            .await
            .map_err(|e| Status::internal(format!("persist media: {e}")))?;
        tracing::info!(media_id = %id, bytes = size, "media upload complete");
        Ok(Response::new(record_to_proto(&record)))
    }

    async fn get(&self, req: Request<MediaRef>) -> Result<Response<MediaAsset>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let id: Uuid = req
            .into_inner()
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("id not a UUID"))?;
        let r = self
            .state
            .media
            .get(tenant_id, id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("no such media"))?;
        Ok(Response::new(record_to_proto(&r)))
    }

    async fn delete(&self, req: Request<MediaRef>) -> Result<Response<()>, Status> {
        let tenant_id = tenant_id_from_request(&req)?;
        let id: Uuid = req
            .into_inner()
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("id not a UUID"))?;
        self.state
            .media
            .delete(tenant_id, id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }
}
