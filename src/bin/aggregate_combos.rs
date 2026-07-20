// src/bin/aggregate_combos.rs
use std::collections::{HashMap, HashSet};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tft_iq::{Config, db};

const MIN_CARRY_RATE: f64 = 0.20;   // 3템 낀 비율 (캐리 판정)
const MIN_TANK_ITEMS: i32 = 2;      // 탱커 판정 (tank 아이템 2+)
const LOAD_LIMIT: i64 = 1_000_000;  // 전체 로드

struct ComboAgg {
    pick_count: i64,
    placement_sum: i64,
    tank_item_count: i32,
}

struct CarryAgg {
    total_appearances: i64,
    three_item_count: i64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;

    let args: Vec<String> = std::env::args().collect();

    let after: Option<DateTime<Utc>> = args.get(1)
        .and_then(|s| s.parse().ok());

    let Some(info) = db::current_patch_info(&pool).await? else {
        eprintln!("패치 데이터 없음.");
        return Ok(());
    };
    let (set_number, patch) = (info.set_number, info.patch.clone());
    eprintln!("집계 대상: set {set_number}, patch {patch}");

    let craftable_items: HashSet<String> = 
    sqlx::query_scalar("SELECT item_id FROM item_classifications")
    .fetch_all(&pool).await?.into_iter().collect();

    // tank 아이템 로드 (item_names랑 같은 형식)
    let tank_list = db::items_by_damage_type(&pool, &["tank".to_string()]).await?;
    let tank_items: HashSet<String> = tank_list.into_iter().map(|(id, _)| id).collect();
    eprintln!("tank 아이템 {}종", tank_items.len());

    let matches; 

    // raw 매치 로드
    match after {
        Some(dt) => {
            eprintln!("증분 집계: {dt} 이후");
            matches = db::load_matches_after(&pool, set_number, &patch, dt).await?;
            // 집계 → upsert (누적)
        }
        None => {
            eprintln!("전체 재집계");
            matches = db::load_matches(&pool, set_number, &patch, LOAD_LIMIT).await?;
            // 집계 → delete + insert (전체)
        }
    }
    eprintln!("매치 {}개 순회", matches.len());

    let mut combo_map: HashMap<(String, String), ComboAgg> = HashMap::new();
    let mut carry_map: HashMap<String, CarryAgg> = HashMap::new();

    for m in &matches {
        for p in &m.info.participants {
            let placement = p.placement as i64;
            for unit in &p.units {
                let carry_id = &unit.character_id;

                let carry = carry_map.entry(carry_id.clone())
                    .or_insert(CarryAgg { total_appearances: 0, three_item_count: 0 });
                carry.total_appearances += 1;

                // 정확히 3템
                if unit.item_names.len() == 3 {

                    let all_craftable = unit.item_names.iter()
                        .all(|i| craftable_items.contains(i));
                    if !all_craftable {
                        continue; // 특수템 낀 유닛은 스킵
                    }

                    carry.three_item_count += 1;

                    let mut items = unit.item_names.clone();
                    items.sort();
                    let combo = items.join(",");

                    let tank_count = items.iter()
                        .filter(|i| tank_items.contains(*i)).count() as i32;

                    let agg = combo_map.entry((carry_id.clone(), combo))
                        .or_insert(ComboAgg {
                            pick_count: 0, placement_sum: 0, tank_item_count: tank_count,
                        });
                    agg.pick_count += 1;
                    agg.placement_sum += placement;
                }
            }
        }
    }

    eprintln!("조합 {}종, 유닛 {}종 집계 완료", combo_map.len(), carry_map.len());

    // 저장
    match after {
        Some(dt) => {
            upsert_combo_stats(&pool, set_number, &patch, &combo_map).await?;
            upsert_carry_stats(&pool, set_number, &patch, &carry_map).await?;
        }
        None => {
            save_combo_stats(&pool, set_number, &patch, &combo_map).await?;
            save_carry_stats(&pool, set_number, &patch, &carry_map, &combo_map, &tank_items).await?;
        }
    }

    eprintln!("저장 완료");
    Ok(())
}

