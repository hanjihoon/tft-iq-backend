//! DB 접근 계층. sqlx 런타임 쿼리(컴파일타임 매크로 X)를 써서
//! DB 없이도 컴파일된다. 안정화되면 query! 매크로로 바꿔 타입 검증을 강화하면 좋다.

use crate::error::Result;
use crate::riot::dto::Match;
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions};
use uuid::Uuid;


const MIN_PATCH_MATCHES: i64 = 2000; // 이 매치 수 넘어야 "현재 패치"로 전환 (자동 지연)

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
        WHERE match_count >= $1
        ORDER BY earliest_game_datetime DESC NULLS LAST
        LIMIT 1
        "#,
    )
    .bind(MIN_PATCH_MATCHES)
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
pub struct ItemFull {
    pub item: String,
    pub name: String,
    pub avg_placement: f64,
    pub picks: i64,
    pub damage_type: String,
    pub carry_mean: f64,
    pub lift: f64,
    pub icon_url: String,
}

/// 캐리의 아이템별 통계 + lift (시그니처 정도).
/// lift = (이 캐리가 이 아이템 들 비율) / (전체에서 이 아이템 들 비율)
pub async fn carry_item_full(
    pool: &PgPool,
    set_number: i32,
    patch: &str,
    character_id: &str,
    min_picks: i64,
) -> Result<Vec<ItemFull>> {
    let rows: Vec<(String, String, f64, i64, String, f64, f64, String)> = sqlx::query_as(
        r#"
        WITH carry_rows AS (
          SELECT (p->>'placement')::int                    AS placement,
                 jsonb_array_elements_text(u->'itemNames') AS item
          FROM raw_matches m,
               jsonb_array_elements(m.raw->'info'->'participants') AS p,
               jsonb_array_elements(p->'units')                    AS u
          WHERE m.set_number = $1 AND m.patch = $2
            AND u->>'character_id' = $3
            AND jsonb_array_length(u->'itemNames') >= 3
        ),
        carry_total AS (SELECT COUNT(*)::float8 AS n FROM carry_rows),
        all_rows AS (
          SELECT jsonb_array_elements_text(u->'itemNames') AS item
          FROM raw_matches m,
               jsonb_array_elements(m.raw->'info'->'participants') AS p,
               jsonb_array_elements(p->'units')                    AS u
          WHERE m.set_number = $1 AND m.patch = $2
            AND jsonb_array_length(u->'itemNames') >= 3
        ),
        all_total AS (SELECT COUNT(*)::float8 AS n FROM all_rows),
        item_overall AS (
          SELECT item, COUNT(*)::float8 AS appears FROM all_rows GROUP BY item
        )
        SELECT t.item, ic.name,
               AVG(t.placement)::float8 AS avg_place,
               COUNT(*)::int8           AS picks,
               ic.damage_type,
               (SELECT AVG(placement)::float8 FROM carry_rows) AS carry_mean,
               ((COUNT(*)::float8 / (SELECT n FROM carry_total)) /
                NULLIF(io.appears / (SELECT n FROM all_total), 0)) AS lift,
               COALESCE(ic.icon_url, '') AS icon_url
        FROM carry_rows t
        JOIN item_classifications ic ON ic.item_id = t.item
        JOIN item_overall io ON io.item = t.item
        GROUP BY t.item, ic.name, ic.damage_type, io.appears, ic.icon_url
        HAVING COUNT(*) >= $4
        ORDER BY avg_place ASC
        "#,
    )
    .bind(set_number).bind(patch).bind(character_id).bind(min_picks)
    .fetch_all(pool).await?;

    Ok(rows.into_iter().map(|(item, name, avg_placement, picks, damage_type, carry_mean, lift, icon_url)| ItemFull {
        item, name, avg_placement, picks, damage_type, carry_mean, lift, icon_url,
    }).collect())
}

// ───────────────────────── 아이템 분류 ─────────────────────────

pub async fn upsert_item_classification(
    pool: &PgPool,
    item_id: &str,
    name: &str,
    category: &str,
    is_damage: bool,
    damage_type: &str,   // 추가
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO item_classifications (item_id, name, category, is_damage, damage_type)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (item_id) DO UPDATE
          SET name = EXCLUDED.name,
              category = EXCLUDED.category,
              is_damage = EXCLUDED.is_damage,
              damage_type = EXCLUDED.damage_type
        "#,
    )
    .bind(item_id)
    .bind(name)
    .bind(category)
    .bind(is_damage)
    .bind(damage_type)   // 추가
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

