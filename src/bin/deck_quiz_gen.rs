//! 덱 완성 퀴즈 생성기.
//!
//! "이 티어덱[코어 유닛들]에서 빠진 핵심 유닛은?"
//!
//! 흐름:
//!   1. raw_decks       → 8기물 덱별 통계 (변형 다 따로)
//!   2. cluster_decks   → 변형 흡수 (공통 7개+ = 같은 덱)
//!   3. filter_tier_decks → 티어덱만 (avg 컷 + 순방덱 표본 컷)
//!   4. 각 덱의 코어 유닛을 하나씩 빼서 문제 생성
//!      - 정답 = 뺀 코어 유닛
//!      - 오답 = 다른 티어덱 유닛 (같은 코스트 우선) 3개
//!
//! 정답 데이터가 이미 정해진 구조라(티어덱 = 사실), 아이템 퀴즈 같은
//! 통계 노이즈 문제가 없다. 표본은 "덱 신뢰도"로만 쓴다.
//!
//! 실행:  cargo run --bin deck_quiz_gen
 
use std::collections::{HashMap, HashSet};
 
use rand::seq::SliceRandom;
use tft_iq::{db, meta::Meta, Config};
 
const RAW_MIN_GAMES: i64 = 150; // 원시 덱 최소 표본
const N_OPTIONS: usize = 4; // 보기 개수 (정답 1 + 오답 3)
// const MIN_CORE: usize = 6; // 코어가 이보다 적으면 덱 스킵 (정체성 불명확)
const MAX_APPEAR_RATE: f64 = 0.30; // 등장률 30% 초과 = 순수 접착제 → 정답 제외
 
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Config 로드 실패: {e}");
        std::process::exit(1);
    });
    let pool = db::connect(&cfg.database_url).await?;
 
    let Some(info) = db::current_patch_info(&pool).await? else {
        eprintln!("패치 정보 없음.");
        return Ok(());
    };
    let (set_number, patch) = (info.set_number, info.patch.clone());
    eprintln!("대상: set {set_number}, patch {patch}");
 
    let meta = Meta::load_with_lang(set_number, "ko_kr", false).await?;

    let carry_items: HashMap<String, Vec<db::CarryTopItem>> =
    db::load_carry_top_items(&pool).await?;

    // 임시: 표본별 덱 수 확인
    for threshold in [50i64, 100, 150, 200] {
        let d = db::raw_decks(&pool, &patch, threshold).await?;
        let valid = d.iter().filter(|x| x.avg_placement <= 5.0).count();
        eprintln!("{}판 이상 + avg5이하: {} 덱 (전체 {})", threshold, valid, d.len());
    }

    let raw = db::raw_decks(&pool, &patch, RAW_MIN_GAMES).await?;
    // raw 선언 다음
    if let Some(first) = raw.first() {
        eprintln!("덱 유닛 예시: {:?}", first.units);
    }
    let tier_decks: Vec<db::RawDeck> = raw
        .into_iter()
        .filter(|d| d.avg_placement <= 5.0)
        .collect();
    eprintln!("티어덱 {}개", tier_decks.len());

    // 유닛별 전체 등장률 (정답 필터용) — 너무 흔한 유닛은 정답에서 제외
    let (unit_appears, total_boards) = db::unit_appearance_rates(&pool, &patch).await?;

    // 전체 티어덱에 등장하는 모든 유닛 = "메타 유닛 풀" (오답 후보)
    let meta_pool: Vec<String> = {
        let mut set: HashSet<String> = HashSet::new();
        for d in &tier_decks {
            for u in &d.units {
                if u.starts_with("TFT17_")
                    && !u.contains("Summon")
                    && !u.contains("Minion")
                {
                    set.insert(u.clone());
                }
            }
        }
        set.into_iter().collect()
    };
    eprintln!("메타 유닛 풀 {}종", meta_pool.len());
 
    let mut rng = rand::thread_rng();
    let mut made = 0;
 
    for deck in &tier_decks {
        // 이 덱의 유닛 = 정확히 이 8명 (흡수 없음)
        let deck_units: HashSet<&String> = deck.units.iter().collect();
 
        // 너무 흔한 유닛은 정답에서 제외 (범용 유닛 도배 방지)
        let signature_core: Vec<String> = deck
            .units
            .iter()
            .filter(|u| {
                let rate = *unit_appears.get(*u).unwrap_or(&0) as f64 / total_boards as f64;
                rate <= MAX_APPEAR_RATE
            })
            .cloned()
            .collect();

        // 이름은 특성만
        let deck_label = match db::deck_signature_trait(&pool, &patch, &deck.units).await? {
            Some(trait_id) => format!("trait:{}", trait_id),   // "trait:TFT17_Sniper"
            None => deck.units.first()
                .map(|u| format!("unit:{}", u))                 // "unit:TFT17_Veigar"
                .unwrap_or_default(),
        };

        
        let mut sorted_units = deck.units.clone();
        sorted_units.sort();

        let units_json: Vec<serde_json::Value> = deck.units.iter().map(|uid| {
            match carry_items.get(uid) {
                Some(items) => serde_json::json!({
                    "id": uid,
                    "name": meta.unit_name(uid),
                    "items": items.iter().map(|it| serde_json::json!({
                        "id": it.item_id,
                        "name": it.name,
                        "icon": it.icon,
                    })).collect::<Vec<_>>(),
                }),
                None => serde_json::json!({
                    "id": uid, "name": meta.unit_name(uid),
                }),
            }
        }).collect();

        
        let mut sorted = deck.units.clone();
        sorted.sort();
        let deck_key = sorted.join(",");

        db::upsert_deck_stats(
            &pool, &deck_key, &patch, set_number,
            &serde_json::json!(units_json),
            &deck_label, deck.avg_placement, deck.games,
        ).await?;

        // 코어 유닛을 하나씩 빼서 문제 생성 (시그니처 코어만 정답)
        for removed in &signature_core {
            let shown: Vec<String> = deck
                .units
                .iter()
                .filter(|u| *u != removed)
                .cloned()
                .collect();

            // 오답 후보: 메타 풀 - 이 덱 유닛
            let mut distractor_pool: Vec<String> = meta_pool
                .iter()
                .filter(|u| !deck_units.contains(*u))
                .cloned()
                .collect();
 
            // 같은 코스트를 앞으로 (그럴듯한 오답) — 코스트 내림차순 근접 정렬
            let ans_cost = meta.unit_cost(removed, 0);
            distractor_pool.sort_by_key(|u| (meta.unit_cost(u, 0) - ans_cost).abs());
 
            // 상위권(코스트 근접)에서 약간의 무작위성 — 앞 10개를 섞어 3개 선택
            let head = distractor_pool.len().min(10);
            distractor_pool[..head].shuffle(&mut rng);
            let distractors: Vec<String> =
                distractor_pool.into_iter().take(N_OPTIONS - 1).collect();
            if distractors.len() < N_OPTIONS - 1 {
                continue; // 오답 못 채우면 스킵
            }
 
            // 보기 = 정답 + 오답, 셔플
            let mut option_ids: Vec<String> = vec![removed.clone()];
            option_ids.extend(distractors);
            option_ids.shuffle(&mut rng);
 
            let options: Vec<serde_json::Value> = option_ids
                .iter()
                .map(|id| {
                    serde_json::json!({
                        "id": id,
                        "name": meta.unit_name(id),
                        "icon": unit_icon(id),
                    })
                })
                .collect();
 
            let shown_units: Vec<serde_json::Value> = shown
                .iter()
                .map(|id| {
                    serde_json::json!({
                        "id": id,
                        "name": meta.unit_name(id),
                        "icon": unit_icon(id),
                    })
                })
                .collect();

            let mut synergy: std::collections::HashMap<String, i32> = std::collections::HashMap::new();
            for uid in &shown {
                if let Some(u) = meta.units.get(uid) {
                    for tr in &u.traits {
                        *synergy.entry(tr.clone()).or_insert(0) += 1;
                    }
                }
            }
            // 2개 이상 모인 특성만, 많은 순 정렬
            let mut synergies: Vec<(String, i32)> = synergy
                .into_iter()
                .filter(|(_, n)| *n >= 2)
                .collect();
            synergies.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let synergies_json: Vec<serde_json::Value> = synergies
                .iter()
                .map(|(tr, n)| serde_json::json!({ "trait": tr, "count": n }))
                .collect();
 
            let prompt = serde_json::json!({
                "question": format!("{} 덱에서 빠진 핵심 유닛은?", deck_label),
                "deck_label": deck_label,
                "shown_units": shown_units,
                "synergies": synergies_json,
                "patch": patch,
            });
 
            let stats = serde_json::json!({
                "deck_avg": deck.avg_placement,
                "deck_games": deck.games,
                "answer": { "id": removed, "name": meta.unit_name(removed), "icon": unit_icon(removed) },
                "options": option_ids.iter().map(|id| serde_json::json!({
                    "id": id, "name": meta.unit_name(id), "is_best": id == removed,
                })).collect::<Vec<_>>(),
            });
 
            // 덱 정체성 키 = 정렬된 코어 (변형 흡수됐으니 안정적)
            let deck_key = deck.units.join(",");
 
            db::insert_deck_puzzle(
                &pool,
                set_number,
                &patch,
                &deck_key,
                removed, // variant = 뺀 유닛 → (덱, 뺀유닛) 유일
                &prompt,
                &serde_json::Value::Array(options),
                removed,
                &stats,
            )
            .await?;
            made += 1;
        }
    }
 
    eprintln!("덱 완성 퀴즈 {made}개 생성");
    Ok(())
}
 
/// 유닛 아이콘 URL (Community Dragon). id 예: "TFT17_Karma".
fn unit_icon(id: &str) -> String {
    let low = id.to_lowercase();
    let set = low
        .trim_start_matches("tft")
        .split('_')
        .next()
        .unwrap_or("");
    // 파일명이 유닛 id와 다른 특수 유닛 (변신폼 등). 폴더는 id, 파일명만 예외.
    let file_base: &str = match low.as_str() {
        "tft17_rhaast" => "tft17_kayn_slay", // 라스트=케인 변신폼
        other => other,
    };
    format!(
        "https://raw.communitydragon.org/latest/game/assets/characters/{low}/hud/{file_base}_square.tft_set{set}.png"
    )
}