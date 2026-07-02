//! tft-iq 공유 라이브러리. 크롤러/분석기/생성기/서버가 이 모듈들을 가져다 쓴다.

pub mod comp;
pub mod config;
pub mod db;
pub mod deck_cluster;
pub mod error;
pub mod meta;
pub mod puzzle;
pub mod riot;

pub use config::Config;
pub use error::{AppError, Result};