/// 현재 패치(가장 최근)의 16.x 매치 수. 티어 풀 결정에 쓴다.
pub async fn current_patch_match_count(pool: &PgPool) -> Result<i64> {
    let row: Option<(i64,)> = sqlx::query_as(
        r#"
        SELECT match_count::int8
        FROM patch_versions
        WHERE match_count >= $1
        ORDER BY earliest_game_datetime DESC NULLS LAST
        LIMIT 1
        "#,
    )
    .bind(MIN_PATCH_MATCHES)
    .fetch_optional(pool)
    .await?;
    // 패치 데이터가 아직 없으면 0
    Ok(row.map(|(c,)| c).unwrap_or(0))
}

/// 현재 패치의 "조회 시작 시각"(epoch seconds).
/// earliest_game_datetime에서 안전 마진(12시간)을 빼서 경계 누락을 막는다.
/// 패치 데이터가 아직 없으면 None → 첫 수집은 startTime 없이 돌린다.
pub async fn current_patch_start_time(pool: &PgPool) -> Result<Option<i64>> {
    let row: Option<(Option<i64>,)> = sqlx::query_as(
        r#"
        SELECT (EXTRACT(EPOCH FROM earliest_game_datetime)::int8 - 12 * 3600)
        FROM patch_versions
        WHERE match_count >= $1
        ORDER BY earliest_game_datetime DESC NULLS LAST
        LIMIT 1
        "#,
    )
    .bind(MIN_PATCH_MATCHES)
    .fetch_optional(pool)
    .await?;

    // 이중 Option: 행이 없거나(fetch_optional None), earliest가 NULL이거나(내부 None)
    Ok(row.and_then(|(t,)| t))
}

/// 플레이어의 마지막 수집 시각을 epoch seconds로. 아직 없으면 None.
pub async fn player_last_crawled_epoch(pool: &PgPool, puuid: &str) -> Result<Option<i64>> {
    let row: Option<(Option<i64>,)> = sqlx::query_as(
        "SELECT EXTRACT(EPOCH FROM last_crawled_at)::int8 FROM tracked_players WHERE puuid = $1",
    )
    .bind(puuid)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(t,)| t))
}


