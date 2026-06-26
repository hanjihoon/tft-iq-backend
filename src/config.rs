use crate::{AppError, Result};

/// 환경변수에서 읽어오는 런타임 설정.
#[derive(Debug, Clone)]
pub struct Config {
    pub riot_api_key: String,
    pub riot_platform: String, // kr, na1 ...   (리그/소환사 라우팅)
    pub riot_region: String,   // asia, americas, europe (매치 라우팅)
    pub database_url: String,
    pub bind_addr: String,
}

impl Config {
    /// .env + 실제 환경변수에서 로드. 누락 시 명확한 에러를 던진다.
    pub fn from_env() -> Result<Self> {
        // .env가 없어도 무시 (CI/배포 환경에선 실제 env를 씀)
        let _ = dotenvy::dotenv();

        fn require(key: &str) -> Result<String> {
            std::env::var(key).map_err(|_| AppError::ConfigError(format!("환경변수 {key} 누락")))
        }

        Ok(Self {
            riot_api_key: require("RIOT_API_KEY")?,
            riot_platform: std::env::var("RIOT_PLATFORM").unwrap_or_else(|_| "kr".into()),
            riot_region: std::env::var("RIOT_REGION").unwrap_or_else(|_| "asia".into()),
            database_url: require("DATABASE_URL")?,
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into()),
        })
    }
}