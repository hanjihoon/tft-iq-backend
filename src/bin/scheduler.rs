//! 자동 스케줄러.
//!
//! {크롤러 → 퍼즐 생성 → 대기}를 무한 반복한다.
//! 기존 바이너리(crawler, item_quiz_gen)를 자식 프로세스로 실행하는 방식이라
//! 기존 코드 리팩터링 없이 바로 동작한다.
//!
//! 실행:  cargo run --bin scheduler
//! 배포:  Fly.io에 상시 실행 머신으로 띄움
//!
//! 환경변수:
//!   SCHED_INTERVAL_SECS  반복 간격(초). 기본 14400(4시간).

use std::time::Duration;
use tokio::process::Command;
use tft_iq::{Config, db};

/// 퀴즈 재생성에 필요한 최소 매치 수.
/// 이 미만이면 표본 부족으로 퀴즈가 빈약해지므로 재생성을 건너뛴다.
/// (기존 퀴즈는 그대로 서빙 → 공백 방지)
const MIN_MATCHES_FOR_REGEN: i64 = 15000;

/// 퀴즈 재생성 주기 = 크롤 주기 × 이 값.
/// 크롤은 자주(데이터 축적), 퀴즈 생성은 가끔(비용 절약).
const REGEN_EVERY_N_CYCLES: u32 = 4;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env()?;

    // 크롤 주기. 기본 6시간 (새 패치 초반 축적 속도 확보)
    let interval_secs: u64 = std::env::var("SCHED_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(21600);

    eprintln!(
        "스케줄러 시작 — 크롤 {}시간마다, 퀴즈 재생성 {}사이클마다({}시간)",
        interval_secs / 3600,
        REGEN_EVERY_N_CYCLES,
        interval_secs * REGEN_EVERY_N_CYCLES as u64 / 3600
    );

    let mut cycle: u32 = 0;

    loop {
        cycle += 1;
        eprintln!("\n=== 사이클 {cycle} 시작 {} ===", now());

        // ── 1) 크롤 ──
        let crawl_ok = run_step("crawler").await;
        if !crawl_ok {
            eprintln!("크롤 실패 → 이번 사이클 나머지 건너뜀 (옛 데이터 보존)");
            sleep_cycle(interval_secs).await;
            continue;
        }

        // ── 2) 집계 (크롤 성공 시에만) ──
        let agg_ok = run_step("aggregate_combos").await;
        let special_agg_ok = run_step("aggregate_special").await;

        if !agg_ok {
            eprintln!("집계 실패 → 퀴즈 재생성 건너뜀 (옛 퀴즈 보존)");
            sleep_cycle(interval_secs).await;
            continue;
        }

        // ── 3) 퀴즈 재생성 (N사이클마다 + 표본 충분할 때만) ──
        if cycle % REGEN_EVERY_N_CYCLES == 0 {
            match should_regenerate(&cfg).await {
                Ok(true) => {
                    eprintln!("표본 충분 → 퀴즈 재생성");
                    run_step("combo_quiz_gen").await;
                    run_step("deck_quiz_gen").await;
                    if special_agg_ok {
                        // 특수템 분류는 cdragon 기반이라 패치마다 갱신 필요
                        run_step("special_item_class").await;
                    }
                }
                Ok(false) => {
                    eprintln!("표본 부족 → 퀴즈 재생성 건너뜀 (기존 퀴즈 유지)");
                }
                Err(e) => {
                    eprintln!("표본 확인 실패: {e} → 안전하게 재생성 건너뜀");
                }
            }
        } else {
            eprintln!("이번 사이클은 크롤/집계만 (재생성은 {}사이클마다)", REGEN_EVERY_N_CYCLES);
        }

        sleep_cycle(interval_secs).await;
    }
}

/// 현재 패치의 매치 수가 퀴즈 재생성 임계를 넘었는지.
async fn should_regenerate(cfg: &Config) -> anyhow::Result<bool> {
    let pool = db::connect(&cfg.database_url).await?;
    let count = db::current_patch_match_count(&pool).await.unwrap_or(0);
    pool.close().await;   // 커넥션 반환 (풀 고갈 방지)
    eprintln!("현재 패치 표본: {count}건 (임계 {MIN_MATCHES_FOR_REGEN})");
    Ok(count >= MIN_MATCHES_FOR_REGEN)
}

async fn sleep_cycle(secs: u64) {
    eprintln!("=== 사이클 완료, {}시간 대기 ===", secs / 3600);
    tokio::time::sleep(Duration::from_secs(secs)).await;
}

/// 성공 여부 반환 (단계 의존성 판단용)
async fn run_step(bin: &str) -> bool {
    eprintln!("[{}] {bin} 실행…", now());
    let program = format!("/app/{bin}");
    match Command::new(&program).status().await {
        Ok(s) if s.success() => {
            eprintln!("[{}] {bin} 완료", now());
            true
        }
        Ok(s) => {
            eprintln!("[{}] {bin} 비정상 종료: {s}", now());
            false
        }
        Err(e) => {
            eprintln!("[{}] {bin} 실행 실패: {e}", now());
            false
        }
    }
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%SZ").to_string()
}