/// 캐리가 자주 든 아이템의 damage_type 분포 (BIS 상위 N개 기준).
/// 반환: (damage_type, 평균등수, 픽수) 리스트 — 평균등수 오름차순.
pub async fn carry_item_types(
    pool: &PgPool,
    set_number: i32,
    patch: &str,
    character_id: &str,
    min_picks: i64,
    top_n: i64,
) -> Result<Vec<(String, f64, i64)>> {
    let rows: Vec<(String, f64, i64)> = sqlx::query_as(
        r#"
        SELECT ic.damage_type,
               AVG(t.placement)::float8 AS avg_place,
               COUNT(*)::int8           AS picks
        FROM (
          SELECT (p->>'placement')::int                    AS placement,
                 jsonb_array_elements_text(u->'itemNames') AS item
          FROM raw_matches m,
               jsonb_array_elements(m.raw->'info'->'participants') AS p,
               jsonb_array_elements(p->'units')                    AS u
          WHERE m.set_number = $1
            AND m.patch = $2
            AND u->>'character_id' = $3
            AND jsonb_array_length(u->'itemNames') >= 2
        ) t
        JOIN item_classifications ic ON ic.item_id = t.item
        GROUP BY ic.damage_type, t.item
        HAVING COUNT(*) >= $4
        ORDER BY avg_place ASC
        LIMIT $5
        "#,
    )
    .bind(set_number)
    .bind(patch)
    .bind(character_id)
    .bind(min_picks)
    .bind(top_n)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CarryType {
    Dealer,
    Bruiser,
    Tank,
}

/// BIS 상위 아이템의 계열 구성으로 캐리 타입 판정.
/// types: carry_item_types가 준 (damage_type, avg_place, picks) 상위 리스트.
pub fn classify_carry(types: &[(String, f64, i64)]) -> CarryType {
    let mut dmg = 0i64;
    let mut bruiser = 0i64;
    let mut tank = 0i64;

    // 평균등수 상위 N개가 아니라, 계열별 픽 수를 전부 합산
    for (dt, _, picks) in types {
        match dt.as_str() {
            "ad" | "ap" | "mixed" => dmg += picks,
            "bruiser" => bruiser += picks,
            "tank" => tank += picks,
            _ => {} // utility 중립
        }
    }

    let total = dmg + bruiser + tank;
    if total == 0 {
        return CarryType::Tank;
    }

    // 브루저 비중이 의미있게 높으면(25%+) 브루저로 — 딜+탱 겸용 강조
    if bruiser * 100 / total >= 25 {
        CarryType::Bruiser
    } else if tank > dmg && tank > bruiser {
        CarryType::Tank // 탱이 압도적 → 제외
    } else {
        CarryType::Dealer
    }
}

/// 지정한 계열들의 모든 분류된 아이템 (오답 풀 보충용).
pub async fn items_by_damage_type(
    pool: &PgPool,
    types: &[String],
) -> Result<Vec<(String, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT item_id, name FROM item_classifications WHERE damage_type = ANY($1)",
    )
    .bind(types)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn insert_item_puzzle(
    pool: &PgPool,
    set_number: i32,
    patch: &str,
    carry_id: &str,
    carry_type: &str,
    prompt: &serde_json::Value,
    options: &serde_json::Value,
    answer: &str,
    stats: &serde_json::Value,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO puzzles
            (puzzle_type, set_number, patch, carry_id, variant, carry_type,
             prompt, options, answer, stats, source_match_id)
        VALUES ('item_combine', $1, $2, $3, 'bis', $4, $5, $6, $7, $8, NULL)
        ON CONFLICT (puzzle_type, carry_id, patch, variant) WHERE carry_id IS NOT NULL
        DO UPDATE SET
            carry_type = EXCLUDED.carry_type,
            prompt     = EXCLUDED.prompt,
            options    = EXCLUDED.options,
            answer     = EXCLUDED.answer,
            stats      = EXCLUDED.stats,
            set_number = EXCLUDED.set_number
        "#,
    )
    .bind(set_number).bind(patch).bind(carry_id).bind(carry_type)
    .bind(prompt).bind(options).bind(answer).bind(stats)
    .execute(pool).await?;
    Ok(())
}

/// 시도 기록 (정답 여부 포함).
pub async fn record_attempt(
    pool: &PgPool,
    user_id: &str,
    puzzle_id: uuid::Uuid,
    chosen: &str,
    correct: bool,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO puzzle_attempts (user_id, puzzle_id, chosen, correct) VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(puzzle_id)
    .bind(chosen)
    .bind(correct)
    .execute(pool)
    .await?;
    Ok(())
}

