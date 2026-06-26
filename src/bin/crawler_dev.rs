//! 개발/테스트용 경량 크롤러.
//!
//! tracked_players 갱신 없이 챌린저 리그에서 puuid를 직접 뽑아
//! raw_matches만 빠르게 채운다. 개발 API 키 한도 안에서 5~7분 안에 완료.
//!
//! 실행:  cargo run --bin crawler_dev
//!
//! 상수를 조정해 수집량을 조절할 수 있다:
//!   TARGET_MATCHES   : 목표 저장 건수 (기본 300)
//!   PLAYERS_TO_USE   : 사용할 플레이어 수 (기본 20)
//!   MATCHES_PER_PLAYER: 플레이어당 조회 매치 수 (기본 20)

use std::collections::HashSet;
use tft_iq::{Config, db, riot::RiotClient};
use std::time::Instant;

const TARGET_MATCHES: usize = 1000;
const PLAYERS_TO_USE: usize = 300;   // 20 → 100 (챌린저 풀 더 활용)
const MATCHES_PER_PLAYER: u32 = 200;

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
    eprintln!("2: Config 로드 완료 - platform={}", cfg.riot_platform);

    let pool = db::connect(&cfg.database_url).await.unwrap_or_else(|e| {
        eprintln!("DB 연결 실패: {e}");
        std::process::exit(1);
    });
    eprintln!("3: DB 연결 완료");

    let riot = RiotClient::new(
        cfg.riot_api_key.clone(),
        cfg.riot_platform.clone(),
        cfg.riot_region.clone(),
    )?;

    // ── 1단계: 챌린저 리그에서 puuid 직접 추출 ──────────────
    eprintln!("4: 챌린저 리그 조회 중...");
    let league = riot.challenger_league().await.unwrap_or_else(|e| {
        eprintln!("챌린저 리그 조회 실패: {e}");
        std::process::exit(1);
    });

    let puuids: Vec<String> = league.entries
        .iter()
        .filter_map(|e| e.puuid.clone())
        .take(PLAYERS_TO_USE)
        .collect();

    eprintln!("5: puuid {}개 추출 완료", puuids.len());

    // ── 2단계: 매치 수집 ─────────────────────────────────────
    let mut seen: HashSet<String> = HashSet::new();
    let mut total = 0usize;

    for (i, puuid) in puuids.iter().enumerate() {
        if total >= TARGET_MATCHES {
            break;
        }

        eprintln!("[{}/{}] {} 매치 조회 중... (현재 {}건 저장)",
            i + 1, puuids.len(), &puuid[..8], total);

        let match_ids = match riot.match_ids(puuid, MATCHES_PER_PLAYER).await {
            Ok(ids) => ids,
            Err(e) => { eprintln!("  매치 ID 조회 실패: {e}"); continue; }
        };

        // ★ API 상세 조회 전에, 이미 가진 매치 ID를 제거
        let already = db::existing_match_ids(&pool, &match_ids).await.unwrap_or_default();
        let new_ids: Vec<String> = match_ids.into_iter()
            .filter(|id| !already.contains(id) && !seen.contains(id))
            .collect();

        eprintln!("  신규 매치 {}개 (중복 {}개 스킵)", new_ids.len(), already.len());

        for mid in new_ids {
            if total >= TARGET_MATCHES { break; }
            seen.insert(mid.clone());

            let t = Instant::now();
            let m = match riot.match_detail(&mid).await {
                Ok(m) => m,
                Err(e) => { eprintln!("  매치 {} 실패: {e}", &mid); continue; }
            };
            let elapsed = t.elapsed().as_millis();

            match db::insert_raw_match(&pool, &m, &cfg.riot_region).await {
                Ok(_) => {
                    total += 1;
                    // 매 건마다 소요시간 표시 — 1000ms 넘게 걸리면 rate limit 대기 중인 것
                    eprintln!("  [{:>4}/{}] {} ({}ms)", total, TARGET_MATCHES, &mid, elapsed);
                }
                Err(e) => eprintln!("  DB 저장 실패: {e}"),
            }
        }
    }

    eprintln!("완료: raw_matches {}건 저장", total);
    db::reconcile_patch_versions(&pool).await?;
    Ok(())
}

/// 패치 나이에 따라 수집할 티어를 결정.
/// 어릴수록 넓게(표본 우선), 성숙할수록 좁게(순도 우선).
fn tiers_for_age(age_days: f64) -> &'static [&'static str] {
    if age_days < 3.0 {
        &["challenger", "grandmaster", "master"] // 패치 직후
    } else if age_days < 7.0 {
        &["challenger", "grandmaster"]
    } else {
        &["challenger"] // 성숙기
    }
}