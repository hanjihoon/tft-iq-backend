//! 아이템 BIS 퀴즈 생성기.
//!
//! 로직:
//!   1) carry_candidates로 충분히 등장한 딜 캐리 목록
//!   2) 각 캐리마다 item_stats_for_carry로 BIS(평균 등수 최저) 아이템
//!   3) 정답 = BIS, 오답 = 다른 캐리들의 인기 아이템 중 이 캐리가 잘 안 쓰는 것
//!   4) 보기 4개 셔플 → puzzles 테이블에 저장
//!
//! 실행:  cargo run --bin item_quiz_gen

use std::collections::HashSet;

use rand::seq::SliceRandom;
use tft_iq::{
    Config, db,
    meta::Meta,
    puzzle::{ItemOptionStat, ItemPrompt, NamedRef, OptionItem, PuzzleKind},
};

/// 캐리로 인정할 최소 등장 수
const MIN_CARRY_APPEARANCES: i64 = 20;
/// BIS 후보로 인정할 아이템의 최소 픽 수
const MIN_ITEM_PICKS: i64 = 10;
/// 캐리당 만들 퀴즈 수 (보통 1개)
const PUZZLES_PER_CARRY: usize = 1;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Config 로드 실패: {e}");
        std::process::exit(1);
    });
    let pool = db::connect(&cfg.database_url).await?;

    let Some(info) = db::current_patch_info(&pool).await? else {
        eprintln!("패치 데이터 없음. crawler를 먼저 실행해라.");
        return Ok(());
    };
    let (set_number, patch) = (info.set_number, info.patch.clone());
    eprintln!("대상: set {set_number}, patch {patch} ({:.1}일차)", info.age_days);

    let meta = Meta::load(set_number).await?;

    // 1) 딜 캐리 목록
    let carries = db::carry_candidates(&pool, set_number, &patch, MIN_CARRY_APPEARANCES).await?;
    eprintln!("캐리 후보 {}종", carries.len());
    if carries.is_empty() {
        eprintln!("캐리가 없음. 데이터를 더 모아라.");
        return Ok(());
    }

    // 모든 캐리의 BIS를 미리 구해 오답 풀로도 활용
    // (캐리 id -> 그 캐리의 아이템 통계)
    let mut all_best: Vec<(String, db::ItemStat)> = Vec::new();
    for (carry_id, _) in &carries {
        let stats = db::item_stats_for_carry(&pool, set_number, &patch, carry_id, MIN_ITEM_PICKS).await?;
        if let Some(best) = stats.first() {
            all_best.push((carry_id.clone(), best.clone()));
        }
    }

    let mut rng = rand::thread_rng();
    let mut made = 0usize;

    for (carry_id, appearances) in &carries {
        let stats = db::item_stats_for_carry(&pool, set_number, &patch, carry_id, MIN_ITEM_PICKS).await?;
        if stats.len() < 4 {
            continue; // 보기 4개를 못 채우면 스킵
        }

        // 정답 = 평균 등수 최저(이미 ORDER BY avg_place ASC라 first)
        let best = stats[0].clone();

        // 이 캐리가 실제로 든 아이템 집합 (오답 중복 방지용)
        let own_items: HashSet<&str> = stats.iter().map(|s| s.item.as_str()).collect();

        // 오답 풀: 다른 캐리들의 BIS 중 이 캐리가 잘 안 쓰는 아이템
        let mut distractor_pool: Vec<String> = all_best
            .iter()
            .filter(|(cid, _)| cid != carry_id)
            .map(|(_, st)| st.item.clone())
            .filter(|item| !own_items.contains(item.as_str()))
            .collect();
        distractor_pool.sort();
        distractor_pool.dedup();

        // 오답 풀이 부족하면 이 캐리의 하위 아이템(평균 등수 나쁜 것)으로 보충
        if distractor_pool.len() < 3 {
            for s in stats.iter().rev().take(3) {
                if s.item != best.item && !distractor_pool.contains(&s.item) {
                    distractor_pool.push(s.item.clone());
                }
            }
        }

        let distractors: Vec<String> = distractor_pool
            .choose_multiple(&mut rng, 3)
            .cloned()
            .collect();
        if distractors.len() < 3 {
            continue;
        }

        // 보기 구성 + 셔플
        let mut option_ids = vec![best.item.clone()];
        option_ids.extend(distractors);
        option_ids.shuffle(&mut rng);

        // 보기/통계 직렬화
        let options: Vec<OptionItem> = option_ids
            .iter()
            .map(|id| OptionItem {
                id: id.clone(),
                name: meta.item_name(id),
            })
            .collect();

        // 보기별 통계: 이 캐리 기준 평균 등수 (없으면 null)
        let option_stats: Vec<ItemOptionStat> = option_ids
            .iter()
            .map(|id| {
                let st = stats.iter().find(|s| &s.item == id);
                ItemOptionStat {
                    id: id.clone(),
                    name: meta.item_name(id),
                    avg_placement: st.map(|s| (s.avg_placement * 100.0).round() / 100.0),
                    sample_size: st.map(|s| s.picks).unwrap_or(0),
                    is_best: *id == best.item,
                }
            })
            .collect();

        let prompt = ItemPrompt {
            question: format!("{} 캐리에게 가장 좋은 아이템은?", meta.unit_name(carry_id)),
            carry: NamedRef {
                id: carry_id.clone(),
                name: meta.unit_name(carry_id),
            },
            context_traits: Vec::new(),
            patch: patch.clone(),
        };

        let stats_payload = serde_json::json!({
            "options": option_stats,
            "carry_appearances": appearances,
        });

        db::insert_puzzle(
            &pool,
            PuzzleKind::ItemCombine.as_str(),
            set_number,
            &patch,
            &serde_json::to_value(&prompt)?,
            &serde_json::to_value(&options)?,
            &best.item, // 정답 = BIS 아이템 id
            &stats_payload,
            None,                    // "" → None
        )
        .await?;

        made += 1;
        if made >= carries.len() * PUZZLES_PER_CARRY {
            break;
        }
    }

    eprintln!("아이템 BIS 퀴즈 {made}개 생성 완료");
    Ok(())
}