/// 이 유저가 아직 안 푼 아이템 퍼즐 하나 (랜덤).
/// 다 풀었으면 None → 프론트가 "다 풀었어요" 처리.
pub async fn unsolved_item_puzzle(
    pool: &PgPool,
    user_id: &str,
    min_sample: i64,
) -> Result<Option<PuzzleRow>> {
    let row: Option<PuzzleRow> = sqlx::query_as(
        r#"
        SELECT id, puzzle_type, patch, set_number, prompt, options, stats
        FROM puzzles p
        WHERE p.puzzle_type = 'item_combine'
          AND (
            SELECT (o->>'sample_size')::int
            FROM jsonb_array_elements(p.stats->'options') o
            WHERE (o->>'is_best')::bool = true
          ) >= $2
          AND NOT EXISTS (
            SELECT 1 FROM puzzle_attempts a
            WHERE a.user_id = $1 AND a.puzzle_id = p.id
          )
        ORDER BY random()
        LIMIT 1
        "#,
    )
    .bind(user_id)
    .bind(min_sample)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 전역 메타 정보 (분석 표본 출처).
pub struct MetaInfo {
    pub patch: String,
    pub total_matches: i64,
    pub puzzle_count: i64,
}

pub async fn meta_info(pool: &PgPool) -> Result<MetaInfo> {
    let (patch, total): (String, i64) = sqlx::query_as(
        r#"SELECT patch, match_count::int8 FROM patch_versions
           ORDER BY earliest_game_datetime DESC NULLS LAST LIMIT 1"#,
    )
    .fetch_optional(pool)
    .await?
    .unwrap_or_else(|| ("?".into(), 0));

    let (pc,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::int8 FROM puzzles WHERE puzzle_type='item_combine'",
    )
    .fetch_one(pool)
    .await?;

    Ok(MetaInfo { patch, total_matches: total, puzzle_count: pc })
}

pub async fn update_item_icon(pool: &PgPool, item_id: &str, icon_url: &str) -> Result<()> {
    sqlx::query("UPDATE item_classifications SET icon_url = $2 WHERE item_id = $1")
        .bind(item_id).bind(icon_url).execute(pool).await?;
    Ok(())
}

/// 덱 하나의 원시 통계. 변형 흡수 전 단계.
#[derive(Debug, Clone)]
pub struct RawDeck {
    pub units: Vec<String>,   // 고유 유닛 id 목록 (정렬됨)
    pub games: i64,
    pub avg_placement: f64,
}

/// 8기물 덱별 통계 (소환물/타세트 제외, 중복 유닛 합침).
/// 변형 흡수는 이 결과를 받아 Rust에서 처리한다.
pub async fn raw_decks(
    pool: &PgPool,
    patch: &str,
    min_games: i64,
) -> Result<Vec<RawDeck>> {
    let rows: Vec<(String, i64, f64)> = sqlx::query_as(
        r#"
        SELECT deck, COUNT(*)::int8 AS games, ROUND(AVG(place), 2)::float8 AS avg_place
        FROM (
          SELECT (p->>'placement')::int AS place,
                 (SELECT string_agg(DISTINCT u->>'character_id', ',' ORDER BY u->>'character_id')
                  FROM jsonb_array_elements(p->'units') u
                  WHERE u->>'character_id' LIKE 'TFT17_%'
                    AND u->>'character_id' NOT LIKE '%Summon%'
                    AND u->>'character_id' NOT LIKE '%Minion%') AS deck,
                 (SELECT COUNT(DISTINCT u->>'character_id')
                  FROM jsonb_array_elements(p->'units') u
                  WHERE u->>'character_id' LIKE 'TFT17_%'
                    AND u->>'character_id' NOT LIKE '%Summon%'
                    AND u->>'character_id' NOT LIKE '%Minion%') AS unit_count
          FROM raw_matches m,
               jsonb_array_elements(m.raw->'info'->'participants') AS p
          WHERE m.patch = $1
        ) sub
        WHERE unit_count = 8 AND deck IS NOT NULL
        GROUP BY deck
        HAVING COUNT(*) >= $2
        ORDER BY avg_place ASC
        "#,
    )
    .bind(patch)
    .bind(min_games)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(deck, games, avg_placement)| RawDeck {
            units: deck.split(',').map(|s| s.to_string()).collect(),
            games,
            avg_placement,
        })
        .collect())
}

/// 각 유닛의 전체 등장 보드 수 + 전체 보드 수. (정답 필터용)
pub async fn unit_appearance_rates(
    pool: &PgPool,
    patch: &str,
) -> Result<(std::collections::HashMap<String, i64>, i64)> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        r#"
        SELECT u->>'character_id', COUNT(*)::int8
        FROM raw_matches m,
             jsonb_array_elements(m.raw->'info'->'participants') AS p,
             jsonb_array_elements(p->'units') AS u
        WHERE m.patch = $1 AND u->>'character_id' LIKE 'TFT17_%'
        GROUP BY u->>'character_id'
        "#,
    )
    .bind(patch)
    .fetch_all(pool)
    .await?;

    let (total,): (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*)::int8 FROM raw_matches m,
           jsonb_array_elements(m.raw->'info'->'participants') AS p WHERE m.patch = $1"#,
    )
    .bind(patch)
    .fetch_one(pool)
    .await?;

    Ok((rows.into_iter().collect(), total))
}

