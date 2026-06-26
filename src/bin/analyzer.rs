//! 컴프 티어 분석기.
//!
//! raw_matches를 읽어 각 보드를 컴프로 분류하고, 컴프별 평균 등수를 집계해
//! 콘솔에 티어표를 출력한다. 1000매치로 분류·집계 로직이 말이 되는지 검증하는 용도.
//!
//! 실행:  cargo run --bin analyzer

use std::collections::HashMap;

use tft_iq::{
    Config,
    comp::{self, CompKey},
    db,
};

/// 티어표에 올릴 최소 표본 수 (이보다 적게 등장한 컴프는 노이즈로 제외)
// const MIN_SAMPLE: i64 = 10;
// TEST 650매치 검증용
const MIN_SAMPLE: i64 = 5;

/// 컴프별 누적 집계기. sum/count만 들고 있다가 마지막에 평균을 낸다.
/// (평균을 매번 다시 계산하지 않고 합계만 누적하는 게 정확하고 빠르다)
#[derive(Default)]
struct Agg {
    sum: i64,
    count: i64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Config 로드 실패: {e}");
        std::process::exit(1);
    });
    let pool = db::connect(&cfg.database_url).await?;

    let Some((set_number, patch)) = db::latest_patch(&pool).await? else {
        eprintln!("raw_matches가 비어 있음. crawler_dev를 먼저 실행해라.");
        return Ok(());
    };
    println!("대상: set {set_number}, patch {patch}");

    let matches = db::load_matches(&pool, set_number, &patch, 5000).await?;
    println!("매치 {}건 로드", matches.len());

    // ── 집계 ────────────────────────────────────────────────
    // HashMap<CompKey, Agg> : 컴프를 키로, 누적값을 값으로.
    let mut table: HashMap<CompKey, Agg> = HashMap::new();
    let mut boards = 0u64;

    for m in &matches {
        for p in &m.info.participants {
            let key = comp::classify(p);

            // entry API: 키가 있으면 그 값의 가변 참조를, 없으면 기본값을 넣고 그 참조를 준다.
            // 이 한 줄이 "조회 → 없으면 삽입 → 수정"을 빌림 검사기와 충돌 없이 처리한다.
            let agg = table.entry(key).or_default();
            agg.sum += p.placement as i64;
            agg.count += 1;
            boards += 1;
        }
    }
    println!("보드 {boards}개 분류, 고유 컴프 {}종\n", table.len());

    // ── 티어표 구성 ──────────────────────────────────────────
    // HashMap을 (키, 평균, 표본) 튜플 Vec로 변환하면서 표본 부족분은 걸러낸다.
    let mut rows: Vec<(CompKey, f64, i64)> = table
        .into_iter() // 소유권째로 꺼낸다 (이후 table은 못 씀)
        .filter(|(_key, a)| a.count >= MIN_SAMPLE)
        .map(|(key, a)| {
            let avg = a.sum as f64 / a.count as f64;
            (key, avg, a.count)
        })
        .collect();

    // 평균 등수 오름차순(낮을수록 강함) 정렬.
    // f64는 NaN 때문에 전순서(Ord)가 없어 cmp를 못 쓴다 → total_cmp로 안전하게 비교.
    rows.sort_by(|a, b| a.1.total_cmp(&b.1));

    // ── 출력 ────────────────────────────────────────────────
    println!("{:<48} {:>7} {:>6}", "컴프", "평균등수", "표본");
    println!("{}", "-".repeat(63));
    for (key, avg, n) in &rows {
        // {:<48} 좌측정렬 48칸, {:>7.2} 우측정렬 소수 2자리
        println!("{:<48} {:>7.2} {:>6}", key.to_string(), avg, n);
    }

    Ok(())
}