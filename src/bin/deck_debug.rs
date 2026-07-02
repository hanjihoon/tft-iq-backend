//! 변형 흡수 결과를 눈으로 확인하는 디버그 바이너리.
//! cargo run --bin deck_debug

use tft_iq::{db, deck_cluster::cluster_decks, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env().expect("Config 로드 실패");
    let pool = db::connect(&cfg.database_url).await?;

    // 현재 패치 확인
    let Some(info) = db::current_patch_info(&pool).await? else {
        eprintln!("패치 정보 없음");
        return Ok(());
    };
    println!("패치: {}\n", info.patch);

    // 원시 덱 로드 (40판 이상)
    let raw = db::raw_decks(&pool, &info.patch, 40).await?;
    println!("원시 덱 {}개 로드\n", raw.len());

    // 변형 흡수 (공통 7개 이상 = 1기물 차이만 병합)
    let clusters = cluster_decks(raw, 7);
    println!("=> 대표덱 {}개로 압축", clusters.len());
    let clusters = tft_iq::deck_cluster::filter_tier_decks(clusters, 100);
    println!("=> 티어덱 {}개로 필터 (순방덱은 100판+만)\n", clusters.len());
    println!("{}", "=".repeat(60));

    // 강한 순으로 출력
    for (i, c) in clusters.iter().enumerate() {
        println!(
            "\n[{}] 대표덱  (변형 {}개, 총 {}판, 최고 평균 {:.2}등)",
            i + 1, c.variants.len(), c.total_games, c.best_avg
        );
        println!("  코어 {}개: {}", c.core.len(), c.core.join(", "));
        // 캐리 찾기 (검증용)
        let all_units: Vec<String> = c.variants.iter()
            .flat_map(|v| v.units.iter().cloned())
            .collect();
        if let Some(carry) = db::deck_carry(&pool, &info.patch, &all_units).await? {
            println!("  => 캐리: {}", carry);
        }

        // 각 변형의 플렉스 유닛(코어 아닌 것)과 성적
        for v in &c.variants {
            let flex: Vec<&String> = v.units.iter().filter(|u| !c.core.contains(u)).collect();
            let flex_str = flex.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("+");
            println!(
                "    변형: [{}]  {}판 {:.2}등",
                flex_str, v.games, v.avg_placement
            );
        }
    }

    Ok(())
}