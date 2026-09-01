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
 
use std::{collections::{HashMap, HashSet}, time::Instant};
use rand::seq::SliceRandom;
use tft_iq::{db, meta::Meta, Config};
use tokio::task::JoinSet;
use tft_iq::meta::cdragon_icon_url;

const MIN_DECK_GAMES_RATE: f64 = 0.005;
const MIN_DECK_GAMES_FLOOR: i64 = 30;
const RAW_MIN_GAMES: i64 = 100;
const N_OPTIONS: usize = 4;
const MAX_APPEAR_RATE: f64 = 0.30;
/// 3성 비율이 이 값 미만이면 싣지 않는다.
/// 대부분 유닛은 2성에 머무르므로 낮은 비율까지 노출하면 노이즈가 된다.
const STAR3_MIN_RATIO: i32 = 40;


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
    eprintln!("meta 유닛 {}종, 특성 {}종", meta.units.len(), meta.traits.len());

    let carry_items = db::load_carry_top_items(&pool, &patch).await?;
    eprintln!("carry_items {}종", carry_items.len());

    let star3 = db::load_unit_star3(&pool, set_number, &patch).await?;
    eprintln!("star3 {}종", star3.len());

    let unit_costs = db::unit_costs_from_matches(&pool, set_number).await?;
    eprintln!("유닛 코스트 {}종", unit_costs.len());

    let total_matches = db::current_patch_match_count(&pool).await?;
    let min_deck_games = ((total_matches as f64 * MIN_DECK_GAMES_RATE) as i64)
        .max(MIN_DECK_GAMES_FLOOR);
    eprintln!("매치 {total_matches}건 → 덱 표본 임계 {min_deck_games}판");

    // 임계 감각 확인용 — 계산값 주변을 훑어본다
    for m in [min_deck_games / 2, min_deck_games, min_deck_games * 2] {
        let d = db::raw_decks(&pool, &patch, m).await?;
        let valid = d.iter().filter(|x| x.avg_placement <= 5.0).count();
        eprintln!("{m}판 이상 + avg5이하: {valid}덱 (전체 {})", d.len());
    }

    let t0 = Instant::now();
    let raw = db::raw_decks(&pool, &patch, RAW_MIN_GAMES).await?;
    eprintln!("① raw 로딩: {:?} ({}건)", t0.elapsed(), raw.len());
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
        // raw_decks가 rarity 0~4(정규 챔피언)만 반환하므로 추가 필터는 불필요하다.
        for d in &tier_decks {
            for u in &d.units {
                set.insert(u.clone());
            }
        }
        set.into_iter().collect()
    };
    eprintln!("메타 유닛 풀 {}종", meta_pool.len());
 
    let mut rng = rand::thread_rng();
    let mut made = 0;

    let deck_labels: HashMap<String, String> = {
        let mut labels = HashMap::new();
        // Supavisor 세션 풀이 15개라 동시 실행은 그보다 낮게 유지한다.
        for chunk in tier_decks.chunks(8) {
            let mut set = JoinSet::new();
            for deck in chunk {
                let pool = pool.clone();
                let patch = patch.clone();
                let units = deck.units.clone();
                set.spawn(async move {
                    let key = { let mut s = units.clone(); s.sort(); s.join(",") };
                    let label = match db::deck_signature_trait(&pool, &patch, &units).await {
                        Ok(Some(t)) => format!("trait:{t}"),
                        _ => units.first().map(|u| format!("unit:{u}")).unwrap_or_default(),
                    };
                    (key, label)
                });
            }
            while let Some(res) = set.join_next().await {
                if let Ok((k, v)) = res {
                    labels.insert(k, v);
                }
            }
        }
        labels
    };

    let t1 = Instant::now();
    let mut deck_rows: Vec<db::DeckStatRow> = Vec::new();
    let mut puzzle_rows: Vec<db::DeckPuzzleRow> = Vec::new();

    for deck in &tier_decks {
        let deck_units: HashSet<&String> = deck.units.iter().collect();

        let signature_core: Vec<String> = deck.units.iter()
            .filter(|u| {
                let rate = *unit_appears.get(*u).unwrap_or(&0) as f64 / total_boards as f64;
                rate <= MAX_APPEAR_RATE
            })
            .cloned()
            .collect();

        let mut sorted = deck.units.clone();
        sorted.sort();
        let deck_key = sorted.join(",");

        // 미리 뽑아둔 라벨 사용 (DB 왕복 없음)
        let deck_label = deck_labels.get(&deck_key).cloned().unwrap_or_default();

        let units_json: Vec<serde_json::Value> = deck.units.iter().map(|uid| {
            let mut u = serde_json::json!({
                "id": uid,
                "name": meta.unit_name(uid),
            });

            if let Some(items) = carry_items.get(uid) {
                u["items"] = serde_json::json!(
                    items.iter().map(|it| serde_json::json!({
                        "id": it.item_id,
                        "name": it.name,
                        "icon": it.icon,
                    })).collect::<Vec<_>>()
                );
            }

            if let Some(ratio) = star3.get(uid) {
                if *ratio >= STAR3_MIN_RATIO && meta.unit_cost(uid, 0) <= 3 {
                    u["star3_ratio"] = serde_json::json!(ratio);
                }
            }

            u
        }).collect();

        deck_rows.push(db::DeckStatRow {
            deck_key: deck_key.clone(),
            units: serde_json::json!(units_json),
            label: deck_label.clone(),
            avg_placement: deck.avg_placement,
            games: deck.games,
        });

        // 코어 유닛을 하나씩 빼서 문제 생성
        for removed in &signature_core {
            let shown: Vec<String> = deck.units.iter()
                .filter(|u| *u != removed)
                .cloned()
                .collect();

            // 오답 후보: 메타 풀 - 이 덱 유닛
            let mut distractor_pool: Vec<String> = meta_pool.iter()
                .filter(|u| !deck_units.contains(*u))
                .cloned()
                .collect();

            // 같은 코스트를 앞으로 (그럴듯한 오답)
            let ans_cost = *unit_costs.get(removed).unwrap_or(&0);
            distractor_pool.sort_by_key(|u| (unit_costs.get(u).unwrap_or(&0) - ans_cost).abs());

            let head = distractor_pool.len().min(10);
            distractor_pool[..head].shuffle(&mut rng);
            let distractors: Vec<String> =
                distractor_pool.into_iter().take(N_OPTIONS - 1).collect();
            if distractors.len() < N_OPTIONS - 1 {
                continue;
            }

            let mut option_ids: Vec<String> = vec![removed.clone()];
            option_ids.extend(distractors);
            option_ids.shuffle(&mut rng);

            let unit_icons: HashMap<String, String> = db::load_unit_icons(&pool, set_number, "ko_kr").await?;
            let icon_of = |id: &str| unit_icons.get(id).cloned().unwrap_or_default();

            let options: Vec<serde_json::Value> = option_ids.iter()
                .map(|id| serde_json::json!({
                    "id": id,
                    "name": meta.unit_name(id),
                    "icon": icon_of(id),
                }))
                .collect();

            let shown_units: Vec<serde_json::Value> = shown.iter()
                .map(|id| serde_json::json!({
                    "id": id,
                    "name": meta.unit_name(id),
                    "icon": icon_of(id),
                }))
                .collect();

            // 남은 유닛들의 특성 시너지 (2개 이상만)
            let mut synergy: HashMap<String, i32> = HashMap::new();
            for uid in &shown {
                if let Some(u) = meta.units.get(uid) {
                    for tr in &u.traits {
                        *synergy.entry(tr.clone()).or_insert(0) += 1;
                    }
                }
            }
            let mut synergies: Vec<(String, i32)> = synergy.into_iter()
                .filter(|(_, n)| *n >= 2)
                .collect();
            synergies.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let synergies_json: Vec<serde_json::Value> = synergies.iter()
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
                "answer": { "id": removed, "name": meta.unit_name(removed), "icon": cdragon_icon_url(removed) },
                "options": option_ids.iter().map(|id| serde_json::json!({
                    "id": id, "name": meta.unit_name(id), "is_best": id == removed,
                })).collect::<Vec<_>>(),
            });

            puzzle_rows.push(db::DeckPuzzleRow {
                carry_id: deck.units.join(","),   // 기존과 동일하게 유지
                variant: removed.clone(),
                prompt,
                options: serde_json::Value::Array(options),
                answer: removed.clone(),
                stats,
            });
            made += 1;
        }
    }

    eprintln!("② 집계 계산: {:?} (덱 {}개)", t1.elapsed(), tier_decks.len());

    // 루프 후 한 번에 저장
    let t2 = Instant::now();
    db::upsert_deck_stats_batch(&pool, &patch, set_number, &deck_rows).await?;
    db::insert_deck_puzzles_batch(&pool, set_number, &patch, &puzzle_rows).await?;
    eprintln!("③ 저장: {:?} (덱 {}개, 퀴즈 {}개)", t2.elapsed(), deck_rows.len(), puzzle_rows.len());

    eprintln!("덱 완성 퀴즈 {made}개 생성");

    pool.close().await;
    Ok(())
}

