//! 원시 덱들을 "변형 흡수"로 대표덱으로 묶는다.
//! 공통 유닛 7개 이상(=1기물 차이)이면 같은 덱으로 병합.

use std::collections::HashSet;
use crate::db::RawDeck;

/// 대표덱: 여러 변형을 흡수한 결과.
#[derive(Debug, Clone)]
pub struct DeckCluster {
    pub core: Vec<String>,        // 모든 변형에 공통인 유닛 (코어)
    pub variants: Vec<RawDeck>,   // 흡수된 변형들 (대표 자신 포함)
    pub total_games: i64,         // 변형 표본 합산
    pub best_avg: f64,            // 가장 강한 변형의 평균등수 (대표값)
}

/// 두 덱의 공통 유닛 개수를 센다.
fn common_count(a: &[String], b: &[String]) -> usize {
    // a를 HashSet으로 만들어 b의 각 유닛이 있는지 빠르게 확인
    let set: HashSet<&String> = a.iter().collect();
    b.iter().filter(|u| set.contains(u)).count()
}

/// 원시 덱들을 대표덱으로 클러스터링.
/// raw는 avg_placement 오름차순(강한 것 먼저)으로 정렬돼 있다고 가정.
pub fn cluster_decks(raw: Vec<RawDeck>, min_common: usize) -> Vec<DeckCluster> {
    let mut clusters: Vec<DeckCluster> = Vec::new();

    for deck in raw {
        // 이미 만든 대표덱 중 "공통 min_common개 이상"인 곳을 찾는다
        let mut absorbed = false;
        for cluster in clusters.iter_mut() {
            if common_count(&cluster.core, &deck.units) >= min_common {
                // 흡수: 코어를 두 덱의 교집합으로 좁히고, 변형에 추가
                cluster.core.retain(|u| deck.units.contains(u));
                cluster.total_games += deck.games;
                cluster.variants.push(deck.clone());
                absorbed = true;
                break;
            }
        }
        // 어느 대표와도 안 맞으면 새 대표덱 생성
        if !absorbed {
            clusters.push(DeckCluster {
                core: deck.units.clone(),
                best_avg: deck.avg_placement,   // 가장 먼저 들어온 = 가장 강한 변형
                total_games: deck.games,
                variants: vec![deck],
            });
        }
    }

    clusters
}

/// 티어덱만 남긴다.
/// - avg 4.5 이하: 포함
/// - avg 4.5~5.0: 표본이 min_games_soft 이상일 때만 (순방덱)
/// - avg 5.0 초과: 제외
pub fn filter_tier_decks(
    clusters: Vec<DeckCluster>,
    min_games_soft: i64,
) -> Vec<DeckCluster> {
    clusters
        .into_iter()
        .filter(|c| {
            if c.best_avg <= 4.5 {
                true
            } else if c.best_avg <= 5.0 {
                c.total_games >= min_games_soft
            } else {
                false
            }
        })
        .collect()
}