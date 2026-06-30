//! 대량 백필 크롤러 (Personal Key).
//!
//! 표본 수에 따라 수집 티어 풀을 자동 조절한다:
//!   - 표본 부족(< 1500)  → Challenger + GM + Master(LP 상위)  : 표본 총동원
//!   - 중간(< 3000)       → Challenger + GM                    : 균형
//!   - 충분(>= 3000)      → Challenger                         : 순도 우선
//! "며칠 지났나"(추정)가 아니라 "표본이 얼마나 모였나"(사실)로 결정.
//!
//! 실행:  cargo run --bin crawler_dev

use std::collections::HashSet;

use tft_iq::{Config, db, riot::RiotClient};

const TARGET_MATCHES: usize = 3000;
/// 티어별로 사용할 최대 플레이어 수 (마스터는 너무 많아 상위만)
const MAX_PLAYERS_PER_TIER: usize = 500;
const MATCHES_PER_PLAYER: u32 = 100; // API 최대 100

/// 현재 표본 수에 따라 긁을 티어 목록을 결정.
///
/// 반환 타입 `&'static [&'static str]`에 주목:
/// - 분기마다 고정된 문자열 배열의 "참조"만 돌려준다 → 힙 할당 0.
/// - 'static은 "프로그램 끝까지 사는 데이터"라는 수명 표시. 리터럴 배열이라 가능.
fn tiers_for_sample(match_count: i64) -> &'static [&'static str] {
    if match_count >= 3000 {
        &["challenger"]
    } else if match_count >= 1500 {
        &["challenger", "grandmaster"]
    } else {
        &["challenger", "grandmaster", "master"]
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Config 로드 실패: {e}");
        std::process::exit(1);
    });
    let pool = db::connect(&cfg.database_url).await?;
    let riot = RiotClient::new(
        cfg.riot_api_key.clone(),
        cfg.riot_platform.clone(),
        cfg.riot_region.clone(),
    )?;

    // ── 표본 수 → 티어 풀 결정 ──────────────────────────────
    let count = db::current_patch_match_count(&pool).await.unwrap_or(0);
    let tiers = tiers_for_sample(count);
    eprintln!("현재 표본 {count}건 → 수집 티어 {tiers:?}");

    // ── 1단계: 티어별 puuid 수집 (LP 상위 우선) ──────────────
    let mut puuids: Vec<String> = Vec::new();
    let mut seen_puuid: HashSet<String> = HashSet::new();

    for tier in tiers {
        let league = match riot.league(tier).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("{tier} 리그 조회 실패: {e}");
                continue;
            }
        };

        // entries를 league_points 내림차순으로 정렬 → 고LP(고수)부터.
        // sort_by_key는 키가 Ord여야 함. i32는 Ord라 OK. 내림차순은 음수화로.
        let mut entries = league.entries;
        entries.sort_by_key(|e| -e.league_points);

        let mut added = 0;
        for e in entries {
            if added >= MAX_PLAYERS_PER_TIER {
                break;
            }
            // puuid가 있고(Some) 아직 안 본 것만 추가.
            // if let Some(p) = ... 는 Option에서 값을 꺼내는 Rust 관용구.
            if let Some(p) = e.puuid {
                if seen_puuid.insert(p.clone()) {
                    // insert는 "새로 들어갔으면 true". 중복이면 false라 건너뜀.
                    puuids.push(p);
                    added += 1;
                }
            }
        }
        eprintln!("  {tier}: {added}명 추가 (누적 {}명)", puuids.len());
    }

    // 표본/티어 결정 다음에 추가
    let start_time = db::current_patch_start_time(&pool).await.unwrap_or(None);
    match start_time {
        Some(t) => eprintln!("startTime={t} 이후 매치만 조회 (지난 패치 제외)"),
        None => eprintln!("startTime 없음 — 첫 수집(패치 경계 발견 모드)"),
    }

    // ── 2단계: 매치 수집 ─────────────────────────────────────
    let mut seen_match: HashSet<String> = HashSet::new();
    let mut total = 0usize;

    'outer: for (i, puuid) in puuids.iter().enumerate() {
        if total >= TARGET_MATCHES {
            break;
        }
        if i % 20 == 0 {
            eprintln!("[플레이어 {}/{}] 누적 {}건", i + 1, puuids.len(), total);
        }

        let match_ids = match riot.match_ids_since(puuid, MATCHES_PER_PLAYER, start_time).await {
            Ok(ids) => ids,
            Err(e) => {
                eprintln!("  매치 ID 조회 실패: {e}");
                continue;
            }
        };

        // API 상세 조회 전에 이미 가진/이번에 본 매치를 제거 → rate limit 절약.
        let already = db::existing_match_ids(&pool, &match_ids).await.unwrap_or_default();
        let new_ids: Vec<String> = match_ids
            .into_iter()
            .filter(|id| !already.contains(id) && !seen_match.contains(id))
            .collect();

        let target_patch: Option<String> = db::current_patch_info(&pool)
            .await
            .ok()
            .flatten()
            .map(|i| i.patch);

        for mid in new_ids {
            if total >= TARGET_MATCHES {
                break 'outer; // 라벨로 바깥 루프까지 한 번에 탈출
            }
            seen_match.insert(mid.clone());

            let m = match riot.match_detail(&mid).await {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("  매치 {mid} 실패: {e}");
                    continue;
                }
            };

            // 안전망: 목표 패치가 정해져 있으면 그 패치만 저장
            if let Some(tp) = &target_patch {
                if &m.info.patch() != tp {
                    continue;  // 지난 패치 매치 스킵
                }
            }

            match db::insert_raw_match(&pool, &m, &cfg.riot_region).await {
                Ok(true) => {
                    total += 1;
                    if total % 100 == 0 {
                        eprintln!("  진행: {total}/{TARGET_MATCHES}건 저장");
                    }
                }
                Ok(false) => {} // 동시 중복(드묾)
                Err(e) => eprintln!("  DB 저장 실패: {e}"),
            }
        }
    }

    eprintln!("완료: 신규 {total}건 저장");
    db::reconcile_patch_versions(&pool).await?;

    let final_count = db::current_patch_match_count(&pool).await.unwrap_or(0);
    eprintln!("현재 패치 총 표본: {final_count}건");
    Ok(())
}