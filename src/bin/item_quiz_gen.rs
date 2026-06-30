//! 아이템 BIS 퀴즈 생성기 (캐리 타입 + 계열 일치 오답).
//!
//! 난이도 정책:
//!   dealer  → 오답을 정답과 같은 계열에서만 (AD캐리=AD오답) → 소거 불가, 어려움
//!   bruiser → 오답에 딜템 + 브루저/방어템 혼합 → "딜 vs 탱" 트레이드오프
//!   tank/애매 → 제외 (정체성 불명확)
//!
//! 정답은 항상 데이터(BIS 1위)가 정한다. 계열은 "오답 풀"에만 관여.
//!
//! 실행:  cargo run --bin item_quiz_gen

use std::collections::HashSet;

use rand::seq::SliceRandom;
use tft_iq::db::{self, CarryType};
use tft_iq::{
    Config,
    meta::Meta,
    puzzle::{ItemOptionStat, ItemPrompt, NamedRef, OptionItem},
};

const MIN_CARRY_APPEARANCES: i64 = 30;
const SHRINK_C: f64 = 25.0; // 수축 강도 (가상 사전표본 개수)
const MIN_LIFT: f64 = 2.0;    // 풀템 기준 lift. 이 미만은 범용템 → 제외
const MIN_ITEM_PICKS: i64 = 10;


