use serde::Serialize;

#[derive(Serialize)]
pub struct ApiMeta {
    pub request_id: String,
    pub timestamp: String,
}

#[derive(Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct ApiErrorResponse {
    pub success: bool,
    pub error: ApiError,
    pub meta: ApiMeta,
}

#[derive(Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: T,
    pub meta: ApiMeta,
}

impl ApiMeta {
    pub fn new() -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

impl<T> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data,
            meta: ApiMeta::new(),
        }
    }
}
