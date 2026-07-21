use std::collections::{HashMap, HashSet};
use sqlx::PgPool;
use tft_iq::{Config, db};

const LOAD_LIMIT: i64 = 1_000_000;

struct Agg { pick_count: i64, placement_sum: i64 }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;

    let Some(info) = db::current_patch_info(&pool).await? else {
        eprintln!("패치 데이터 없음."); return Ok(());
    };
    let (set_number, patch) = (info.set_number, info.patch.clone());
    eprintln!("특수템 집계: set {set_number}, patch {patch}");

    // 특수템 id → category 맵 로드 (분류 bin이 채운 것)
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT item_id, category FROM special_items")
            .fetch_all(&pool).await?;
    let special_cat: HashMap<String, String> = rows.into_iter().collect();
    eprintln!("특수템 {}종 참조", special_cat.len());

    let matches = db::load_matches(&pool, set_number, &patch, LOAD_LIMIT).await?;
    eprintln!("매치 {}개 순회", matches.len());

    // (carry_id, item_id) → Agg
    let mut map: HashMap<(String, String), Agg> = HashMap::new();

    for m in &matches {
        for p in &m.info.participants {
            let placement = p.placement as i64;
            for unit in &p.units {
                let carry_id = &unit.character_id;
                // 유닛이 낀 아이템 중 특수템만 (조합 아닌 단일 단위)
                for item in &unit.item_names {
                    if special_cat.contains_key(item) {
                        let agg = map.entry((carry_id.clone(), item.clone()))
                            .or_insert(Agg { pick_count: 0, placement_sum: 0 });
                        agg.pick_count += 1;
                        agg.placement_sum += placement;
                    }
                }
            }
        }
    }

    eprintln!("캐리×특수템 {}종 집계", map.len());
    save_special_stats(&pool, set_number, &patch, &map, &special_cat).await?;

    eprintln!("저장 완료");
    pool.close().await;
    Ok(())
}

async fn save_special_stats(
    pool: &PgPool, set_number: i32, patch: &str,
    map: &HashMap<(String, String), Agg>,
    special_cat: &HashMap<String, String>,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM special_item_stats WHERE set_number=$1 AND patch=$2")
        .bind(set_number).bind(patch).execute(&mut *tx).await?;

    const BATCH: usize = 1000;
    let entries: Vec<_> = map.iter().collect();
    for chunk in entries.chunks(BATCH) {
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO special_item_stats (set_number, patch, carry_id, item_id, category, pick_count, placement_sum, avg_placement) "
        );
        qb.push_values(chunk, |mut b, ((carry_id, item_id), agg)| {
            let avg = agg.placement_sum as f64 / agg.pick_count as f64;
            let cat = special_cat.get(item_id).map(|s| s.as_str()).unwrap_or("");
            b.push_bind(set_number).push_bind(patch)
             .push_bind(carry_id).push_bind(item_id).push_bind(cat)
             .push_bind(agg.pick_count).push_bind(agg.placement_sum)
             .push_bind(avg as f32);
        });
        qb.build().execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}