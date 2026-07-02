//! 퍼즐 도메인 타입.
//!
//! puzzles 테이블의 세 JSONB 컬럼이 정확히 이 구조로 직렬화된다:
//!   - prompt  → `Prompt`   (문제 상황: 보드/특성/유닛 + 질문)
//!   - options → `Vec<OptionItem>` (보기 목록)
//!   - stats   → `Stats`    (채점 후 피드백: 보기별 평균 등수)
//!
//! 설계 원칙: 정답은 "이 보기들 중 평균 등수(avg placement)가 가장 좋았던 것".
//! 승률(win rate)이 아니라 평균 등수를 쓰는 이유는 Riot 정책 + TFT 특성 때문.

use serde::{Deserialize, Serialize};

/// 퍼즐 종류. DB의 puzzle_type 컬럼 값과 1:1.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PuzzleKind {
    /// 오그먼트 선택: 보드를 보고 어떤 오그먼트가 가장 좋았는지
    AugmentPick,
    /// 보드 완성: 가려진 유닛을 추론
    BoardComplete,
    /// 아이템 조합: 완성 아이템의 컴포넌트 역산
    ItemCombine,
}

impl PuzzleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PuzzleKind::AugmentPick => "augment_pick",
            PuzzleKind::BoardComplete => "board_complete",
            PuzzleKind::ItemCombine => "item_combine",
        }
    }
}

// ───────────────────────── prompt ─────────────────────────

/// 문제 상황. 프론트가 이걸 보고 보드를 렌더링한다.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prompt {
    /// 한글 질문 문구
    pub question: String,
    pub context: BoardContext,
}

/// 최종 보드 스냅샷에서 추출한 맥락. (API가 주는 건 최종 상태뿐임을 기억)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardContext {
    pub level: i32,
    pub last_round: i32,
    /// 이 보드의 실제 등수. 난이도/맥락 참고용으로 노출.
    pub placement: i32,
    pub traits: Vec<TraitView>,
    pub units: Vec<UnitView>,
    /// 같이 골랐던 다른 오그먼트(있으면 맥락 제공). 정답 보기에선 제외.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prior_augments: Vec<NamedRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraitView {
    pub id: String,
    pub name: String, // 한글
    pub tier_current: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitView {
    pub id: String,
    pub name: String, // 한글
    pub cost: i32,
    pub star: i32, // 1~3
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<NamedRef>,
}

/// id + 한글 이름 묶음 (오그먼트/아이템 공용).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedRef {
    pub id: String,
    pub name: String,
}

// ───────────────────────── options ─────────────────────────

/// 보기 하나. id는 오그먼트 apiName, name은 한글.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionItem {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
}

// ───────────────────────── stats (feedback) ─────────────────────────

/// 채점 후 내려주는 피드백 데이터.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub options: Vec<OptionStat>,
    pub source_match_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionStat {
    pub id: String,
    /// ★ 핵심 지표: 이 오그먼트를 고른 판들의 평균 등수 (낮을수록 좋음).
    pub avg_placement: f64,
    /// 표본 수 (신뢰도 표시용)
    pub sample_size: i64,
    /// 이 보드의 실제 플레이어가 고른 보기인가
    pub was_actual_pick: bool,
}

// ───────────────────────── 아이템 BIS 퀴즈 ─────────────────────────

/// "이 캐리한테 뭘 줄까?" 퀴즈의 prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemPrompt {
    pub question: String,
    /// 캐리 유닛
    pub carry: NamedRef,
    /// 이 캐리가 자주 함께 쓰는 특성(맥락 제공, 선택적)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_traits: Vec<String>,
    pub patch: String,
}

/// 아이템 보기 + 채점용 통계.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemOptionStat {
    pub id: String,
    pub name: String,
    /// 이 캐리가 이 아이템을 들었을 때 평균 등수 (없으면 null)
    pub avg_placement: Option<f64>,
    pub sample_size: i64,
    /// 실제 BIS(정답)인가
    pub is_best: bool,
}