/// 덱(유닛 목록)에서 캐리를 찾는다.
/// 캐리 = 이 덱 보드들에서 아이템을 가장 많이(평균) 든 유닛.
pub async fn deck_carry(
    pool: &PgPool,
    patch: &str,
    units: &[String],
) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        r#"
        SELECT u->>'character_id' AS unit
        FROM raw_matches m,
             jsonb_array_elements(m.raw->'info'->'participants') AS p,
             jsonb_array_elements(p->'units') AS u
        WHERE m.patch = $1
          AND u->>'character_id' = ANY($2)
        GROUP BY u->>'character_id'
        ORDER BY SUM((
          SELECT COUNT(*)
          FROM jsonb_array_elements_text(u->'itemNames') AS it
          JOIN item_classifications ic ON ic.item_id = it
          WHERE ic.damage_type IN ('ad','ap','mixed')
        )) DESC
        LIMIT 1
        "#,
    )
    .bind(patch)
    .bind(units)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(u,)| u))
}

/// 덱 완성 퍼즐 upsert. (덱키 + 뺀유닛)으로 정체성 유지 → 재생성해도 id 안정.
pub async fn insert_deck_puzzle(
    pool: &PgPool,
    set_number: i32,
    patch: &str,
    deck_key: &str,      // carry_id 컬럼에 저장 (덱 정체성 = 정렬된 코어)
    removed_unit: &str,  // variant 컬럼에 저장 (뺀 유닛)
    prompt: &serde_json::Value,
    options: &serde_json::Value,
    answer: &str,
    stats: &serde_json::Value,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO puzzles
            (puzzle_type, set_number, patch, carry_id, variant, carry_type,
             prompt, options, answer, stats, source_match_id)
        VALUES ('deck_complete', $1, $2, $3, $4, 'deck', $5, $6, $7, $8, NULL)
        ON CONFLICT (puzzle_type, carry_id, patch, variant) WHERE carry_id IS NOT NULL
        DO UPDATE SET
            prompt     = EXCLUDED.prompt,
            options    = EXCLUDED.options,
            answer     = EXCLUDED.answer,
            stats      = EXCLUDED.stats,
            set_number = EXCLUDED.set_number
        "#,
    )
    .bind(set_number).bind(patch).bind(deck_key).bind(removed_unit)
    .bind(prompt).bind(options).bind(answer).bind(stats)
    .execute(pool).await?;
    Ok(())
}

pub async fn insert_flex_puzzle(
    pool: &PgPool, set_number: i32, patch: &str, deck_key: &str,
    prompt: &serde_json::Value, options: &serde_json::Value,
    answer: &str, stats: &serde_json::Value,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO puzzles
            (puzzle_type, set_number, patch, carry_id, variant, carry_type,
             prompt, options, answer, stats, source_match_id)
        VALUES ('deck_flex', $1, $2, $3, 'flex', 'deck', $4, $5, $6, $7, NULL)
        ON CONFLICT (puzzle_type, carry_id, patch, variant) WHERE carry_id IS NOT NULL
        DO UPDATE SET prompt=EXCLUDED.prompt, options=EXCLUDED.options,
            answer=EXCLUDED.answer, stats=EXCLUDED.stats, set_number=EXCLUDED.set_number
        "#,
    )
    .bind(set_number).bind(patch).bind(deck_key)
    .bind(prompt).bind(options).bind(answer).bind(stats)
    .execute(pool).await?;
    Ok(())
}

/// 안 푼 퍼즐 하나. puzzle_type 을 지정하면 그 유형만.
pub async fn unsolved_puzzle_by_type(
    pool: &PgPool,
    user_id: &str,
    puzzle_type: &str,
    patch: &str,
) -> Result<Option<PuzzleRow>> {
    let row: Option<PuzzleRow> = sqlx::query_as(
        r#"
        SELECT id, puzzle_type, patch, set_number, prompt, options, stats
        FROM puzzles p
        WHERE p.puzzle_type = $2
          AND ($2 = 'trait_quiz' OR p.patch = $3)     
          AND NOT EXISTS (
            SELECT 1 FROM puzzle_attempts a
            WHERE a.user_id = $1 AND a.puzzle_id = p.id
          )
        ORDER BY random()
        LIMIT 1
        "#,
    )
    .bind(user_id)
    .bind(puzzle_type)
    .bind(patch)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 덱(코어 유닛들이 모두 포함된 보드)의 대표 특성 apiName.
/// num_units>=2 이고 style(활성등급) 최고인 특성을 보드마다 뽑아 최빈값.
pub async fn deck_signature_trait(
    pool: &PgPool,
    patch: &str,
    core_units: &[String],
) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        r#"
        WITH core_boards AS (
          SELECT p
          FROM raw_matches m,
               jsonb_array_elements(m.raw->'info'->'participants') AS p
          WHERE m.patch = $1
            AND (
              SELECT bool_and(cu = ANY(
                ARRAY(SELECT u->>'character_id' FROM jsonb_array_elements(p->'units') u)
              ))
              FROM unnest($2::text[]) AS cu
            )
        ),
        -- 각 보드의 "대표 특성" (num_units>=2 중 style 최고 1개)
        board_top_trait AS (
          SELECT (
            SELECT t->>'name'
            FROM jsonb_array_elements(cb.p->'traits') t
            WHERE (t->>'num_units')::int >= 2
            ORDER BY (t->>'style')::int DESC, (t->>'num_units')::int DESC
            LIMIT 1
          ) AS trait_name
          FROM core_boards cb
        )
        SELECT trait_name FROM board_top_trait
        WHERE trait_name IS NOT NULL
        GROUP BY trait_name
        ORDER BY COUNT(*) DESC
        LIMIT 1
        "#,
    )
    .bind(patch)
    .bind(core_units)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(t,)| t))
}


