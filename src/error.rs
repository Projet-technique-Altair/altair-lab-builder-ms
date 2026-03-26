use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use crate::models::api::{ApiError, ApiErrorResponse, ApiMeta};

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            AppError::BadRequest(message) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", message),
            AppError::NotFound(message) => (StatusCode::NOT_FOUND, "NOT_FOUND", message),
            AppError::Internal(message) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", message)
            }
        };

        let body = ApiErrorResponse {
            success: false,
            error: ApiError {
                code: code.to_string(),
                message,
                details: None,
            },
            meta: ApiMeta::new(),
        };

        (status, Json(body)).into_response()
    }
}