/// 조합 통계 저장
async fn save_combo_stats(
    pool: &PgPool, set_number: i32, patch: &str,
    combo_map: &HashMap<(String, String), ComboAgg>,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM item_combo_stats WHERE set_number = $1 AND patch = $2")
        .bind(set_number).bind(patch).execute(pool).await?;

    // 배치로 나눠서 (한 쿼리에 너무 많으면 파라미터 한계)
    const BATCH: usize = 3000;
    let entries: Vec<_> = combo_map.iter().collect();

    for chunk in entries.chunks(BATCH) {
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO item_combo_stats (set_number, patch, carry_id, item_combo, pick_count, placement_sum, avg_placement, tank_item_count) "
        );
        qb.push_values(chunk, |mut b, ((carry_id, combo), agg)| {
            let avg = agg.placement_sum as f64 / agg.pick_count as f64;
            b.push_bind(set_number)
             .push_bind(patch)
             .push_bind(carry_id)
             .push_bind(combo)
             .push_bind(agg.pick_count)
             .push_bind(agg.placement_sum)
             .push_bind(avg as f32)
             .push_bind(agg.tank_item_count);
        });
        qb.build().execute(pool).await?;
    }
    Ok(())
}

/// 캐리 통계 저장 (캐리/탱커 판정)
async fn save_carry_stats(
    pool: &PgPool, set_number: i32, patch: &str,
    carry_map: &HashMap<String, CarryAgg>,
    combo_map: &HashMap<(String, String), ComboAgg>,
    _tank_items: &HashSet<String>,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM carry_stats WHERE set_number = $1 AND patch = $2")
        .bind(set_number).bind(patch)
        .execute(pool).await?;

    for (carry_id, agg) in carry_map {
        let rate = agg.three_item_count as f64 / agg.total_appearances as f64;
        let is_carry = rate >= MIN_CARRY_RATE;

        // 탱커 판정: 이 캐리의 픽률 1위 조합의 tank_item_count >= 2
        let top_combo = combo_map.iter()
            .filter(|((cid, _), _)| cid == carry_id)
            .max_by_key(|(_, a)| a.pick_count);
        let is_tank = top_combo
            .map(|(_, a)| a.tank_item_count >= MIN_TANK_ITEMS)
            .unwrap_or(false);

        sqlx::query(
            r#"
            INSERT INTO carry_stats
                (set_number, patch, carry_id, total_appearances, three_item_count, three_item_rate, is_carry, is_tank)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(set_number).bind(patch).bind(carry_id)
        .bind(agg.total_appearances).bind(agg.three_item_count)
        .bind(rate as f32).bind(is_carry).bind(is_tank)
        .execute(pool).await?;
    }
    Ok(())
}

/// carry_stats 증분 UPSERT
/// - total_appearances, three_item_count는 누적 (기존 + 새 매치)
/// - three_item_rate, is_carry는 누적 후 재계산
/// - is_tank는 combo_stats(누적된 전체)의 픽률1위 조합으로 재계산
///
/// 주의: 이 함수는 upsert_combo_stats가 먼저 실행된 뒤에 호출해야 한다.
/// (is_tank가 누적된 combo_stats를 참조하기 때문)
async fn upsert_carry_stats(
    pool: &sqlx::PgPool,
    set_number: i32,
    patch: &str,
    carry_map: &std::collections::HashMap<String, CarryAgg>,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    // ── 1. total_appearances, three_item_count 누적 (UPSERT) ──
    // rate/is_carry/is_tank는 임시값으로 넣고, 아래 2·3단계에서 재계산.
    const BATCH: usize = 1000;
    let entries: Vec<(&String, &CarryAgg)> = carry_map.iter().collect();

    for chunk in entries.chunks(BATCH) {
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO carry_stats \
             (set_number, patch, carry_id, total_appearances, three_item_count, three_item_rate, is_carry, is_tank) ",
        );
        qb.push_values(chunk, |mut b, (carry_id, agg)| {
            // 신규 삽입 시 임시 rate (아래서 어차피 재계산됨)
            let tmp_rate = if agg.total_appearances > 0 {
                agg.three_item_count as f32 / agg.total_appearances as f32
            } else {
                0.0
            };
            b.push_bind(set_number)
                .push_bind(patch)
                .push_bind(carry_id.as_str())
                .push_bind(agg.total_appearances)
                .push_bind(agg.three_item_count)
                .push_bind(tmp_rate)
                .push_bind(false) // is_carry 임시
                .push_bind(false); // is_tank 임시
        });
        qb.push(
            " ON CONFLICT (set_number, patch, carry_id) DO UPDATE SET \
             total_appearances = carry_stats.total_appearances + EXCLUDED.total_appearances, \
             three_item_count  = carry_stats.three_item_count  + EXCLUDED.three_item_count",
        );
        qb.build().execute(&mut *tx).await?;
    }

    // ── 2. rate, is_carry 재계산 (누적된 값 기준, 통째로) ──
    sqlx::query(
        "UPDATE carry_stats SET \
         three_item_rate = three_item_count::real / NULLIF(total_appearances, 0), \
         is_carry = (three_item_count::real / NULLIF(total_appearances, 0)) >= $3 \
         WHERE set_number = $1 AND patch = $2",
    )
    .bind(set_number)
    .bind(patch)
    .bind(MIN_CARRY_RATE as f32)
    .execute(&mut *tx)
    .await?;

    // ── 3. is_tank 재계산: 각 캐리의 픽률1위 조합의 tank_item_count >= 2 ──
    sqlx::query(
        "UPDATE carry_stats cs SET is_tank = COALESCE(( \
             SELECT ics.tank_item_count >= $3 \
             FROM item_combo_stats ics \
             WHERE ics.set_number = cs.set_number \
               AND ics.patch = cs.patch \
               AND ics.carry_id = cs.carry_id \
             ORDER BY ics.pick_count DESC \
             LIMIT 1 \
         ), false) \
         WHERE cs.set_number = $1 AND cs.patch = $2",
    )
    .bind(set_number)
    .bind(patch)
    .bind(MIN_TANK_ITEMS)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

async fn upsert_combo_stats(
    pool: &PgPool, set_number: i32, patch: &str,
    combo_map: &HashMap<(String, String), ComboAgg>,) -> anyhow::Result<()> {

    const BATCH: usize = 3000;
    let entries: Vec<_> = combo_map.iter().collect();

    for chunk in entries.chunks(BATCH) {
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO item_combo_stats (set_number, patch, carry_id, item_combo, pick_count, placement_sum, avg_placement, tank_item_count) "
        );
        qb.push_values(chunk, |mut b, ((carry_id, combo), agg)| {
            let avg = agg.placement_sum as f64 / agg.pick_count as f64;
            b.push_bind(set_number)
             .push_bind(patch)
             .push_bind(carry_id)
             .push_bind(combo)
             .push_bind(agg.pick_count)
             .push_bind(agg.placement_sum)
             .push_bind(avg as f32)
             .push_bind(agg.tank_item_count);
        });
        qb.push(
            r#" ON CONFLICT (set_number, patch, carry_id, item_combo)
                DO UPDATE SET
                    pick_count = item_combo_stats.pick_count + EXCLUDED.pick_count,
                    placement_sum = item_combo_stats.placement_sum + EXCLUDED.placement_sum,
                    avg_placement = (item_combo_stats.placement_sum + EXCLUDED.placement_sum)::real 
                                    / (item_combo_stats.pick_count + EXCLUDED.pick_count),
                    tank_item_count = EXCLUDED.tank_item_count
            "#
        );
        qb.build().execute(pool).await?;
    }
    Ok(())
}

