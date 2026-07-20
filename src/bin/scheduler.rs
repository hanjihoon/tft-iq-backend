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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    let interval_secs: u64 = std::env::var("SCHED_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(172800); // 48시간

    eprintln!("스케줄러 시작 — {interval_secs}초마다 반복 ({}시간)", interval_secs / 3600);

    loop {
        eprintln!("\n=== 사이클 시작 {} ===", now());

        // 1) 크롤러 — 새 매치 수집
        run_step("crawler").await;
        // 2) 집계 — 3템 통계 갱신 (필수!)
        run_step("aggregate_combos").await;
        // 3) 퍼즐 재생성
        run_step("combo_quiz_gen").await;   // 3템 (bis 아님)
        run_step("deck_quiz_gen").await;

        eprintln!("=== 사이클 완료, {interval_secs}초 대기 ({}시간) ===", interval_secs / 3600);
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
    }
}

async fn run_step(bin: &str) {
    eprintln!("[{}] {bin} 실행…", now());
    let program = format!("/app/{bin}");
    let status = Command::new(&program).status().await;
    match status {
        Ok(s) if s.success() => eprintln!("[{}] {bin} 완료", now()),
        Ok(s) => eprintln!("[{}] {bin} 비정상 종료: {s}", now()),
        Err(e) => eprintln!("[{}] {bin} 실행 실패: {e}", now()),
    }
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%SZ").to_string()
}