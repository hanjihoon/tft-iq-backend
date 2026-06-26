//! 컴프 분류 엔진.
//!
//! 한 참가자의 최종 보드(유닛 + 특성)를 "대표 컴프"를 식별하는 `CompKey`로 변환한다.
//! 같은 컴프는 항상 같은 키로 매핑돼야 집계(평균 등수)가 의미를 가진다.
//!
//! 분류 기준:
//!   1) 컴프 정체성 = 활성 특성 중 상위 2개 (유닛 1명짜리 고유 특성은 노이즈라 제외)
//!   2) 캐리 = 아이템을 가장 많이 든 유닛 (동률이면 코스트 높은 쪽)

use crate::riot::dto::{Participant, Trait, Unit};
use std::fmt;

/// rarity(0~6) → 코스트(1~5) 매핑.
/// TFT는 rarity가 코스트와 선형이 아니다 (5코스트가 rarity 6).
pub fn cost_from_rarity(rarity: i32) -> i32 {
    // `match`는 모든 경우를 강제로 다루게 하는 표현식이다.
    // 마지막 `other =>`가 없으면 컴파일 에러 — Rust가 누락을 막아준다.
    match rarity {
        0 => 1,
        1 => 2,
        2 => 3,
        4 => 4,
        6 => 5,
        other => other + 1, // 예상 밖 값에 대한 안전한 기본값
    }
}

/// "TFT17_DarkStar" → "DarkStar" 처럼 세트 접두사를 떼어 사람이 읽기 좋게.
pub fn prettify(id: &str) -> String {
    // split_once는 첫 '_'에서 한 번만 쪼개 (앞, 뒤) 튜플을 Option으로 돌려준다.
    // '_'가 없으면 None → 원본을 그대로 쓴다.
    id.split_once('_')
        .map(|(_prefix, rest)| rest)
        .unwrap_or(id)
        .to_string()
}

/// 컴프를 식별하는 키.
///
/// `Hash + Eq + PartialEq`를 derive해야 HashMap의 키로 쓸 수 있다.
/// 이 셋의 계약: "같다고 판정되는(Eq) 두 값은 반드시 같은 해시(Hash)를 내야 한다."
/// derive를 쓰면 모든 필드를 자동으로 비교/해싱하므로 이 계약이 저절로 지켜진다.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CompKey {
    /// 정규화된(정렬된) 상위 특성들. 정렬해두면 순서가 달라도 같은 컴프로 취급된다.
    pub traits: Vec<String>,
    /// 캐리 유닛의 character_id. 캐리가 없으면 None.
    pub carry: Option<String>,
}

/// `Display`를 구현하면 `{}`로 출력할 수 있고, `.to_string()`도 공짜로 따라온다.
/// (`Debug`는 `{:?}`용 개발자 표기, `Display`는 사람이 읽는 표기 — 둘은 별개다.)
impl fmt::Display for CompKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let traits = if self.traits.is_empty() {
            "(특성없음)".to_string()
        } else {
            self.traits.join(" + ")
        };
        match &self.carry {
            Some(c) => write!(f, "{traits} | 캐리: {}", prettify(c)),
            None => write!(f, "{traits}"),
        }
    }
}

/// 참가자 보드 → CompKey.
///
/// 함수가 `&Participant`(빌림)를 받는 데 주목. 소유권을 가져오지 않으므로
/// 호출자는 여전히 자기 데이터를 자유롭게 쓸 수 있다. 분류는 "읽기"만 하면 되니까.
pub fn classify(p: &Participant) -> CompKey {
    CompKey {
        traits: identity_traits(p),
        // TEST
        carry: find_carry(p),
    }
}

/// 컴프 정체성이 되는 상위 특성 추출.
fn identity_traits(p: &Participant) -> Vec<String> {
    // iter()는 &Trait를 흘려주고, filter로 후보만 남긴 뒤 collect로 Vec<&Trait>를 만든다.
    // 여기서 모은 건 참조라서 복사 비용이 없다 (원본을 빌려 가리킬 뿐).
    let mut candidates: Vec<&Trait> = p
        .traits
        .iter()
        .filter(|t| t.tier_current > 0 && t.tier_total >= 2)
        // tier_total >= 2 : 유닛 1명짜리 고유 특성(BlitzcrankUniqueTrait 등) 제외
        .collect();

    // 다중 키 정렬: tier_current 내림차순 → num_units 내림차순 → 이름 오름차순.
    // cmp는 Ordering을 돌려주고, then_with는 "앞이 동률일 때만" 다음 기준을 본다.
    // 정수라서 cmp(전순서, Ord)를 쓸 수 있다. (실수 f64라면 Ord가 없어 cmp 못 씀)
    candidates.sort_by(|a, b| {
        b.tier_current
            .cmp(&a.tier_current)
            .then_with(|| b.num_units.cmp(&a.num_units))
            .then_with(|| a.name.cmp(&b.name))
    });

    // 상위 2개만 취해 이름만 뽑는다.
    let mut traits: Vec<String> = candidates
        .iter()
        // TEST 2가 표준
        .take(2)
        .map(|t| prettify(&t.name))
        .collect();

    // ★ 정규화: 정렬해두면 ["Mecha","DarkStar"]와 ["DarkStar","Mecha"]가
    //   같은 Vec가 되어 같은 컴프로 집계된다.
    traits.sort();
    traits
}