fn adjusted(avg: f64, picks: i64, prior_mean: f64) -> f64 {
    (avg * picks as f64 + prior_mean * SHRINK_C) / (picks as f64 + SHRINK_C)
}


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Config 로드 실패: {e}");
        std::process::exit(1);
    });
    let pool = db::connect(&cfg.database_url).await?;

    let Some(info) = db::current_patch_info(&pool).await? else {
        eprintln!("패치 데이터 없음.");
        return Ok(());
    };
    let (set_number, patch) = (info.set_number, info.patch.clone());
    eprintln!("대상: set {set_number}, patch {patch}");

    let meta = Meta::load(set_number).await?;
    let mut rng = rand::thread_rng();

    let carries = db::carry_candidates(&pool, set_number, &patch, MIN_CARRY_APPEARANCES).await?;
    eprintln!("캐리 후보 {}종", carries.len());

    let mut made = 0;
    let mut skipped_tank = 0;

    for (carry_id, appearances) in &carries {
        // 1) 캐리 타입 판정
        let types = db::carry_item_types(&pool, set_number, &patch, carry_id, MIN_ITEM_PICKS, 50).await?;
        let carry_type = db::classify_carry(&types);
        if carry_type == CarryType::Tank {
            skipped_tank += 1;
            continue; // 순수 탱커/애매 캐리 제외
        }

        
        
        let all = db::carry_item_full(&pool, set_number, &patch, carry_id, MIN_ITEM_PICKS).await?;
        if all.is_empty() {
            continue;
        }
        let prior = all[0].carry_mean;

        // lift >= 2.0 인 것만 정답 후보 (범용템 제외)
        let mut candidates: Vec<_> = all.iter().filter(|s| s.lift >= MIN_LIFT).cloned().collect();
        if candidates.is_empty() {
            continue;
        }
        // 베이지안 보정 평균으로 정렬 → 1위 정답
        candidates.sort_by(|a, b| {
            adjusted(a.avg_placement, a.picks, prior)
                .partial_cmp(&adjusted(b.avg_placement, b.picks, prior))
                .unwrap()
        });
        let answer = candidates[0].clone();

        // 숨은 픽: 정답 아니면서 lift 충분 + 픽률 낮(정답의 60% 미만) + 보정평균 좋은 것
        let hidden = candidates.iter()
            .skip(1)
            .filter(|s| s.picks < answer.picks * 60 / 100)
            .min_by(|a, b| {
                adjusted(a.avg_placement, a.picks, prior)
                    .partial_cmp(&adjusted(b.avg_placement, b.picks, prior))
                    .unwrap()
            })
            .cloned();


        // 3) 오답 허용 계열 결정
        let allowed: HashSet<&str> = allowed_distractor_types(carry_type, &answer.damage_type)
            .into_iter()
            .collect();

        // 4) 오답 후보: 먼저 이 캐리의 다른 아이템(같은 허용 계열)
        let mut distractors: Vec<(String, String)> = candidates
            .iter()
            .filter(|s| s.item != answer.item)
            .filter(|s| allowed.contains(s.damage_type.as_str()))
            .map(|s| (s.item.clone(), s.name.clone()))
            .collect();

        // 5) 부족하면 전역 계열 풀에서 보충
        if distractors.len() < 3 {
            let types_vec: Vec<String> = allowed.iter().map(|s| s.to_string()).collect();
            let mut pool_items = db::items_by_damage_type(&pool, &types_vec).await?;
            pool_items.shuffle(&mut rng);
            for (id, name) in pool_items {
                if distractors.len() >= 3 {
                    break;
                }
                if id == answer.item || distractors.iter().any(|(d, _)| d == &id) {
                    continue;
                }
                distractors.push((id, name));
            }
        }
        if distractors.len() < 3 {
            continue; // 오답 4개 못 채우면 스킵
        }

        // 6) 보기 구성 (정답 + 오답 3개) 셔플
        distractors.truncate(3);
        let mut opts: Vec<(String, String)> = vec![(answer.item.clone(), answer.name.clone())];
        opts.extend(distractors);
        opts.shuffle(&mut rng);

        let options: Vec<OptionItem> = opts
            .iter()
            .map(|(id, name)| OptionItem { id: id.clone(), name: name.clone() })
            .collect();

        // 보기별 통계 (이 캐리 기준, 없으면 null)
        let option_stats: Vec<ItemOptionStat> = opts
            .iter()
            .map(|(id, name)| {
                let st = all.iter().find(|s| &s.item == id);
                ItemOptionStat {
                    id: id.clone(),
                    name: name.clone(),
                    avg_placement: st.map(|s| (s.avg_placement * 100.0).round() / 100.0),
                    sample_size: st.map(|s| s.picks).unwrap_or(0),
                    is_best: *id == answer.item,
                }
            })
            .collect();

        let prompt = ItemPrompt {
            question: format!("{} 캐리에게 가장 좋은 아이템은?", meta.unit_name(carry_id)),
            carry: NamedRef { id: carry_id.clone(), name: meta.unit_name(carry_id) },
            context_traits: Vec::new(),
            patch: patch.clone(),
        };
        let stats_payload = serde_json::json!({
            "options": option_stats,
            "carry_appearances": appearances,
            "carry_type": carry_type_str(carry_type),
            "hidden_pick": hidden.as_ref().map(|h| serde_json::json!({
                "id": h.item,
                "name": h.name,
                "avg_placement": (h.avg_placement * 100.0).round() / 100.0,
                "sample_size": h.picks,
            })),
        });

        db::insert_item_puzzle(
            &pool, set_number, &patch, carry_id, carry_type_str(carry_type),
            &serde_json::to_value(&prompt)?, &serde_json::to_value(&options)?,
            &answer.item, &stats_payload,
        ).await?;
        made += 1;
    }

    eprintln!("아이템 퀴즈 {made}개 생성 (탱커/애매 {skipped_tank}종 제외)");
    Ok(())
}

/// 캐리 타입 + 정답 계열 → 오답으로 허용할 계열 목록.
fn allowed_distractor_types<'a>(carry: CarryType, answer_type: &'a str) -> Vec<&'a str> {
    match carry {
        // 브루저: 딜템 + 방어/브루저템 혼합 (딜 vs 탱 트레이드오프가 문제의 핵심)
        CarryType::Bruiser => vec!["ad", "ap", "mixed", "bruiser", "tank"],
        // 딜러: 정답과 같은 계열로 제한 (mixed는 양쪽에 호환)
        _ => match answer_type {
            "ad" => vec!["ad", "mixed"],
            "ap" => vec!["ap", "mixed"],
            "mixed" => vec!["ad", "ap", "mixed"],
            // 정답이 util/방어 계열이면(드묾) 딜템들로 변별
            other => vec!["ad", "ap", "mixed", other],
        },
    }
}

fn carry_type_str(t: CarryType) -> &'static str {
    match t {
        CarryType::Dealer => "dealer",
        CarryType::Bruiser => "bruiser",
        CarryType::Tank => "tank",
    }
}