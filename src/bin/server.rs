//! HTTP API 서버 (axum).
//!
//! 엔드포인트:
//!   GET  /health                       헬스체크
//!   GET  /api/puzzles/daily            오늘의 퍼즐 (정답 제외)
//!   POST /api/puzzles/:id/attempt      답안 제출 → 채점 + 통계 피드백
//!   GET  /api/me/:puuid/weakness       퍼즐 타입별 정답률 (약점 분석)
//!
//! 실행:  cargo run --bin server

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use tft_iq::{AppError, Config, db};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    pool: sqlx::PgPool,
}


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tft_iq=debug".into()),
        )
        .init();

    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;
    let state = AppState { pool };

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/puzzles/daily", get(daily_puzzle))
        .route("/api/puzzles/{id}/attempt", post(submit_attempt))
        .route("/api/me/{puuid}/weakness", get(user_weakness))
        .route("/api/quiz/item", get(item_quiz))
        .route("/api/quiz/{id}/answer", post(answer_quiz))
        .route("/api/quiz/next", get(next_unsolved))
        .route("/api/meta/info", get(meta_info_handler))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive()) // 개발용. 운영에선 도메인 제한.
        .with_state(state);

    let addr: SocketAddr = cfg.bind_addr.parse()?;
    info!("서버 시작: http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// ───────────────────────── 핸들러 ─────────────────────────

async fn health() -> &'static str {
    "ok"
}

#[derive(Serialize)]
struct PuzzleView {
    id: Uuid,
    puzzle_type: String,
    patch: String,
    set_number: i32,
    prompt: serde_json::Value,
    options: serde_json::Value,
}

async fn daily_puzzle(State(st): State<AppState>) -> Result<Json<PuzzleView>, ApiError> {
    let puzzle = db::random_puzzle(&st.pool, None)
        .await?
        .ok_or(ApiError::NotFound)?;

    // 정답(answer)과 stats는 제출 전에는 숨긴다.
    Ok(Json(PuzzleView {
        id: puzzle.id,
        puzzle_type: puzzle.puzzle_type,
        patch: puzzle.patch,
        set_number: puzzle.set_number,
        prompt: puzzle.prompt,
        options: puzzle.options,
    }))
}

#[derive(Deserialize)]
struct AttemptReq {
    chosen: String,
    /// RSO 로그인 사용자의 puuid. 비로그인 풀이면 None.
    user_puuid: Option<String>,
}

#[derive(Serialize)]
struct AttemptResp {
    correct: bool,
    answer: String,
    /// 보기별 평균 등수 등 피드백 (winrate 아님)
    stats: serde_json::Value,
}

async fn submit_attempt(
    State(st): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<AttemptReq>,
) -> Result<Json<AttemptResp>, ApiError> {
    let (answer, stats) = db::puzzle_answer(&st.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let correct = req.chosen == answer;

    // 로그인 사용자면 약점 분석용으로 기록
    if let Some(puuid) = req.user_puuid.as_deref() {
        db::ensure_user(&st.pool, puuid, None).await?;
        db::record_attempt(&st.pool, puuid, id, &req.chosen, correct).await?;
    }

    Ok(Json(AttemptResp {
        correct,
        answer,
        stats,
    }))
}

async fn user_weakness(
    State(st): State<AppState>,
    Path(puuid): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows = db::user_weakness(&st.pool, &puuid).await?;
    let breakdown: Vec<_> = rows
        .into_iter()
        .map(|(ptype, attempts, correct)| {
            let accuracy = if attempts > 0 {
                (correct as f64 / attempts as f64 * 1000.0).round() / 1000.0
            } else {
                0.0
            };
            json!({
                "puzzle_type": ptype,
                "attempts": attempts,
                "correct": correct,
                "accuracy": accuracy,
            })
        })
        .collect();
    Ok(Json(json!({ "weakness": breakdown })))
}

// ───────────────────────── 에러 → HTTP ─────────────────────────

enum ApiError {
    NotFound,
    Internal(AppError),
}

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        ApiError::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            ApiError::Internal(e) => {
                tracing::error!("internal error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

/// 아이템 퀴즈 하나 내보내기 (정답/표본 숨김).
async fn item_quiz(State(st): State<AppState>) -> Result<Json<PuzzleView>, ApiError> {
    let puzzle = db::random_item_puzzle(&st.pool, 10)   // 표본 10+
        .await?
        .ok_or(ApiError::NotFound)?;

    Ok(Json(PuzzleView {
        id: puzzle.id,
        puzzle_type: puzzle.puzzle_type,
        patch: puzzle.patch,
        set_number: puzzle.set_number,
        prompt: puzzle.prompt,
        options: puzzle.options,
        // stats/answer는 의도적으로 제외 — 제출 전엔 숨김
    }))
}

/// 답안 제출 → 채점 + stats(보기별 평균등수) 공개.
async fn answer_quiz(
    State(st): State<AppState>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(req): Json<AttemptReq>,
) -> Result<Json<AttemptResp>, ApiError> {
    let (answer, stats) = db::puzzle_answer(&st.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let correct = req.chosen == answer;

    let user_id = headers.get("X-User-Id").and_then(|v| v.to_str().ok()).unwrap_or("anon");
    if let Some(puuid) = req.user_puuid.as_deref() {
        db::ensure_user(&st.pool, puuid, None).await?;
        db::record_attempt(&st.pool, user_id, id, &req.chosen, correct).await?;
    }

    Ok(Json(AttemptResp { correct, answer, stats }))
}

/// 안 푼 퀴즈 하나. 헤더 X-User-Id로 익명 유저 식별.
async fn next_unsolved(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = headers
        .get("X-User-Id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("anon");

    match db::unsolved_item_puzzle(&st.pool, user_id, 100).await? {
        Some(p) => Ok(Json(serde_json::json!({
            "status": "ok",
            "puzzle": {
                "id": p.id, "patch": p.patch,
                "prompt": p.prompt, "options": p.options,
            }
        }))),
        // 다 품 → 프론트가 "오늘의 문제 완료" 화면
        None => Ok(Json(serde_json::json!({ "status": "all_solved" }))),
    }
}

async fn meta_info_handler(State(st): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let m = db::meta_info(&st.pool).await?;
    // 표본 수로 "상위 N위" 추정 (티어 풀과 연동)
    let approx_rank = if m.total_matches >= 3000 { "상위 ~1500위" } else { "상위 ~1500위 (수집 중)" };
    Ok(Json(serde_json::json!({
        "patch": m.patch,
        "total_matches": m.total_matches,
        "puzzle_count": m.puzzle_count,
        "approx_rank": approx_rank,
        "region": "한국 서버",
    })))
}