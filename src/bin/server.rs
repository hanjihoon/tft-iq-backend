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
    extract::{Path, State, Query},
    http::StatusCode,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;
use tft_iq::{AppError, Config, db, meta::Meta};
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
        .route("/api/quiz/review/count", get(review_counts))
        .route("/api/quiz/stats", get(user_stats_handler))
        .route("/api/quiz/reset", post(reset_handler))
        .route("/api/quiz/{id}/report", post(report_puzzle))
        .route("/api/meta/decks", get(meta_decks_handler))
        .route("/api/meta/units", get(meta_units_handler))
        .route("/api/meta/traits", get(meta_traits_handler))
        .route("/api/meta/items", get(meta_items_handler))
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

#[derive(Deserialize)]
struct MetaQuery {
    lang: Option<String>,
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
    // puuid는 있으면 유저 정보 보강(선택), 시도 기록은 항상 저장
    if let Some(puuid) = req.user_puuid.as_deref() {
        db::ensure_user(&st.pool, puuid, None).await?;
    }
    db::record_attempt(&st.pool, user_id, id, &req.chosen, correct).await?;

    Ok(Json(AttemptResp { correct, answer, stats }))
}

/// 안 푼 퀴즈 하나. 헤더 X-User-Id로 익명 유저 식별.
async fn next_unsolved(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = headers.get("X-User-Id").and_then(|v| v.to_str().ok()).unwrap_or("anon");
    // ?type=item_combine | deck_complete, 기본은 item_combine
    let ptype = q.get("type").map(|s| s.as_str()).unwrap_or("item_combine");
    let mode = q.get("mode").map(|s| s.as_str()).unwrap_or("normal");


    let current_patch = match db::current_patch_info(&st.pool).await? {
        Some(info) => info.patch,
        None => {
            // 표본 충분한 패치 없음 → 서빙할 게 없음
            return Ok(Json(serde_json::json!({ "all_solved": true })));
        }
    };

    let puzzle = if mode == "review" {
        db::review_puzzle(&st.pool, user_id, ptype, &current_patch).await?
    } else {
        db::unsolved_puzzle_by_type(&st.pool, user_id, ptype, &current_patch).await?
    };

    match puzzle {
        Some(p) => Ok(Json(serde_json::json!({
            "status": "ok",
            "puzzle": {
                "id": p.id,
                "type": p.puzzle_type,       // "item_combine" | "deck_complete"
                "patch": p.patch,
                "prompt": p.prompt,
                "options": p.options,
                "stats": p.stats
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

// server.rs에 새 라우트 핸들러
async fn review_counts(
    State(st): State<AppState>, headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = headers.get("X-User-Id").and_then(|v| v.to_str().ok()).unwrap_or("anon");
    let item = db::review_count(&st.pool, user_id, "item_combine").await?;
    let deck = db::review_count(&st.pool, user_id, "deck_complete").await?;
    Ok(Json(serde_json::json!({ "item_combine": item, "deck_complete": deck })))
}
// 라우트 등록: .route("/api/quiz/review/count", get(review_counts))

async fn user_stats_handler(
    State(st): State<AppState>, headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = headers.get("X-User-Id").and_then(|v| v.to_str().ok()).unwrap_or("anon");
    Ok(Json(db::user_stats(&st.pool, user_id).await?))
}

async fn reset_handler(
    State(st): State<AppState>, headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = headers.get("X-User-Id").and_then(|v| v.to_str().ok()).unwrap_or("anon");
    let deleted = db::reset_attempts(&st.pool, user_id).await?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

async fn report_puzzle(
    State(st): State<AppState>, Path(id): Path<Uuid>, headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = headers.get("X-User-Id").and_then(|v| v.to_str().ok()).unwrap_or("anon");
    db::insert_report(&st.pool, id, user_id, None).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}


async fn meta_decks_handler(
    State(st): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    // 현재 패치 (표본 임계 넘는 최신)
    let patch = match db::current_patch_info(&st.pool).await? {
        Some(info) => info.patch,
        None => return Ok(Json(vec![])), // 패치 없으면 빈 목록
    };
    let decks = db::meta_decks(&st.pool, &patch).await?;
    Ok(Json(decks))
}

async fn meta_units_handler(
    Query(q): Query<MetaQuery>,
    State(st): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let patch = match db::current_patch_info(&st.pool).await? {
        Some(info) => info,
        None => return Ok(Json(serde_json::json!({}))),
    };
    let lang = q.lang.unwrap_or_else(|| "ko_kr".into());
    // 언어 화이트리스트 (안전 — 아무 문자열이나 URL에 넣으면 위험)
    let lang = validate_lang(&lang);
    let meta = Meta::load_with_lang(patch.set_number, &lang, false).await?;

    let info: serde_json::Map<String, serde_json::Value> = meta.units.iter()
        .map(|(id, u)| {
            (id.clone(), serde_json::json!({
                "name": u.name, 
                "cost": u.cost,
                "traits": u.traits,
                "ability": u.ability,  // SkillMeta (Serialize)
            }))
        })
        .collect();

    Ok(Json(serde_json::Value::Object(info)))
} 

async fn meta_traits_handler(
    Query(q): Query<MetaQuery>,
    State(st): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let patch = match db::current_patch_info(&st.pool).await? {
        Some(info) => info,
        None => return Ok(Json(serde_json::json!({}))),
    };
    let lang = q.lang.unwrap_or_else(|| "ko_kr".into());
    // 언어 화이트리스트 (안전 — 아무 문자열이나 URL에 넣으면 위험)
    let lang = validate_lang(&lang);
    let meta = Meta::load_with_lang(patch.set_number, &lang, false).await?;

    // 한글명 키로 맵 구성 (프론트 유닛 traits가 한글명이라 매칭 편함)
    let mut out = serde_json::Map::new();
    for (api, t) in meta.trait_details.iter() {  // api = apiName
        out.insert(api.clone(), serde_json::json!({
            "name": t.name,      // 이름 추가! (프론트가 표시용)
            "icon": t.icon,
            "breakpoints": t.breakpoints,
            "desc": t.desc,
            "effects": t.effects,
        }));
    }
    Ok(Json(serde_json::Value::Object(out)))
}

async fn meta_items_handler(
    Query(q): Query<MetaQuery>,
    State(st): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let patch = match db::current_patch_info(&st.pool).await? {
        Some(info) => info,
        None => return Ok(Json(serde_json::json!({}))),
    };
    let lang = validate_lang(&q.lang.unwrap_or_else(|| "ko_kr".into()));
    // patch, set_number 가져오기 (다른 핸들러처럼)
    let meta = Meta::load_with_lang(patch.set_number, &lang, false).await?;

    // items 맵을 그대로 JSON으로
    let out: serde_json::Map<String, Value> = meta.items.iter()
        .map(|(api, name)| (api.clone(), Value::String(name.clone())))
        .collect();
    Ok(Json(Value::Object(out)))
}

fn validate_lang(lang: &str) -> String {
    const ALLOWED: &[&str] = &[
        "ko_kr", "en_us", "ja_jp", "zh_cn", "pt_br",
        "es_mx", "fr_fr", "de_de", "ru_ru", "vi_vn", "th_th",
    ];
    if ALLOWED.contains(&lang) { lang.to_string() }
    else { "ko_kr".to_string() }  // 기본 폴백
}