/// 복습 대상: 이 유저가 마지막으로 틀린 문제 (그 뒤 안 맞힘).
/// 같은 문제를 여러 번 풀었으면 가장 최근 시도 기준.
pub async fn review_puzzle(
    pool: &PgPool,
    user_id: &str,
    puzzle_type: &str,
    patch: &str,
) -> Result<Option<PuzzleRow>> {
    let row: Option<PuzzleRow> = sqlx::query_as(
        r#"
        SELECT p.id, p.puzzle_type, p.patch, p.set_number, p.prompt, p.options, p.stats
        FROM puzzles p
        WHERE p.puzzle_type = $2
        AND p.patch = $3 
          AND EXISTS (
            -- 이 퍼즐의 가장 최근 시도가 '틀림'인 경우
            SELECT 1 FROM puzzle_attempts a
            WHERE a.user_id = $1 AND a.puzzle_id = p.id
              AND a.correct = false
              AND a.created_at = (
                SELECT MAX(a2.created_at) FROM puzzle_attempts a2
                WHERE a2.user_id = $1 AND a2.puzzle_id = p.id
              )
          )
        ORDER BY random()
        LIMIT 1
        "#,
    )
    .bind(user_id)
    .bind(puzzle_type)
    .bind(patch)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 복습 대상 개수
pub async fn review_count(pool: &PgPool, user_id: &str, puzzle_type: &str) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)::int8 FROM puzzles p
        WHERE p.puzzle_type = $2
          AND EXISTS (
            SELECT 1 FROM puzzle_attempts a
            WHERE a.user_id = $1 AND a.puzzle_id = p.id AND a.correct = false
              AND a.created_at = (
                SELECT MAX(a2.created_at) FROM puzzle_attempts a2
                WHERE a2.user_id = $1 AND a2.puzzle_id = p.id
              )
          )
        "#,
    ).bind(user_id).bind(puzzle_type).fetch_one(pool).await?;
    Ok(n)
}

