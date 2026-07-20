use std::collections::HashMap;

use tft_iq::{
    Config,
    db::{self, ComboStat},
    meta::Meta,
};

const MIN_TOTAL_APPEARANCES: i64 = 10000;
const MIN_COMBO_PICKS: i64 = 50;
const MIN_COMBOS_NEEDED: usize = 6; // 문제1(1~4) + 변형(3~6) = 최소 6개
const SHRINK_C: f64 = 25.0;         // 베이지안 수축 강도 (기존 계승)

/// 베이지안 보정 평균: 표본 적으면 prior로 끌어당김
fn adjusted(avg: f64, picks: i64, prior: f64) -> f64 {
    (avg * picks as f64 + prior * SHRINK_C) / (picks as f64 + SHRINK_C)
}

/// 캐리의 전체 평균 등수 (prior) — 조합들의 가중평균
fn carry_prior(combos: &[ComboStat]) -> f64 {
    let total_picks: i64 = combos.iter().map(|c| c.picks).sum();
    if total_picks == 0 {
        return 4.5; // 안전 기본값
    }
    let weighted: f64 = combos
        .iter()
        .map(|c| c.avg_placement * c.picks as f64)
        .sum();
    weighted / total_picks as f64
}

/// combo 문자열("A,B,C") → 아이템 3개의 {id, name, icon} JSON
fn build_combo_items(
    combo: &str,
    item_map: &HashMap<String, (String, String)>,
) -> serde_json::Value {
    let items: Vec<serde_json::Value> = combo
        .split(',')
        .map(|id| {
            let (name, icon) = item_map
                .get(id)
                .cloned()
                .unwrap_or_else(|| (id.to_string(), String::new()));
            serde_json::json!({ "id": id, "name": name, "icon": icon })
        })
        .collect();
    serde_json::Value::Array(items)
}

/// 한 문제 생성 + 저장
async fn make_and_insert(
    pool: &sqlx::PgPool,
    set_number: i32,
    patch: &str,
    carry_id: &str,
    carry_name: &str,
    is_tank: bool,
    variant: &str,
    candidates: &[ComboStat], // 4개 (선택지)
    prior: f64,
    hidden: Option<&ComboStat>,
    item_map: &HashMap<String, (String, String)>,
) -> anyhow::Result<()> {
    // 정답: 보정 평균 최저(최고 등수)
    let answer = candidates
        .iter()
        .min_by(|a, b| {
            adjusted(a.avg_placement, a.picks, prior)
                .partial_cmp(&adjusted(b.avg_placement, b.picks, prior))
                .unwrap()
        })
        .unwrap();

    // options: 각 조합 = {combo, items[3], avg, picks}
    let options: Vec<serde_json::Value> = candidates
        .iter()
        .map(|c| {
            serde_json::json!({
                "combo": c.item_combo,
                "items": build_combo_items(&c.item_combo, item_map),
                "avg_placement": (c.avg_placement * 100.0).round() / 100.0,
                "picks": c.picks,
                "is_best": c.item_combo == answer.item_combo,
            })
        })
        .collect();

    let carry_type = if is_tank { "tank" } else { "dealer" };

    let prompt = serde_json::json!({
        "question": format!("{} 캐리의 최적 3코어 아이템 조합은?", carry_name),
        "carry": { "id": carry_id, "name": carry_name },
        "patch": patch,
    });

    // 히든픽 (있으면)
    let hidden_json = hidden.map(|h| {
        serde_json::json!({
            "combo": h.item_combo,
            "items": build_combo_items(&h.item_combo, item_map),
            "avg_placement": (h.avg_placement * 100.0).round() / 100.0,
            "picks": h.picks,
        })
    });

    let stats = serde_json::json!({
        "options": options,
        "carry_type": carry_type,
        "hidden_pick": hidden_json,
    });

    // options는 정답 위치 노출 방지 위해 셔플된 순서로 저장하는 게 좋지만,
    // is_best 플래그로 구분하므로 프론트에서 처리 가능. 여기선 픽률 순 유지.
    db::insert_item_puzzle(
        pool,
        set_number,
        patch,
        carry_id,
        variant,
        carry_type,
        &prompt,
        &serde_json::Value::Array(options.clone()),
        &answer.item_combo,
        &stats,
    )
    .await?;

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;

    let Some(info) = db::current_patch_info(&pool).await? else {
        eprintln!("패치 데이터 없음.");
        return Ok(());
    };
    let (set_number, patch) = (info.set_number, info.patch.clone());
    eprintln!("대상: set {set_number}, patch {patch}");

    let meta = Meta::load_with_lang(set_number, "ko_kr", false).await?;
    let item_map = db::all_item_info(&pool).await?;
    eprintln!("아이템 정보 {}종 로드", item_map.len());

    let carries =
        db::carry_list_for_combo(&pool, set_number, &patch, MIN_TOTAL_APPEARANCES).await?;
    eprintln!("캐리 후보 {}종", carries.len());

    let mut made = 0;
    let mut skipped = 0;

    for carry in &carries {
        let combos =
            db::combo_stats_for_carry(&pool, set_number, &patch, &carry.carry_id, MIN_COMBO_PICKS)
                .await?;

        if combos.len() < MIN_COMBOS_NEEDED {
            skipped += 1;
            continue; // 6개 안 되면 스킵
        }

        let prior = carry_prior(&combos);
        let carry_name = meta.unit_name(&carry.carry_id);

        // 히든픽: 상위 6개 밖에서, 픽률 있지만 보정평균 좋은 조합
        // (7위 이하 중 보정 최고)
        let hidden = combos
            .iter()
            .skip(MIN_COMBOS_NEEDED)
            .min_by(|a, b| {
                adjusted(a.avg_placement, a.picks, prior)
                    .partial_cmp(&adjusted(b.avg_placement, b.picks, prior))
                    .unwrap()
            })
            .cloned();

        // 문제1: 픽률 1~4위
        make_and_insert(
            &pool, set_number, &patch, &carry.carry_id, &carry_name,
            carry.is_tank, "main", &combos[0..4], prior,
            hidden.as_ref(), &item_map,
        )
        .await?;
        made += 1;

        // 변형: 픽률 3~6위
        make_and_insert(
            &pool, set_number, &patch, &carry.carry_id, &carry_name,
            carry.is_tank, "variant", &combos[2..6], prior,
            hidden.as_ref(), &item_map,
        )
        .await?;
        made += 1;
    }

    eprintln!("3템 조합 퀴즈 {made}개 생성 (조합부족 {skipped}종 제외)");
    Ok(())
}