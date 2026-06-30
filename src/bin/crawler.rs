//! 운영용 크롤러 (증분 수집).
//!
//! 핵심: 플레이어마다 last_crawled_at "이후" 매치만 조회 → 같은 구간을 다시 안 긁음.
//! 첫 수집(last_crawled_at = NULL)인 플레이어는 패치 시작 시각부터 받아 과거를 채운다.
//! 티어 풀은 표본 수에 따라 자동 조절(부족하면 GM·마스터까지).
//!
//! 실행:  cargo run --bin crawler   (cron으로 주기 실행 가정)

use std::collections::HashSet;
use tft_iq::{Config, db, riot::RiotClient};
use tracing::{info, warn};

/// 한 사이클에 처리할 플레이어 수
const PLAYERS_PER_CYCLE: i64 = 50;
/// 티어별 사용할 최대 플레이어 수
const MAX_PLAYERS_PER_TIER: usize = 500;
/// 증분 조회 시 마지막 수집 시각에서 빼는 안전 마진(초). 막 끝난 게임 누락 방지.
const CRAWL_MARGIN_SECS: i64 = 2 * 3600;

/// 표본 수 → 수집 티어. (crawler_dev와 동일 정책)
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tft_iq=info".into()),
        )
        .init();

    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;
    let riot = RiotClient::new(
        cfg.riot_api_key.clone(),
        cfg.riot_platform.clone(),
        cfg.riot_region.clone(),
    )?;

    // 현재 패치 상태: 표본 수 / 패치 시작 시각 / 목표 패치 문자열
    let count = db::current_patch_match_count(&pool).await.unwrap_or(0);
    let patch_start = db::current_patch_start_time(&pool).await.unwrap_or(None);
    let target_patch: Option<String> = db::current_patch_info(&pool)
        .await
        .ok()
        .flatten()
        .map(|i| i.patch);

    info!("표본 {count}건, 목표패치 {target_patch:?}, 패치시작 {patch_start:?}");

    // ── 1단계: 표본 기반 티어 풀로 상위권 갱신 ───────────────
    refresh_top_players(&riot, &pool, &cfg.riot_region, tiers_for_sample(count)).await?;

    // ── 2~3단계: 증분 매치 수집 ─────────────────────────────
    let players = db::players_to_crawl(&pool, PLAYERS_PER_CYCLE).await?;
    info!("이번 사이클 대상 {}명", players.len());

    let mut total_new = 0usize;
    let mut seen: HashSet<String> = HashSet::new();

    for puuid in &players {
        // 이 플레이어의 시작 시각 결정:
        //   last_crawled 있으면 그 시각 - 마진 (증분)
        //   없으면 패치 시작 시각 (첫 수집 = 과거 백필)
        let last = db::player_last_crawled_epoch(&pool, puuid).await.unwrap_or(None);
        let start_time = last.map(|t| t - CRAWL_MARGIN_SECS).or(patch_start);

        match crawl_player(&riot, &pool, &cfg, puuid, start_time, target_patch.as_deref(), &mut seen).await {
            Ok(n) => total_new += n,
            Err(e) => warn!("플레이어 {puuid} 크롤 실패: {e}"),
        }
        if let Err(e) = db::mark_crawled(&pool, puuid).await {
            warn!("mark_crawled 실패 {puuid}: {e}");
        }
    }

    info!("크롤 완료. 신규 매치 {total_new}건 저장.");
    db::reconcile_patch_versions(&pool).await?;
    Ok(())
}

async fn refresh_top_players(
    riot: &RiotClient,
    pool: &sqlx::PgPool,
    region: &str,
    tiers: &[&str],
) -> anyhow::Result<()> {
    let mut count = 0;
    for tier in tiers {
        let league = match riot.league(tier).await {
            Ok(l) => l,
            Err(e) => {
                warn!("{tier} 리그 조회 실패: {e}");
                continue;
            }
        };
        // LP 상위 우선 (내림차순)
        let mut entries = league.entries;
        entries.sort_by_key(|e| -e.league_points);

        for entry in entries.into_iter().take(MAX_PLAYERS_PER_TIER) {
            let Some(puuid) = entry.puuid else { continue };
            db::upsert_tracked_player(pool, &puuid, &tier.to_uppercase(), entry.league_points, region).await?;
            count += 1;
        }
    }
    info!("상위권 플레이어 {count}명 갱신 (티어 {tiers:?})");
    Ok(())
}

/// 한 플레이어의 신규 매치를 증분으로 수집. 반환: 저장 건수.
async fn crawl_player(
    riot: &RiotClient,
    pool: &sqlx::PgPool,
    cfg: &Config,
    puuid: &str,
    start_time: Option<i64>,
    target_patch: Option<&str>,
    seen: &mut HashSet<String>,
) -> anyhow::Result<usize> {
    // start_time 이후 매치만 받음 (증분). None이면 전체.
    let match_ids = riot.match_ids_since(puuid, 100, start_time).await?;

    // 이미 가진/이번에 본 매치는 상세 조회 전에 제거 (rate limit 절약)
    let already = db::existing_match_ids(pool, &match_ids).await.unwrap_or_default();
    let new_ids: Vec<String> = match_ids
        .into_iter()
        .filter(|id| !already.contains(id) && !seen.contains(id))
        .collect();

    let mut new_count = 0;
    for mid in new_ids {
        seen.insert(mid.clone());
        let m = match riot.match_detail(&mid).await {
            Ok(m) => m,
            Err(e) => {
                warn!("매치 {mid} 조회 실패: {e}");
                continue;
            }
        };

        // 안전망: 목표 패치만 저장 (경계에서 새는 지난 패치 차단)
        if let Some(tp) = target_patch {
            if m.info.patch() != tp {
                continue;
            }
        }

        if db::insert_raw_match(pool, &m, &cfg.riot_region).await? {
            new_count += 1;
        }
    }
    Ok(new_count)
}