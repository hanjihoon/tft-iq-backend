//! Riot TFT API 응답을 매핑하는 타입들.
//!
//! ⚠️ 중요: 이 파일이 "API로 실제 얻을 수 있는 데이터의 전부"다.
//! 여기 없는 필드(오그먼트 후보 3개, 라운드별 스냅샷, 상대 보드 등)는
//! API가 애초에 주지 않는다. 타임라인 엔드포인트는 TFT엔 존재하지 않는다.

use serde::{Deserialize, Serialize};

// ───────────────────────── TFT-LEAGUE-V1 ─────────────────────────

/// GET /tft/league/v1/challenger  (grandmaster, master 동일 구조)
#[derive(Debug, Deserialize)]
pub struct LeagueList {
    pub tier: String,           // "CHALLENGER"
    #[serde(default)]
    pub name: String,
    pub entries: Vec<LeagueItem>,
}

#[derive(Debug, Deserialize)]
pub struct LeagueItem {
    /// 최신 API는 puuid를 직접 제공. 구버전 호환을 위해 summoner_id도 옵션.
    #[serde(default)]
    pub puuid: Option<String>,
    #[serde(default, rename = "summonerId")]
    pub summoner_id: Option<String>,
    #[serde(rename = "leaguePoints")]
    pub league_points: i32,
    pub rank: String, // "I" (챌린저는 항상 I)
    pub wins: i32,
    pub losses: i32,
}

// ───────────────────────── TFT-MATCH-V1 ─────────────────────────

/// GET /tft/match/v1/matches/{matchId}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Match {
    pub metadata: Metadata,
    pub info: Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    #[serde(rename = "match_id")]
    pub match_id: String,
    /// 이 매치 참가자 8명의 puuid 목록
    pub participants: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    /// epoch milliseconds
    #[serde(rename = "game_datetime")]
    pub game_datetime: i64,
    /// 예: "Version 14.1.x" — 여기서 패치를 파싱한다
    #[serde(rename = "game_version")]
    pub game_version: String,
    /// 세트 번호 (Space Gods = 17). 세트가 바뀌면 퍼즐을 분리해야 함.
    #[serde(rename = "tft_set_number")]
    pub tft_set_number: i32,
    #[serde(rename = "tft_game_type", default)]
    pub tft_game_type: String, // "standard", "turbo"(더블업) 등
    #[serde(rename = "queue_id", default)]
    pub queue_id: i32, // 1100 = 랭크 단식
    pub participants: Vec<Participant>,
}

/// 한 플레이어의 "최종 결과 스냅샷". 게임 도중 상태는 들어있지 않다.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub puuid: String,
    pub placement: i32,
    pub level: i32,
    #[serde(default, rename = "last_round")]
    pub last_round: i32,
    #[serde(default, rename = "gold_left")]
    pub gold_left: i32,
    #[serde(default, rename = "players_eliminated")]
    pub players_eliminated: i32,
    #[serde(default, rename = "time_eliminated")]
    pub time_eliminated: f64,
    #[serde(default, rename = "total_damage_to_players")]
    pub total_damage_to_players: i32,
    #[serde(default)]
    pub augments: Vec<String>,
    #[serde(default)]
    pub traits: Vec<Trait>,
    #[serde(default)]
    pub units: Vec<Unit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trait {
    pub name: String,            // "TFT17_SpaceGod" 같은 내부 id
    #[serde(rename = "num_units")]
    pub num_units: i32,
    /// 활성 등급 (예: 브론즈/실버/골드 → 1/2/3). 0이면 미활성.
    pub style: i32,
    #[serde(rename = "tier_current")]
    pub tier_current: i32,
    #[serde(rename = "tier_total")]
    pub tier_total: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unit {
    #[serde(rename = "character_id")]
    pub character_id: String,    // "TFT17_Aphelios"
    /// 장착 아이템의 문자열 id. (items 정수배열은 사실상 deprecated)
    #[serde(rename = "itemNames", default)]
    pub item_names: Vec<String>,
    pub rarity: i32,             // 0~6 (코스트와 연관)
    pub tier: i32,               // 1~3 (별 개수)
}

impl Info {
    /// "Version 17.5.123" → "17.5" 형태로 패치 추출.
    pub fn patch(&self) -> String {
        // "<Releases/16.12>" → "16.12"
        // "Version 17.5.123" → "17.5"
        let v = &self.game_version;
        
        // 숫자.숫자 패턴을 찾아서 추출
        let digits: Vec<&str> = v
            .split(|c: char| !c.is_ascii_digit() && c != '.')
            .filter(|s| s.contains('.') && !s.is_empty())
            .collect();
        
        if let Some(ver) = digits.first() {
            let parts: Vec<&str> = ver.split('.').collect();
            if parts.len() >= 2 {
                return format!("{}.{}", parts[0], parts[1]);
            }
        }
        v.clone()
    }
}