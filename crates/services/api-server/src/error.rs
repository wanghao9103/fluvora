use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

#[derive(Debug)]
pub(super) struct ApiError {
    pub(super) status: StatusCode,
    pub(super) code: &'static str,
    pub(super) message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

impl From<fluvora_control_store::StoreError> for ApiError {
    fn from(error: fluvora_control_store::StoreError) -> Self {
        eprintln!("control-store operation failed: {error}");
        drop(error);
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "control_store_unavailable",
            message: "control store is unavailable".to_owned(),
        }
    }
}

pub(super) fn unauthorized() -> ApiError {
    ApiError {
        status: StatusCode::UNAUTHORIZED,
        code: "unauthorized",
        message: "valid Bearer token required".to_owned(),
    }
}

pub(super) fn forbidden() -> ApiError {
    ApiError {
        status: StatusCode::FORBIDDEN,
        code: "forbidden",
        message: "token scope, room restriction, or membership denied".to_owned(),
    }
}

pub(super) fn room_not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "room_not_found",
        message: "room does not exist".to_owned(),
    }
}

pub(super) fn domain_error(error: &fluvora_domain::RoomError) -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        code: "room_command_rejected",
        message: error.to_string(),
    }
}

pub(super) fn internal_error(error: impl std::fmt::Display) -> ApiError {
    eprintln!("API internal operation failed: {error}");
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal_error",
        message: "internal API operation failed".to_owned(),
    }
}

pub(super) fn control_store_unavailable(error: fluvora_control_store::StoreError) -> ApiError {
    eprintln!("control-store operation failed: {error}");
    drop(error);
    ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "control_store_unavailable",
        message: "control store is unavailable".to_owned(),
    }
}

pub(super) fn lock_error<T>(_: std::sync::PoisonError<T>) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "state_unavailable",
        message: "API state lock is poisoned".to_owned(),
    }
}

pub(super) fn state_io_error(error: &std::io::Error) -> ApiError {
    eprintln!("API state persistence failed: {error}");
    ApiError {
        status: StatusCode::INSUFFICIENT_STORAGE,
        code: "state_persistence_failed",
        message: "API state persistence failed".to_owned(),
    }
}