pub struct ComboStat {
    pub item_combo: String,      // "TFT_Item_A,B,C"
    pub picks: i64,
    pub avg_placement: f64,
    pub tank_item_count: i32,
}

pub async fn combo_stats_for_carry(
    pool: &PgPool,
    set_number: i32,
    patch: &str,
    carry_id: &str,
    min_picks: i64,
) -> anyhow::Result<Vec<ComboStat>> {
    let rows = sqlx::query_as::<_, (String, i64, f64, i32)>(
        r#"
        SELECT item_combo, pick_count, avg_placement, tank_item_count
        FROM item_combo_stats
        WHERE set_number = $1 AND patch = $2 AND carry_id = $3
          AND pick_count >= $4
        ORDER BY pick_count DESC
        "#,
    )
    .bind(set_number).bind(patch).bind(carry_id).bind(min_picks)
    .fetch_all(pool).await?;

    Ok(rows.into_iter().map(|(combo, picks, avg, tank)| ComboStat {
        item_combo: combo, picks, avg_placement: avg, tank_item_count: tank,
    }).collect())
}

pub struct CarryInfo {
    pub carry_id: String,
    pub total_appearances: i64,
    pub is_tank: bool,
}

pub async fn carry_list_for_combo(
    pool: &PgPool,
    set_number: i32,
    patch: &str,
    min_appearances: i64,
) -> anyhow::Result<Vec<CarryInfo>> {
    let rows = sqlx::query_as::<_, (String, i64, bool)>(
        r#"
        SELECT carry_id, total_appearances, is_tank
        FROM carry_stats
        WHERE set_number = $1 AND patch = $2
          AND is_carry = true
          AND total_appearances >= $3
        ORDER BY total_appearances DESC
        "#,
    )
    .bind(set_number).bind(patch).bind(min_appearances)
    .fetch_all(pool).await?;

    Ok(rows.into_iter().map(|(id, app, tank)| CarryInfo {
        carry_id: id, total_appearances: app, is_tank: tank,
    }).collect())
}