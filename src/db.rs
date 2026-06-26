//! DB 접근 계층. sqlx 런타임 쿼리(컴파일타임 매크로 X)를 써서
//! DB 없이도 컴파일된다. 안정화되면 query! 매크로로 바꿔 타입 검증을 강화하면 좋다.

use crate::error::Result;
use crate::riot::dto::Match;
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions};
use uuid::Uuid;

pub async fn connect(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;
    Ok(pool)
}

// ───────────────────────── 크롤러용 ─────────────────────────

/// 추적 대상 상위권 플레이어를 upsert.
pub async fn upsert_tracked_player(
    pool: &PgPool,
    puuid: &str,
    tier: &str,
    league_points: i32,
    region: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO tracked_players (puuid, tier, league_points, region)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (puuid) DO UPDATE
          SET tier = EXCLUDED.tier,
              league_points = EXCLUDED.league_points,
              region = EXCLUDED.region
        "#,
    )
    .bind(puuid)
    .bind(tier)
    .bind(league_points)
    .bind(region)
    .execute(pool)
    .await?;
    Ok(())
}

/// raw 매치 저장. 이미 있으면 무시. 반환: 새로 저장됐으면 true.
pub async fn insert_raw_match(pool: &PgPool, m: &Match, region: &str) -> Result<bool> {
    let patch = m.info.patch();
    let raw = serde_json::to_value(m)?;
    let game_dt = DateTime::from_timestamp_millis(m.info.game_datetime).unwrap_or_else(Utc::now);

    let result = sqlx::query(
        r#"
        INSERT INTO raw_matches
            (match_id, set_number, patch, queue_id, game_datetime, region, raw)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (match_id) DO NOTHING
        "#,
    )
    .bind(&m.metadata.match_id)
    .bind(m.info.tft_set_number)
    .bind(&patch)
    .bind(m.info.queue_id)
    .bind(game_dt)
    .bind(region)
    .bind(raw)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn players_to_crawl(pool: &PgPool, limit: i64) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        r#"
        SELECT puuid FROM tracked_players
        ORDER BY last_crawled_at ASC NULLS FIRST
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(p,)| p).collect())
}

pub async fn mark_crawled(pool: &PgPool, puuid: &str) -> Result<()> {
    sqlx::query("UPDATE tracked_players SET last_crawled_at = now() WHERE puuid = $1")
        .bind(puuid)
        .execute(pool)
        .await?;
    Ok(())
}

