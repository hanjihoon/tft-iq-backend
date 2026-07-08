//! 메타데이터 로더 (오그먼트/유닛/아이템 → 한글 이름).
//!
//! Data Dragon은 최신 세트 TFT 데이터가 종종 누락돼서, 커뮤니티 표준인
//! Community Dragon의 ko_kr 통합 파일을 쓴다. 오답(distractor) 풀과
//! id→한글 매핑 양쪽에 사용.

use crate::error::Result;
use std::collections::HashMap;


pub struct Meta {
    pub augments: HashMap<String, String>,
    pub units: HashMap<String, UnitMeta>,
    pub items: HashMap<String, String>,
    pub traits: HashMap<String, String>,   // apiName → 한글 (예: TFT17_Divine → 신성)
    pub trait_details: HashMap<String, TraitMeta>,  // 추가 (icon, breakpoints)
}

pub struct UnitMeta {
    pub name: String,
    pub cost: i32,
    pub traits: Vec<String>,
    pub ability: Option<SkillMeta>,
}

#[derive(Clone, serde::Serialize)]
pub struct SkillMeta {
    pub name: String,
    pub icon: String,        // 변환된 URL
    pub desc: String,
    pub variables: serde_json::Value,  // [{name, value:[...]}] 그대로
}

#[derive(Clone, serde::Serialize)]
pub struct TraitMeta {
    pub name: String,
    pub icon: String,
    pub breakpoints: Vec<(i32, i32)>,  // (minUnits, style)
}

impl Meta {
    /// 특정 세트의 메타데이터를 로드.
    pub async fn load(set_number: i32, use_pbe: bool) -> Result<Self> {
        // 메타 로드는 1회성이라 기본 클라이언트로 충분
        let url = if use_pbe {
            "https://raw.communitydragon.org/pbe/cdragon/tft/ko_kr.json"
        } else {
            "https://raw.communitydragon.org/latest/cdragon/tft/ko_kr.json"
        };

        let v: serde_json::Value = reqwest::get(url).await?.json().await?;

        let mut augments = HashMap::new();
        let mut items = HashMap::new();

        fn skill_icon_url(raw: &str) -> String {
            if raw.is_empty() {
                return String::new();
            }
            // 1. 소문자화
            let low = raw.to_lowercase();
            // 2. .tex → .png
            let png = low.replace(".tex", ".png");
            // 3. cdragon URL (ASSETS/ 는 경로에 그대로, game/ 아래로)
            format!("https://raw.communitydragon.org/latest/game/{}", png)
        }

        
        fn trait_icon_url(raw: &str) -> String {
            if raw.is_empty() { return String::new(); }
            let low = raw.to_lowercase();
            let png = low.replace(".tex", ".png");
            format!("https://raw.communitydragon.org/latest/game/{}", png)
        }


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
        // 세트별 챔피언 + 특성은 "setData" 배열에서 number로 찾는다.
        let mut units = HashMap::new();
        let mut traits = HashMap::new();
        let mut trait_details = HashMap::new();
        if let Some(sets) = v.get("setData").and_then(|x| x.as_array()) {
            for s in sets {
                if s.get("number").and_then(|n| n.as_i64()) != Some(set_number as i64) {
                    continue;
                }
                // 이 세트의 특성 한글명
                if let Some(tr) = s.get("traits").and_then(|x| x.as_array()) {
                    for t in tr {
                        let (Some(id), Some(name)) = (
                            t.get("apiName").and_then(|x| x.as_str()),
                            t.get("name").and_then(|x| x.as_str()),
                        ) else {
                            continue;
                        };
                        if !name.is_empty() {
                            traits.insert(id.to_string(), name.to_string());
                        }

                        // 추가: trait_details (icon, breakpoints)
                        let icon = trait_icon_url(t.get("icon").and_then(|x| x.as_str()).unwrap_or(""));
                        let breakpoints: Vec<(i32, i32)> = t.get("effects")
                            .and_then(|e| e.as_array())
                            .map(|arr| arr.iter().filter_map(|e| {
                                let min = e.get("minUnits")?.as_i64()? as i32;
                                let style = e.get("style")?.as_i64()? as i32;
                                Some((min, style))
                            }).collect())
                            .unwrap_or_default();

                        trait_details.insert(name.to_string(), TraitMeta {  // 한글명 키!
                            name: name.to_string(), icon, breakpoints,
                        });
                    }
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
                        // 유닛의 특성 배열 (예: ["우주 그루브", "저격수"])
                        let traits: Vec<String> = c
                            .get("traits")
                            .and_then(|t| t.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();

                        // 스킬(ability) 파싱
                        let ability = c.get("ability").and_then(|ab| {
                            let name = ab.get("name").and_then(|x| x.as_str())?.to_string();
                            let desc = ab.get("desc").and_then(|x| x.as_str()).unwrap_or("").to_string();
                            let icon_raw = ab.get("icon").and_then(|x| x.as_str()).unwrap_or("");
                            let icon = skill_icon_url(icon_raw);
                            let variables = ab.get("variables").cloned().unwrap_or(serde_json::json!([]));
                            Some(SkillMeta { name, icon, desc, variables })
                        });

                        units.insert(
                            id.to_string(),
                            UnitMeta { name: name.to_string(), cost, traits, ability },
                        );
                    }
                }
            }
        }


        Ok(Self {
            augments,
            units,
            items,
            traits,
            trait_details,
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

    pub fn trait_name(&self, id: &str) -> String {
        self.traits.get(id).cloned().unwrap_or_else(|| id.to_string())
    }
}