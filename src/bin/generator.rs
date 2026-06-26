//! 퍼즐 생성 워커.
//!
//! raw_matches → puzzles. 지금은 오그먼트 선택 퍼즐(AugmentPick)만 생성한다.
//!
//! 로직:
//!   1) 가장 최근 (set, patch) 선택
//!   2) 오그먼트별 평균 등수 집계 (전체 풀)
//!   3) 상위권 보드(placement <= 4)를 골라 문제 상황으로 사용
//!   4) 실제 픽 1개 + 같은 패치 풀에서 뽑은 오답 3개 → 보기 4개
//!   5) 정답 = 보기 중 평균 등수가 가장 좋은(낮은) 것
//!   6) stats에 보기별 평균 등수를 담아 교육적 피드백 제공
//!
//! 실행:  cargo run --bin generator

use std::collections::HashMap;

use rand::seq::SliceRandom;
use tft_iq::{
    Config, db,
    meta::Meta,
    puzzle::{
        BoardContext, NamedRef, OptionItem, OptionStat, Prompt, PuzzleKind, Stats, TraitView,
        UnitView,
    },
    riot::dto::Participant,
};
use tracing::{info, warn};

/// 한 번 실행에 만들 최대 퍼즐 수
const MAX_PUZZLES: usize = 60;
/// 오답 풀에 포함시키려면 최소 이만큼은 픽돼야 (표본 신뢰도)
const MIN_PICKS: i64 = 3;  // 테스트용
/// 문제로 쓸 보드의 최대 등수 (상위권 보드만)
const MAX_PLACEMENT_FOR_SOURCE: i32 = 4;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    eprintln!("1: dotenv 완료");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tft_iq=debug".into()),
        )
        .init();

    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Config 로드 실패: {e}");
        std::process::exit(1);
    });
    eprintln!("2: Config 로드 완료");

    let pool = db::connect(&cfg.database_url).await.unwrap_or_else(|e| {
        eprintln!("DB 연결 실패: {e}");
        std::process::exit(1);
    });
    eprintln!("3: DB 연결 완료");

    let Some((set_number, patch)) = db::latest_patch(&pool).await.unwrap_or_else(|e| {
        eprintln!("latest_patch 실패: {e}");
        std::process::exit(1);
    }) else {
        eprintln!("raw_matches가 비어 있음. crawler_dev를 먼저 실행해라.");
        return Ok(());
    };
    eprintln!("4: 대상 set={set_number}, patch={patch}");

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM raw_matches WHERE set_number = $1 AND patch = $2")
        .bind(set_number)
        .bind(&patch)
        .fetch_one(&pool)
        .await
        .unwrap_or((0,));
    eprintln!("4-1: 조건 매칭 raw_matches 건수={}", count.0);

    let stats = db::augment_placement_stats(&pool, set_number, &patch, MIN_PICKS)
        .await
        .unwrap_or_else(|e| {
            eprintln!("augment_placement_stats 실패: {e}");
            std::process::exit(1);
        });
    eprintln!("5: 오그먼트 통계 {}개 집계됨", stats.len());

    if stats.len() < 4 {
        eprintln!("오그먼트 통계 부족 ({}개). MIN_PICKS를 낮추거나 데이터를 더 수집해야 함.", stats.len());
        return Ok(());
    }
    let stat_map: HashMap<String, db::AugStat> =
        stats.iter().cloned().map(|s| (s.id.clone(), s)).collect();
    let pool_ids: Vec<String> = stats.iter().map(|s| s.id.clone()).collect();

    // 메타데이터(한글 이름)
    let meta = Meta::load(set_number).await?;

    // 3) 소스 매치 로드
    let matches = db::load_matches(&pool, set_number, &patch, 300).await?;
    info!("소스 매치 {}건 로드", matches.len());

    let mut rng = rand::thread_rng();
    let mut made = 0usize;

    'outer: for m in &matches {
        for p in &m.info.participants {
            if made >= MAX_PUZZLES {
                break 'outer;
            }
            if p.placement > MAX_PLACEMENT_FOR_SOURCE || p.augments.is_empty() {
                continue;
            }

            // 정답 후보가 될 "실제 픽" — 집계에 존재하는 것 중 하나
            let Some(actual) = p
                .augments
                .iter()
                .find(|a| stat_map.contains_key(*a))
                .cloned()
            else {
                continue;
            };

            // 오답: 같은 풀에서, 이 참가자가 안 고른 것 중 3개
            let distractors: Vec<String> = pool_ids
                .iter()
                .filter(|id| !p.augments.contains(*id))
                .cloned()
                .collect::<Vec<_>>()
                .choose_multiple(&mut rng, 3)
                .cloned()
                .collect();
            if distractors.len() < 3 {
                continue;
            }

            // 보기 구성 + 셔플
            let mut option_ids = vec![actual.clone()];
            option_ids.extend(distractors);
            option_ids.shuffle(&mut rng);

            // 정답 = 평균 등수가 가장 좋은(낮은) 보기
            let answer = option_ids
                .iter()
                .min_by(|a, b| {
                    stat_map[*a]
                        .avg_placement
                        .partial_cmp(&stat_map[*b].avg_placement)
                        .unwrap()
                })
                .cloned()
                .unwrap();

            // 직렬화용 구조체 빌드
            let options: Vec<OptionItem> = option_ids
                .iter()
                .map(|id| OptionItem {
                    id: id.clone(),
                    name: meta.augment_name(id),
                })
                .collect();

            let option_stats: Vec<OptionStat> = option_ids
                .iter()
                .map(|id| {
                    let s = &stat_map[id];
                    OptionStat {
                        id: id.clone(),
                        avg_placement: (s.avg_placement * 100.0).round() / 100.0,
                        sample_size: s.picks,
                        was_actual_pick: *id == actual,
                    }
                })
                .collect();

            let prompt = Prompt {
                question: "이 챌린저 보드에서 어떤 오그먼트가 평균 등수가 가장 좋았을까?".into(),
                context: build_context(p, &meta),
            };
            let stats_payload = Stats {
                options: option_stats,
                source_match_id: m.metadata.match_id.clone(),
            };

            db::insert_puzzle(
                &pool,
                PuzzleKind::AugmentPick.as_str(),
                set_number,
                &patch,
                &serde_json::to_value(&prompt)?,
                &serde_json::to_value(&options)?,
                &answer,
                &serde_json::to_value(&stats_payload)?,
                Some(&m.metadata.match_id),
            )
            .await?;

            made += 1;
            break; // 매치당 1퍼즐로 다양성 확보
        }
    }

    info!("퍼즐 {made}개 생성 완료.");
    Ok(())
}

/// 참가자 최종 스냅샷 → 퍼즐 맥락(한글 이름 포함).
fn build_context(p: &Participant, meta: &Meta) -> BoardContext {
    let traits = p
        .traits
        .iter()
        .filter(|t| t.tier_current > 0) // 활성 특성만
        .map(|t| TraitView {
            id: t.name.clone(),
            name: t.name.clone(), // 특성 한글 매핑은 추후 meta에 추가
            tier_current: t.tier_current,
        })
        .collect();

    let units = p
        .units
        .iter()
        .map(|u| UnitView {
            id: u.character_id.clone(),
            name: meta.unit_name(&u.character_id),
            cost: meta.unit_cost(&u.character_id, u.rarity + 1),
            star: u.tier,
            items: u
                .item_names
                .iter()
                .map(|i| NamedRef {
                    id: i.clone(),
                    name: meta.item_name(i),
                })
                .collect(),
        })
        .collect();

    BoardContext {
        level: p.level,
        last_round: p.last_round,
        placement: p.placement,
        traits,
        units,
        prior_augments: Vec::new(),
    }
}