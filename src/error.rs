use thiserror::Error;

/// 프로젝트 전역 에러 타입.
/// Java의 checked exception 계층 대신, Rust는 enum 하나로 에러 종류를 표현하고
/// `?` 연산자로 전파한다. `#[from]` 덕분에 reqwest/sqlx 에러가 자동 변환된다.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("Riot API 요청 실패: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Riot API가 {status} 응답: {body}")]
    RiotStatus {
        status: u16,
        body: String,
    },

    #[error("DB 오류: {0}")]
    Db(#[from] sqlx::Error),

    #[error("JSON 직렬화 오류: {0}")]
    Json(#[from] serde_json::Error),

    #[error("설정 오류: {0}")]
    ConfigError(String),

    #[error("리소스를 찾을 수 없음: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, AppError>;