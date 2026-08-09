use axum::Json;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use fluvora_auth::Scopes;
use serde::Deserialize;

use crate::error::ApiError;
use crate::gateway_client::{gateway_json_request, gateway_request};
use crate::models::AppState;
use crate::services::authenticate;
use crate::validation::{idempotency_key, percent_encode_query, validate_media_identifier};

const MAX_MEDIA_CONTROL_BODY_BYTES: usize = 8 * 1_024 * 1_024;

#[derive(Debug, Deserialize)]
pub(super) struct AssetUploadQuery {
    offset: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct LiveSegmentQuery {
    duration_millis: u64,
    discontinuity: Option<bool>,
    program_date_time: Option<String>,
}

pub(super) async fn create_asset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::VOD_MANAGE, None)?;
    let _ = idempotency_key(&headers)?;
    gateway_json_request(&state, reqwest::Method::POST, "/v1/assets", &request).await
}

pub(super) async fn get_asset(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::VOD_MANAGE, None)?;
    validate_media_identifier(&asset_id)?;
    gateway_request(
        &state,
        reqwest::Method::GET,
        &format!("/v1/assets/{asset_id}"),
        None,
    )
    .await
}

pub(super) async fn delete_asset(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::VOD_MANAGE, None)?;
    let _ = idempotency_key(&headers)?;
    validate_media_identifier(&asset_id)?;
    gateway_request(
        &state,
        reqwest::Method::DELETE,
        &format!("/v1/assets/{asset_id}"),
        None,
    )
    .await
}

pub(super) async fn upload_asset_chunk(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    Query(query): Query<AssetUploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::VOD_MANAGE, None)?;
    validate_media_identifier(&asset_id)?;
    let bytes = media_upload_body(
        body,
        "upload_chunk_too_large",
        "VOD upload chunk exceeds 8 MiB",
        "VOD upload chunk",
    )
    .await?;
    gateway_request(
        &state,
        reqwest::Method::PATCH,
        &format!("/v1/assets/{asset_id}/source?offset={}", query.offset),
        Some((bytes, "application/octet-stream")),
    )
    .await
}

pub(super) async fn complete_asset(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::VOD_MANAGE, None)?;
    let _ = idempotency_key(&headers)?;
    validate_media_identifier(&asset_id)?;
    gateway_json_request(
        &state,
        reqwest::Method::POST,
        &format!("/v1/assets/{asset_id}/complete"),
        &request,
    )
    .await
}

pub(super) async fn create_live_stream(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::LIVE_MANAGE, None)?;
    let _ = idempotency_key(&headers)?;
    validate_media_identifier(&stream_id)?;
    gateway_json_request(
        &state,
        reqwest::Method::POST,
        &format!("/v1/live/{stream_id}"),
        &request,
    )
    .await
}

pub(super) async fn get_live_stream(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::LIVE_MANAGE, None)?;
    validate_media_identifier(&stream_id)?;
    gateway_request(
        &state,
        reqwest::Method::GET,
        &format!("/v1/live/{stream_id}"),
        None,
    )
    .await
}

pub(super) async fn delete_live_stream(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::LIVE_MANAGE, None)?;
    let _ = idempotency_key(&headers)?;
    validate_media_identifier(&stream_id)?;
    gateway_request(
        &state,
        reqwest::Method::DELETE,
        &format!("/v1/live/{stream_id}"),
        None,
    )
    .await
}

pub(super) async fn upload_live_init(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::LIVE_MANAGE, None)?;
    validate_media_identifier(&stream_id)?;
    let bytes = media_upload_body(
        body,
        "live_init_too_large",
        "live initialization segment exceeds 8 MiB",
        "live initialization segment",
    )
    .await?;
    gateway_request(
        &state,
        reqwest::Method::PUT,
        &format!("/v1/live/{stream_id}/init"),
        Some((bytes, "video/mp4")),
    )
    .await
}

pub(super) async fn upload_live_segment(
    State(state): State<AppState>,
    Path((stream_id, sequence)): Path<(String, u64)>,
    Query(query): Query<LiveSegmentQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::LIVE_MANAGE, None)?;
    validate_media_identifier(&stream_id)?;
    let bytes = media_upload_body(
        body,
        "live_segment_too_large",
        "live media segment exceeds 8 MiB",
        "live media segment",
    )
    .await?;
    let mut path = format!(
        "/v1/live/{stream_id}/segments/{sequence}?duration_millis={}",
        query.duration_millis
    );
    if query.discontinuity.unwrap_or_default() {
        path.push_str("&discontinuity=true");
    }
    if let Some(timestamp) = query.program_date_time {
        path.push_str("&program_date_time=");
        path.push_str(&percent_encode_query(&timestamp));
    }
    gateway_request(
        &state,
        reqwest::Method::PUT,
        &path,
        Some((bytes, "video/iso.segment")),
    )
    .await
}

pub(super) async fn finish_live_stream(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, Scopes::LIVE_MANAGE, None)?;
    let _ = idempotency_key(&headers)?;
    validate_media_identifier(&stream_id)?;
    gateway_request(
        &state,
        reqwest::Method::POST,
        &format!("/v1/live/{stream_id}/finish"),
        Some((axum::body::Bytes::from_static(b"{}"), "application/json")),
    )
    .await
}

async fn media_upload_body(
    body: Body,
    oversized_code: &'static str,
    oversized_message: &'static str,
    label: &'static str,
) -> Result<Bytes, ApiError> {
    let bytes = to_bytes(body, MAX_MEDIA_CONTROL_BODY_BYTES)
        .await
        .map_err(|_| ApiError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: oversized_code,
            message: oversized_message.to_owned(),
        })?;
    if bytes.is_empty() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "empty_media_upload",
            message: format!("{label} cannot be empty"),
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::StatusCode;

    use super::{MAX_MEDIA_CONTROL_BODY_BYTES, media_upload_body};

    #[tokio::test]
    async fn media_uploads_are_non_empty_and_bounded() {
        let empty = media_upload_body(Body::empty(), "too_large", "too large", "media")
            .await
            .expect_err("empty upload");
        assert_eq!(empty.status, StatusCode::BAD_REQUEST);
        assert_eq!(empty.code, "empty_media_upload");

        let oversized = media_upload_body(
            Body::from(vec![0_u8; MAX_MEDIA_CONTROL_BODY_BYTES + 1]),
            "too_large",
            "too large",
            "media",
        )
        .await
        .expect_err("oversized upload");
        assert_eq!(oversized.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(oversized.code, "too_large");
    }
}
