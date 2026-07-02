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
 
use std::collections::HashSet;
 
use rand::seq::SliceRandom;
use tft_iq::{
    db,
    deck_cluster::{cluster_decks, filter_tier_decks, DeckCluster},
    meta::Meta,
    Config,
};
 
const RAW_MIN_GAMES: i64 = 40; // 원시 덱 최소 표본
const MIN_COMMON: usize = 7; // 변형 흡수 기준 (공통 7개+ = 1기물 차이)
const SOFT_GAMES: i64 = 100; // 순방덱(avg 4.5~5.0) 최소 표본
const N_OPTIONS: usize = 4; // 보기 개수 (정답 1 + 오답 3)
const MIN_CORE: usize = 6; // 코어가 이보다 적으면 덱 스킵 (정체성 불명확)
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
 
    let meta = Meta::load(set_number).await?;

    // 1~3단계: 원시 덱 → 흡수 → 티어덱
    let raw = db::raw_decks(&pool, &patch, RAW_MIN_GAMES).await?;
    let clusters = cluster_decks(raw, MIN_COMMON);
    let tier_decks = filter_tier_decks(clusters, SOFT_GAMES);
    eprintln!("티어덱 {}개", tier_decks.len());
 
    // 유닛별 전체 등장률 (정답 필터용) — 너무 흔한 유닛은 정답에서 제외
    let (unit_appears, total_boards) = db::unit_appearance_rates(&pool, &patch).await?;

    // 전체 티어덱에 등장하는 모든 유닛 = "메타 유닛 풀" (오답 후보)
    let meta_pool: Vec<String> = {
        let mut set: HashSet<String> = HashSet::new();
        for c in &tier_decks {
            for v in &c.variants {
                for u in &v.units {
                    set.insert(u.clone());
                }
            }
        }
        set.into_iter().collect()
    };
    eprintln!("메타 유닛 풀 {}종", meta_pool.len());
 
    let mut rng = rand::thread_rng();
    let mut made = 0;
 
    for cluster in &tier_decks {
        if cluster.core.len() < MIN_CORE {
            continue; // 코어가 너무 적으면 덱 정체성이 약함 → 스킵
        }
 
        // 이 덱의 전체 유닛(코어 + 모든 변형 플렉스) = 오답에서 제외할 집합
        let deck_units: HashSet<&String> = cluster
            .variants
            .iter()
            .flat_map(|v| v.units.iter())
            .collect();
 
        // 덱 이름 (임시): 코어 중 대표 유닛 한글명. 특성 기반은 2단계에서.
        let deck_label = match db::deck_signature_trait(&pool, &patch, &cluster.core).await? {
            Some(trait_id) => meta.trait_name(&trait_id),
            None => deck_display_name(cluster, &meta), // 특성 없으면 대표 유닛명 폴백
        };
 
        // 너무 흔한 유닛(등장 30%+)은 정답에서 제외 → 카르마·모데 도배 방지
        let signature_core: Vec<String> = cluster
            .core
            .iter()
            .filter(|u| {
                let rate = *unit_appears.get(*u).unwrap_or(&0) as f64 / total_boards as f64;
                rate <= MAX_APPEAR_RATE
            })
            .cloned()
            .collect();

        // 코어 유닛을 하나씩 빼서 문제 생성 (시그니처 코어만 정답)
        for removed in &signature_core {
            // 화면에 보여줄 유닛 = 코어에서 정답만 뺀 것
            let shown: Vec<String> = cluster
                .core
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
 
            let prompt = serde_json::json!({
                "question": format!("{} 덱에서 빠진 핵심 유닛은?", deck_label),
                "deck_label": deck_label,
                "shown_units": shown_units,
                "patch": patch,
            });
 
            let stats = serde_json::json!({
                "deck_avg": cluster.best_avg,
                "deck_games": cluster.total_games,
                "answer": { "id": removed, "name": meta.unit_name(removed), "icon": unit_icon(removed) },
                "options": option_ids.iter().map(|id| serde_json::json!({
                    "id": id, "name": meta.unit_name(id), "is_best": id == removed,
                })).collect::<Vec<_>>(),
            });
 
            // 덱 정체성 키 = 정렬된 코어 (변형 흡수됐으니 안정적)
            let deck_key = cluster.core.join(",");
 
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

        // ── 층위 2: 마무리 최적화 (변형 2개 이상만) ──
        if cluster.variants.len() >= 2 {
            // 각 변형의 플렉스 유닛(코어 아닌 것) + 평균등수
            // variants[i].units 에서 코어를 뺀 것이 그 변형의 플렉스
            let core_set: HashSet<&String> = cluster.core.iter().collect();
            let mut flex_opts: Vec<(String, f64, i64)> = Vec::new(); // (unit, avg, games)
            for v in &cluster.variants {
                for u in &v.units {
                    if !core_set.contains(u) {
                        flex_opts.push((u.clone(), v.avg_placement, v.games));
                    }
                }
            }
            // 플렉스 후보가 2개 미만이면 문제 불가
            if flex_opts.len() >= 2 {
                // 최적 = 평균등수 최저
                flex_opts.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
                let answer_flex = flex_opts[0].0.clone();

                // 보기 = 플렉스 후보들 (최대 4개), 각 평균등수 포함
                let options: Vec<serde_json::Value> = flex_opts.iter().take(N_OPTIONS)
                    .map(|(id, avg, games)| serde_json::json!({
                        "id": id, "name": meta.unit_name(id), "icon": unit_icon(id),
                        "avg_placement": (avg * 100.0).round() / 100.0, "games": games,
                    })).collect();

                let shown_units: Vec<serde_json::Value> = cluster.core.iter()
                    .map(|id| serde_json::json!({
                        "id": id, "name": meta.unit_name(id), "icon": unit_icon(id),
                    })).collect();

                let prompt = serde_json::json!({
                    "question": format!("{} 덱의 마지막 한 자리, 최적은?", deck_label),
                    "deck_label": deck_label,
                    "shown_units": shown_units,
                    "patch": patch,
                });
                let stats = serde_json::json!({
                    "deck_avg": cluster.best_avg,
                    "deck_games": cluster.total_games,
                    "answer": { "id": answer_flex, "name": meta.unit_name(&answer_flex) },
                    "options": flex_opts.iter().take(N_OPTIONS).map(|(id, avg, games)| serde_json::json!({
                        "id": id, "name": meta.unit_name(id),
                        "avg_placement": (avg * 100.0).round() / 100.0, "games": games,
                        "is_best": *id == answer_flex,
                    })).collect::<Vec<_>>(),
                });

                let deck_key = cluster.core.join(",");
                db::insert_flex_puzzle(
                    &pool, set_number, &patch, &deck_key,
                    &prompt, &serde_json::Value::Array(options), &answer_flex, &stats,
                ).await?;
                made += 1;
            }
        }
    }
 
    eprintln!("덱 완성 퀴즈 {made}개 생성");
    Ok(())
}
 
/// 덱 표시 이름 (임시): 코어 중 가장 고코스트 유닛의 한글명 + " 덱".
/// 캐리 판별은 예외가 많아 접었고, 특성 기반 이름은 2단계 작업.
fn deck_display_name(cluster: &DeckCluster, meta: &Meta) -> String {
    let top = cluster
        .core
        .iter()
        .max_by_key(|u| meta.unit_cost(u, 0))
        .map(|u| meta.unit_name(u))
        .unwrap_or_else(|| "티어".to_string());
    format!("{top}")
}
 
/// 유닛 아이콘 URL (Community Dragon). id 예: "TFT17_Karma".
fn unit_icon(id: &str) -> String {
    let low = id.to_lowercase(); // tft17_karma
    // 접두사 tft{N}_ 에서 N 추출
    let set = low
        .trim_start_matches("tft")
        .split('_')
        .next()
        .unwrap_or("");
    format!(
        "https://raw.communitydragon.org/latest/game/assets/characters/{low}/hud/{low}_square.tft_set{set}.png"
    )
}