/// 캐리(아이템 최다 유닛) 식별.
fn find_carry(p: &Participant) -> Option<String> {
    let mut carriers: Vec<&Unit> = p
        .units
        .iter()
        .filter(|u| u.item_names.len() >= 2) // 아이템 2개 이상 든 유닛만 후보
        .collect();

    carriers.sort_by(|a, b| {
        b.item_names
            .len()
            .cmp(&a.item_names.len()) // 아이템 많은 순
            .then_with(|| cost_from_rarity(b.rarity).cmp(&cost_from_rarity(a.rarity))) // 코스트 높은 순
            .then_with(|| a.character_id.cmp(&b.character_id)) // 동률 시 결정론적 순서
    });

    // first()는 Option<&&Unit>. map으로 character_id를 복제해 Option<String>으로.
    carriers.first().map(|u| u.character_id.clone())
}

// ───────────────────────── 단위 테스트 ─────────────────────────
// `#[cfg(test)]`: 이 모듈은 `cargo test`로 빌드할 때만 컴파일된다 (운영 바이너리엔 안 들어감).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::riot::dto::{Trait, Unit};

    fn trait_(name: &str, tier_current: i32, tier_total: i32, num_units: i32) -> Trait {
        Trait {
            name: name.to_string(),
            num_units,
            style: 0,
            tier_current,
            tier_total,
        }
    }

    fn unit_(id: &str, rarity: i32, items: &[&str]) -> Unit {
        Unit {
            character_id: id.to_string(),
            item_names: items.iter().map(|s| s.to_string()).collect(),
            rarity,
            tier: 2,
        }
    }

    fn participant(traits: Vec<Trait>, units: Vec<Unit>, placement: i32) -> Participant {
        Participant {
            puuid: "x".into(),
            placement,
            level: 9,
            last_round: 33,
            gold_left: 0,
            players_eliminated: 0,
            time_eliminated: 0.0,
            total_damage_to_players: 0,
            augments: vec![],
            traits,
            units,
        }
    }

    #[test]
    fn 고유특성은_컴프식별에서_제외된다() {
        let p = participant(
            vec![
                trait_("TFT17_DarkStar", 2, 4, 6),
                trait_("TFT17_FioraUniqueTrait", 1, 1, 1), // 제외 대상
            ],
            vec![],
            1,
        );
        let key = classify(&p);
        assert_eq!(key.traits, vec!["DarkStar".to_string()]);
    }

    #[test]
    fn 특성순서가_달라도_같은_키가_된다() {
        let a = participant(
            vec![trait_("TFT17_Mecha", 2, 3, 4), trait_("TFT17_DarkStar", 2, 3, 4)],
            vec![],
            1,
        );
        let b = participant(
            vec![trait_("TFT17_DarkStar", 2, 3, 4), trait_("TFT17_Mecha", 2, 3, 4)],
            vec![],
            1,
        );
        // 정규화(정렬) 덕분에 입력 순서가 달라도 동치
        assert_eq!(classify(&a), classify(&b));
    }

    #[test]
    fn 아이템_최다_유닛이_캐리로_뽑힌다() {
        let p = participant(
            vec![],
            vec![
                unit_("TFT17_Tank", 4, &["a", "b"]),
                unit_("TFT17_Jhin", 2, &["a", "b", "c"]), // 아이템 3개 → 캐리
            ],
            1,
        );
        assert_eq!(classify(&p).carry, Some("TFT17_Jhin".to_string()));
    }

    #[test]
    fn 아이템_없으면_캐리는_None() {
        let p = participant(vec![], vec![unit_("TFT17_X", 0, &[])], 8);
        assert_eq!(classify(&p).carry, None);
    }
}