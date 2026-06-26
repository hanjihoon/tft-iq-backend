//! 데이터 수집 크롤러.
//!
//! 파이프라인:
//!   1) 챌린저/그랜드마스터 리그 → puuid 수집 → tracked_players upsert
//!   2) 각 플레이어의 최근 매치 id 조회
//!   3) 매치 상세를 받아 raw_matches에 저장 (중복은 스킵)
//!
//! 실행:  cargo run --bin crawler
//! cron/스케줄러로 주기 실행(예: 6시간마다)하는 걸 가정한다.

use std::collections::HashSet;
use tft_iq::{Config, db, riot::RiotClient};
use tracing::{info, warn};

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
    eprintln!("2: 로거 초기화 완료");

    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Config 로드 실패: {e}");
        std::process::exit(1);
    });
    eprintln!("3: Config 로드 완료 - platform={}", cfg.riot_platform);

    let pool = db::connect(&cfg.database_url).await.unwrap_or_else(|e| {
        eprintln!("DB 연결 실패: {e}");
        std::process::exit(1);
    });
    eprintln!("4: DB 연결 완료");
    let riot = RiotClient::new(
        cfg.riot_api_key.clone(),
        cfg.riot_platform.clone(),
        cfg.riot_region.clone(),
    )?;

    // ── 1단계: 상위권 플레이어 갱신 ─────────────────────────
    refresh_top_players(&riot, &pool, &cfg.riot_region).await.unwrap_or_else(|e| {
        eprintln!("refresh_top_players 실패: {e}");
    });
    eprintln!("5: refresh_top_players 완료");

    // ── 2~3단계: 매치 수집 ─────────────────────────────────
    // 한 번 실행에 처리할 플레이어 수 (rate limit 고려해 보수적으로)
    let players = db::players_to_crawl(&pool, 30).await.unwrap_or_else(|e| {
        eprintln!("players_to_crawl 실패: {e}");
        vec![]
    });
    eprintln!("6: 크롤 대상 플레이어 {}명", players.len());
    
    let mut total_new = 0usize;
    let mut seen_matches: HashSet<String> = HashSet::new();

    for puuid in &players {
        match crawl_player(&riot, &pool, &cfg.riot_region, puuid, &mut seen_matches).await {
            Ok(n) => total_new += n,
            Err(e) => warn!("플레이어 {puuid} 크롤 실패: {e}"),
        }
        // 처리 완료 표시 (다음 사이클엔 뒤로 밀림)
        if let Err(e) = db::mark_crawled(&pool, puuid).await {
            warn!("mark_crawled 실패 {puuid}: {e}");
        }
    }

    info!("크롤 완료. 신규 매치 {total_new}건 저장.");
    Ok(())
}

/// 챌린저 + 그랜드마스터 엔트리를 tracked_players에 반영.
async fn refresh_top_players(
    riot: &RiotClient,
    pool: &sqlx::PgPool,
    region: &str,
) -> anyhow::Result<()> {
    // 패치 나이 판단 (데이터 없으면 0일 = 최신으로 간주, 최대 수집)
    let info = db::current_patch_info(&pool).await?;
    let age = info.as_ref().map(|i| i.age_days).unwrap_or(0.0);
    let tiers = tiers_for_age(age);

    match &info {
        Some(i) => eprintln!("패치 {} ({:.1}일차, {}판) → 티어 {:?}", i.patch, age, i.match_count, tiers),
        None    => eprintln!("패치 데이터 없음 → 최대 수집 모드, 티어 {:?}", tiers),
    }

    for tier in tiers {
        match riot.league(tier).await {
            Ok(league) => {
                for entry in &league.entries {
                    let Some(puuid) = &entry.puuid else { continue };
                    db::upsert_tracked_player(
                        &pool, puuid, &tier.to_uppercase(), entry.league_points, region,
                    ).await?;
                }
            }
            Err(e) => eprintln!("{tier} 리그 조회 실패: {e}"),
        }
    }
    db::reconcile_patch_versions(&pool).await?;
    Ok(())   // ← 이 줄 추가
}

/// 한 플레이어의 최근 매치를 수집해 저장. 반환: 신규 저장 건수.
async fn crawl_player(
    riot: &RiotClient,
    pool: &sqlx::PgPool,
    region: &str,
    puuid: &str,
    seen: &mut HashSet<String>,
) -> anyhow::Result<usize> {
    let match_ids = riot.match_ids(puuid, 20).await?;
    let mut new_count = 0;

    for mid in match_ids {
        // 이번 사이클에서 이미 다른 플레이어로 처리한 매치면 건너뜀
        if !seen.insert(mid.clone()) {
            continue;
        }
        let m = match riot.match_detail(&mid).await {
            Ok(m) => m,
            Err(e) => {
                warn!("매치 {mid} 조회 실패: {e}");
                continue;
            }
        };

        // 랭크 단식(1100)만 보관하고 싶다면 여기서 필터
        // if m.info.queue_id != 1100 { continue; }

        if db::insert_raw_match(pool, &m, region).await? {
            new_count += 1;
        }
    }
    Ok(new_count)
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