/// 스토리지 관리: 지난 패치의 오래된 raw 매치 삭제. 반환: 삭제 건수.
/// (Supabase 무료 티어를 쓰면 크롤러 끝에 주기적으로 호출하면 좋다.)
pub async fn delete_matches_before(pool: &PgPool, cutoff: DateTime<Utc>) -> Result<u64> {
    let r = sqlx::query("DELETE FROM raw_matches WHERE game_datetime < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(r.rows_affected())
}

// ───────────────────────── generator용 ─────────────────────────

/// raw_matches에서 가장 최근에 데이터가 쌓인 (set, patch)를 찾는다.
pub async fn latest_patch(pool: &PgPool) -> Result<Option<(i32, String)>> {
    let row: Option<(i32, String)> = sqlx::query_as(
        r#"
        SELECT set_number, patch
        FROM raw_matches
        GROUP BY set_number, patch
        ORDER BY MAX(game_datetime) DESC
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 퍼즐 소스로 쓸 매치들을 raw에서 역직렬화해 가져온다.
pub async fn load_matches(
    pool: &PgPool,
    set_number: i32,
    patch: &str,
    limit: i64,
) -> Result<Vec<Match>> {
    let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
        r#"
        SELECT raw FROM raw_matches
        WHERE set_number = $1 AND patch = $2
        ORDER BY game_datetime DESC
        LIMIT $3
        "#,
    )
    .bind(set_number)
    .bind(patch)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for (raw,) in rows {
        // 스키마가 안 맞는 옛 매치는 조용히 스킵
        if let Ok(m) = serde_json::from_value::<Match>(raw) {
            out.push(m);
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct AugStat {
    pub id: String,
    pub avg_placement: f64,
    pub picks: i64,
}

/// 오그먼트별 평균 등수 + 픽 수 집계 (JSONB 안을 펼쳐서 계산).
/// ★ 승률이 아니라 평균 등수 — 정책 + 지표 정확도 양쪽 이유.
pub async fn augment_placement_stats(
    pool: &PgPool,
    set_number: i32,
    patch: &str,
    min_picks: i64,
) -> Result<Vec<AugStat>> {
    let rows: Vec<(String, f64, i64)> = sqlx::query_as(
        r#"
        SELECT aug,
               AVG(placement)::float8 AS avg_place,
               COUNT(*)::int8         AS picks
        FROM (
          SELECT (p->>'placement')::int                  AS placement,
                 jsonb_array_elements_text(p->'augments') AS aug
          FROM raw_matches m,
               jsonb_array_elements(m.raw->'info'->'participants') AS p
          WHERE m.set_number = $1 AND m.patch = $2
        ) t
        GROUP BY aug
        HAVING COUNT(*) >= $3
        ORDER BY avg_place ASC
        "#,
    )
    .bind(set_number)
    .bind(patch)
    .bind(min_picks)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, avg_placement, picks)| AugStat {
            id,
            avg_placement,
            picks,
        })
        .collect())
}

/// 생성된 퍼즐 저장.
#[allow(clippy::too_many_arguments)]
pub async fn insert_puzzle(
    pool: &PgPool,
    kind: &str,
    set_number: i32,
    patch: &str,
    prompt: &serde_json::Value,
    options: &serde_json::Value,
    answer: &str,
    stats: &serde_json::Value,
    source_match_id: Option<&str>,   // &str → Option<&str>
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO puzzles
            (puzzle_type, set_number, patch, prompt, options, answer, stats, source_match_id)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(kind)
    .bind(set_number)
    .bind(patch)
    .bind(prompt)
    .bind(options)
    .bind(answer)
    .bind(stats)
    .bind(source_match_id)
    .execute(pool)
    .await?;
    Ok(())
}

// ───────────────────────── 서버용: 퍼즐 조회 ─────────────────────────

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct PuzzleRow {
    pub id: Uuid,
    pub puzzle_type: String,
    pub patch: String,
    pub set_number: i32,
    pub prompt: serde_json::Value,
    pub options: serde_json::Value,
    pub stats: serde_json::Value,
}

pub async fn random_puzzle(pool: &PgPool, patch: Option<&str>) -> Result<Option<PuzzleRow>> {
    let row: Option<PuzzleRow> = sqlx::query_as(
        r#"
        SELECT id, puzzle_type, patch, set_number, prompt, options, stats
        FROM puzzles
        WHERE ($1::text IS NULL OR patch = $1)
        ORDER BY random()
        LIMIT 1
        "#,
    )
    .bind(patch)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn puzzle_answer(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<(String, serde_json::Value)>> {
    let row: Option<(String, serde_json::Value)> =
        sqlx::query_as("SELECT answer, stats FROM puzzles WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row)
}

// ───────────────────────── 서버용: 사용자 / 기록 ─────────────────────────

/// RSO 로그인 사용자 보장 (없으면 생성).
pub async fn ensure_user(pool: &PgPool, puuid: &str, riot_id: Option<&str>) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO users (puuid, riot_id) VALUES ($1, $2)
        ON CONFLICT (puuid) DO UPDATE SET riot_id = COALESCE(EXCLUDED.riot_id, users.riot_id)
        "#,
    )
    .bind(puuid)
    .bind(riot_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_attempt(
    pool: &PgPool,
    user_puuid: &str,
    puzzle_id: Uuid,
    chosen: &str,
    correct: bool,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO puzzle_attempts (user_puuid, puzzle_id, chosen, correct)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(user_puuid)
    .bind(puzzle_id)
    .bind(chosen)
    .bind(correct)
    .execute(pool)
    .await?;
    Ok(())
}

/// 퍼즐 타입별 정답률 → 약점 분석.
/// 반환: (puzzle_type, 시도수, 정답수)
pub async fn user_weakness(pool: &PgPool, puuid: &str) -> Result<Vec<(String, i64, i64)>> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        r#"
        SELECT p.puzzle_type,
               COUNT(*)::int8                                   AS attempts,
               SUM(CASE WHEN a.correct THEN 1 ELSE 0 END)::int8 AS correct
        FROM puzzle_attempts a
        JOIN puzzles p ON p.id = a.puzzle_id
        WHERE a.user_puuid = $1
        GROUP BY p.puzzle_type
        ORDER BY attempts DESC
        "#,
    )
    .bind(puuid)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 주어진 match_id들 중 이미 DB에 있는 것만 골라낸다.
pub async fn existing_match_ids(
    pool: &PgPool,
    ids: &[String],
) -> Result<std::collections::HashSet<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT match_id FROM raw_matches WHERE match_id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

// ───────────────────────── 패치 추적 ─────────────────────────

/// patch_versions를 raw_matches로부터 재계산(upsert).
/// first_detected_at은 ON CONFLICT의 UPDATE SET에 없으므로 최초값이 유지된다.
pub async fn reconcile_patch_versions(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO patch_versions (patch, set_number, earliest_game_datetime, match_count, last_seen_at)
        SELECT patch,
               MAX(set_number),           -- patch당 대표 set 하나로 집약
               MIN(game_datetime),
               COUNT(*)::int,
               now()
        FROM raw_matches
        GROUP BY patch                     -- set_number 제거 → patch당 정확히 한 행
        ON CONFLICT (patch) DO UPDATE SET
            earliest_game_datetime = LEAST(patch_versions.earliest_game_datetime, EXCLUDED.earliest_game_datetime),
            match_count            = EXCLUDED.match_count,
            last_seen_at           = now()
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PatchInfo {
    pub patch: String,
    pub set_number: i32,
    pub age_days: f64,
    pub match_count: i32,
}

/// 현재 패치(가장 최근 게임이 있는 버전)와 나이 정보.
pub async fn current_patch_info(pool: &PgPool) -> Result<Option<PatchInfo>> {
    let row: Option<(String, i32, Option<f64>, i32)> = sqlx::query_as(
        r#"
        SELECT patch,
               set_number,
               (EXTRACT(EPOCH FROM (now() - earliest_game_datetime)) / 86400.0)::float8,
               match_count
        FROM patch_versions
        ORDER BY earliest_game_datetime DESC NULLS LAST
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(patch, set_number, age, match_count)| PatchInfo {
        patch,
        set_number,
        age_days: age.unwrap_or(0.0),
        match_count,
    }))
}

// ───────────────────────── 아이템 BIS 집계 ─────────────────────────

/// 퀴즈 소재가 될 캐리 후보. 아이템 2개 이상 든 채로 자주 등장한 유닛.
/// 반환: (character_id, 등장 횟수)
pub async fn carry_candidates(
    pool: &PgPool,
    set_number: i32,
    patch: &str,
    min_appearances: i64,
) -> Result<Vec<(String, i64)>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        r#"
        SELECT u->>'character_id' AS carry,
               COUNT(*)::int8     AS appearances
        FROM raw_matches m,
             jsonb_array_elements(m.raw->'info'->'participants') AS p,
             jsonb_array_elements(p->'units')                    AS u
        WHERE m.set_number = $1
          AND m.patch = $2
          AND jsonb_array_length(u->'itemNames') >= 3
          AND EXISTS (
            SELECT 1
            FROM jsonb_array_elements_text(u->'itemNames') AS it
            JOIN item_classifications ic ON ic.item_id = it
            WHERE ic.is_damage = true
          )
        GROUP BY carry
        HAVING COUNT(*) >= $3
        ORDER BY appearances DESC
        "#,
    )
    .bind(set_number)
    .bind(patch)
    .bind(min_appearances)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

#[derive(Debug, Clone)]
pub struct ItemStat {
    pub item: String,
    pub avg_placement: f64,
    pub picks: i64,
}

/// 특정 캐리가 든 아이템별 평균 등수 (낮을수록 BIS).
pub async fn item_stats_for_carry(
    pool: &PgPool,
    set_number: i32,
    patch: &str,
    character_id: &str,
    min_picks: i64,
) -> Result<Vec<ItemStat>> {
    let rows: Vec<(String, f64, i64)> = sqlx::query_as(
        r#"
        SELECT item,
               AVG(placement)::float8 AS avg_place,
               COUNT(*)::int8         AS picks
        FROM (
          SELECT (p->>'placement')::int                     AS placement,
                 jsonb_array_elements_text(u->'itemNames')  AS item
          FROM raw_matches m,
               jsonb_array_elements(m.raw->'info'->'participants') AS p,
               jsonb_array_elements(p->'units')                    AS u
          WHERE m.set_number = $1
            AND m.patch = $2
            AND u->>'character_id' = $3
            AND jsonb_array_length(u->'itemNames') >= 2
        ) t
        JOIN item_classifications ic ON ic.item_id = t.item   -- ★ 추가: 정상 완성템만
        GROUP BY item
        HAVING COUNT(*) >= $4
        ORDER BY avg_place ASC
        "#,
    )
    .bind(set_number)
    .bind(patch)
    .bind(character_id)
    .bind(min_picks)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(item, avg_placement, picks)| ItemStat {
            item,
            avg_placement,
            picks,
        })
        .collect())
}

// ───────────────────────── 아이템 분류 ─────────────────────────

pub async fn upsert_item_classification(
    pool: &PgPool,
    item_id: &str,
    name: &str,
    category: &str,
    is_damage: bool,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO item_classifications (item_id, name, category, is_damage)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (item_id) DO UPDATE
          SET name = EXCLUDED.name,
              category = EXCLUDED.category,
              is_damage = EXCLUDED.is_damage
        "#,
    )
    .bind(item_id)
    .bind(name)
    .bind(category)
    .bind(is_damage)
    .execute(pool)
    .await?;
    Ok(())
}


// ───────────────────────── 아이템 퀴즈 조회 ─────────────────────────

/// 표본이 충분한(정답 표본 >= min_sample) 아이템 퀴즈를 랜덤으로 하나.
pub async fn random_item_puzzle(
    pool: &PgPool,
    min_sample: i64,
) -> Result<Option<PuzzleRow>> {
    let row: Option<PuzzleRow> = sqlx::query_as(
        r#"
        SELECT id, puzzle_type, patch, set_number, prompt, options, stats
        FROM puzzles
        WHERE puzzle_type = 'item_combine'
          AND (
            SELECT (o->>'sample_size')::int
            FROM jsonb_array_elements(stats->'options') o
            WHERE (o->>'is_best')::bool = true
          ) >= $1
        ORDER BY random()
        LIMIT 1
        "#,
    )
    .bind(min_sample)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}