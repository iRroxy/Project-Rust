use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    ServiceUnavailable(String),

    #[error("Internal server error")]
    Internal(#[from] anyhow::Error),

    #[error("Database error")]
    Database(#[from] sqlx::Error),
}

#[derive(Serialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            AppError::Database(sqlx::Error::RowNotFound) => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "resource not found".to_string(),
            ),
            AppError::Database(sqlx::Error::PoolTimedOut) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                "database connection pool exhausted; please retry".to_string(),
            ),
            AppError::Database(sqlx::Error::Database(db_err))
                if db_err.code().as_deref() == Some("23503") =>
            {
                (
                    StatusCode::NOT_FOUND,
                    "NOT_FOUND",
                    "referenced resource not found".to_string(),
                )
            }
            AppError::Database(err) => {
                tracing::error!(error = ?err, "Database error occurred");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "DATABASE_ERROR",
                    "A database error occurred".to_string(),
                )
            }
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg.clone()),
            AppError::ServiceUnavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                msg.clone(),
            ),
            AppError::Internal(err) => {
                tracing::error!(error = ?err, "Internal server error occurred");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "An unexpected internal error occurred".to_string(),
                )
            }
        };

        let body = Json(ErrorResponse {
            error: ErrorDetail { code, message },
        });

        (status, body).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
