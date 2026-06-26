//! 메타데이터 로더 (오그먼트/유닛/아이템 → 한글 이름).
//!
//! Data Dragon은 최신 세트 TFT 데이터가 종종 누락돼서, 커뮤니티 표준인
//! Community Dragon의 ko_kr 통합 파일을 쓴다. 오답(distractor) 풀과
//! id→한글 매핑 양쪽에 사용.

use crate::error::Result;
use std::collections::HashMap;

const CDRAGON_KO: &str = "https://raw.communitydragon.org/latest/cdragon/tft/ko_kr.json";

pub struct UnitMeta {
    pub name: String,
    pub cost: i32,
}

pub struct Meta {
    /// apiName → 한글 (예: "TFT17_Augment_..." → "...")
    pub augments: HashMap<String, String>,
    pub units: HashMap<String, UnitMeta>,
    pub items: HashMap<String, String>,
}

impl Meta {
    /// 특정 세트의 메타데이터를 로드.
    pub async fn load(set_number: i32) -> Result<Self> {
        // 메타 로드는 1회성이라 기본 클라이언트로 충분
        let v: serde_json::Value = reqwest::get(CDRAGON_KO).await?.json().await?;

        let mut augments = HashMap::new();
        let mut items = HashMap::new();

        // 최상위 "items"에 일반 아이템과 오그먼트가 섞여 있다.
        if let Some(arr) = v.get("items").and_then(|x| x.as_array()) {
            for it in arr {
                let (Some(id), Some(name)) = (
                    it.get("apiName").and_then(|x| x.as_str()),
                    it.get("name").and_then(|x| x.as_str()),
                ) else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                if id.contains("_Augment_") {
                    augments.insert(id.to_string(), name.to_string());
                } else {
                    items.insert(id.to_string(), name.to_string());
                }
            }
        }

        // 세트별 챔피언은 "setData" 배열에서 number로 찾는다.
        let mut units = HashMap::new();
        if let Some(sets) = v.get("setData").and_then(|x| x.as_array()) {
            for s in sets {
                if s.get("number").and_then(|n| n.as_i64()) != Some(set_number as i64) {
                    continue;
                }
                if let Some(champs) = s.get("champions").and_then(|c| c.as_array()) {
                    for c in champs {
                        let (Some(id), Some(name)) = (
                            c.get("apiName").and_then(|x| x.as_str()),
                            c.get("name").and_then(|x| x.as_str()),
                        ) else {
                            continue;
                        };
                        let cost = c.get("cost").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                        units.insert(
                            id.to_string(),
                            UnitMeta {
                                name: name.to_string(),
                                cost,
                            },
                        );
                    }
                }
            }
        }

        Ok(Self {
            augments,
            units,
            items,
        })
    }

    pub fn augment_name(&self, id: &str) -> String {
        self.augments.get(id).cloned().unwrap_or_else(|| id.to_string())
    }

    pub fn unit_name(&self, id: &str) -> String {
        self.units
            .get(id)
            .map(|u| u.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    pub fn unit_cost(&self, id: &str, fallback: i32) -> i32 {
        self.units.get(id).map(|u| u.cost).unwrap_or(fallback)
    }

    pub fn item_name(&self, id: &str) -> String {
        self.items.get(id).cloned().unwrap_or_else(|| id.to_string())
    }
}