/// 유저의 학습 통계: 전체/유형별 정답률 + 약점(자주 틀리는 그룹).
pub async fn user_stats(pool: &PgPool, user_id: &str) -> Result<serde_json::Value> {
    // 1) 유형별 정답률 + 총계
    let type_rows: Vec<(String, i64, i64)> = sqlx::query_as(
        r#"
        SELECT p.puzzle_type,
               COUNT(*)::int8 AS total,
               COUNT(*) FILTER (WHERE a.correct)::int8 AS correct
        FROM puzzle_attempts a
        JOIN puzzles p ON p.id = a.puzzle_id
        WHERE a.user_id = $1
        GROUP BY p.puzzle_type
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    // 2) 약점: 그룹(덱=deck_label, 아이템=carry명)별 정답률 낮은 순 TOP 5
    //    최소 2회 이상 시도한 그룹만 (표본)
    let weak_rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
        r#"
        SELECT p.puzzle_type,
               COALESCE(
                 p.prompt->>'deck_label',
                 p.prompt->'carry'->>'name'
               ) AS grp,
               COUNT(*)::int8 AS total,
               COUNT(*) FILTER (WHERE a.correct)::int8 AS correct
        FROM puzzle_attempts a
        JOIN puzzles p ON p.id = a.puzzle_id
        WHERE a.user_id = $1
        GROUP BY p.puzzle_type, grp
        HAVING COUNT(*) >= 2
           AND (COUNT(*) FILTER (WHERE a.correct))::float8 / COUNT(*) < 0.7
        ORDER BY (COUNT(*) FILTER (WHERE a.correct))::float8 / COUNT(*) ASC
        LIMIT 5
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let by_type: Vec<serde_json::Value> = type_rows.iter().map(|(t, total, correct)| {
        serde_json::json!({
            "type": t, "total": total, "correct": correct,
            "rate": if *total > 0 { (*correct as f64 / *total as f64 * 100.0).round() } else { 0.0 },
        })
    }).collect();

    let total: i64 = type_rows.iter().map(|(_, t, _)| t).sum();
    let correct: i64 = type_rows.iter().map(|(_, _, c)| c).sum();

    let weak: Vec<serde_json::Value> = weak_rows.iter().map(|(t, grp, total, correct)| {
        serde_json::json!({
            "type": t, "group": grp, "total": total, "correct": correct,
            "rate": (*correct as f64 / *total as f64 * 100.0).round(),
        })
    }).collect();

    Ok(serde_json::json!({
        "total": total,
        "correct": correct,
        "rate": if total > 0 { (correct as f64 / total as f64 * 100.0).round() } else { 0.0 },
        "by_type": by_type,
        "weak": weak,
    }))
}

pub async fn reset_attempts(pool: &PgPool, user_id: &str) -> Result<u64> {
    let r = sqlx::query("DELETE FROM puzzle_attempts WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(r.rows_affected())
}

/// 매치 수 상한 유지: 초과분을 [이전 패치 → 오래된 순]으로 삭제.
/// 티어 컬럼이 없지만, 수집 정책상 오래된 매치가 곧 저티어라
/// 시간순 삭제가 사실상 저티어 우선 삭제로 동작한다.
pub async fn prune_matches(pool: &PgPool, keep: i64, current_patch: &str) -> Result<u64> {
    let r = sqlx::query(
        r#"
        DELETE FROM raw_matches
        WHERE match_id IN (
            SELECT match_id FROM raw_matches
            ORDER BY (patch = $1) ASC,   -- 현재 패치는 뒤로(보존), 이전 패치 먼저 삭제
                     game_datetime ASC    -- 오래된 것 먼저 삭제
            OFFSET $2                      -- 상위 keep개 보존, 나머지 삭제
        )
        "#,
    )
    .bind(current_patch)
    .bind(keep)
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// 현재 패치를 특정 못 할 때: 순수 시간순으로만 정리
pub async fn prune_matches_by_time(pool: &PgPool, keep: i64) -> Result<u64> {
    let r = sqlx::query(
        r#"
        DELETE FROM raw_matches
        WHERE match_id IN (
            SELECT match_id FROM raw_matches
            ORDER BY game_datetime ASC
            OFFSET $1
        )
        "#,
    )
    .bind(keep)
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// 전체 매치 수
pub async fn total_match_count(pool: &PgPool) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*)::int8 FROM raw_matches")
        .fetch_one(pool)
        .await?;
    Ok(n)
}


pub async fn insert_trait_puzzle(
    pool: &PgPool,
    puzzle_type: &str,
    patch: &str,
    set_number: i32,
    unit_id: &str,
    answer: &str,              // 추가: "우주 그루브,저격수"
    prompt: &serde_json::Value,
    stats: &serde_json::Value,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO puzzles (puzzle_type, patch, set_number, carry_id, variant, answer, prompt, options, stats)
        VALUES ($1, $2, $3, $4, '0', $5, $6, '[]'::jsonb, $7)
        ON CONFLICT (puzzle_type, carry_id, patch, variant) WHERE carry_id IS NOT NULL
        DO UPDATE SET
            answer = EXCLUDED.answer,
            prompt = EXCLUDED.prompt,
            stats = EXCLUDED.stats
        "#,
    )
    .bind(puzzle_type)
    .bind(patch)
    .bind(set_number)
    .bind(unit_id)
    .bind(answer)          // $5
    .bind(prompt)          // $6
    .bind(stats)           // $7
    .execute(pool)
    .await?;
    Ok(())
}