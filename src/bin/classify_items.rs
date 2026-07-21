//! 아이템 분류기 (일회성/패치마다 실행).
//!
//! Community Dragon items의 composition + effects를 보고 분류:
//!   1) category / is_damage  — 딜/탱/유틸 (캐리 게이트용)
//!   2) damage_type           — ad / ap / bruiser / tank / utility (오답 풀용)
//!
//! 분류 전략: "자동으로 될 건 자동, 모호한 건 수동"
//!   - 딜템 계열(ad/ap): effects의 AP vs AD×10으로 자동 판정
//!     (AD는 비율 0.3, AP는 정수 30이라 스케일을 ×10으로 맞춰 비교)
//!   - 브루저템: 종류 적고 메타 지식이 명확 → 명시적 목록 (BRUISER_ITEMS)
//!   - 그 외 비딜템: category(tank/utility) 그대로
//!   - 데이터로 안 잡히는 예외: OVERRIDES (구인수 등)
//!
//! 실행:  cargo run --bin classify_items

use std::collections::{HashMap, HashSet};

use tft_iq::{Config, db};

const CDRAGON_KO: &str = "https://raw.communitydragon.org/latest/cdragon/tft/ko_kr.json";

/// 브루저 전용 방어템 (방어 스탯 + 딜 기여를 같이 줘서 순수 탱커는 안 드는 것).
/// effects 스탯 키가 제각각(StackingAD 등)이라 자동 판정이 부정확 → 명시 목록.
const BRUISER_ITEMS: &[&str] = &[
    "TFT_Item_TitansResolve", // 거인의 결의
    "TFT_Item_SteraksGage",   // 스테락의 도전
    "TFT_Item_Quicksilver",   // 수은
    // 메타 보면서 추가 (정의의 손길 등)
];

/// effects/composition으로 안 잡히는 계열 예외.
const OVERRIDES: &[(&str, &str)] = &[
    ("TFT_Item_GuinsoosRageblade", "ad"), // 온힛 — 평딜 캐리가 주로 듦
    // 이상한 분류 발견되면 여기 추가
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Config 로드 실패: {e}");
        std::process::exit(1);
    });
    let pool = db::connect(&cfg.database_url).await?;

    const MIXED_ITEMS: &[&str] = &[
    "TFT_Item_GiantSlayer",   // 거인 학살자
    "TFT_Item_PowerGauntlet", // 타격대의 철퇴
    ];

    // ── 컴포넌트 분류 (딜/탱 게이트용) ──────────────────────
    let dmg_components: HashSet<&str> = [
        "TFT_Item_BFSword",            // AD
        "TFT_Item_NeedlesslyLargeRod", // AP
        "TFT_Item_RecurveBow",         // 공속
        "TFT_Item_SparringGloves",     // 치명
    ]
    .into_iter()
    .collect();
    let tank_components: HashSet<&str> = [
        "TFT_Item_ChainVest",
        "TFT_Item_NegatronCloak",
        "TFT_Item_GiantsBelt",
        "TFT_Item_HandOfJustice",  // 정의의 손길
        "TFT_Item_Bloodthirster",
        "TFT_Item_GuardianAngel",
    ]
    .into_iter()
    .collect();

    let bruiser_set: HashSet<&str> = BRUISER_ITEMS.iter().copied().collect();
    let mixed_set: HashSet<&str> = MIXED_ITEMS.iter().copied().collect();
    let override_map: HashMap<&str, &str> = OVERRIDES.iter().copied().collect();

    // ── Community Dragon 로드 ───────────────────────────────
    eprintln!("Community Dragon 로드 중...");
    let v: serde_json::Value = reqwest::get(CDRAGON_KO).await?.json().await?;
    let items = v
        .get("items")
        .and_then(|x| x.as_array())
        .ok_or_else(|| anyhow::anyhow!("items 배열 없음"))?;

    // ── 분류 ────────────────────────────────────────────────
    // (item_id, name, category, is_damage, damage_type)
    let mut classified: HashMap<String, (String, String, bool, String)> = HashMap::new();
    let mut icons: HashMap<String, String> = HashMap::new();

    for it in items {
        let (Some(api), Some(name)) = (
            it.get("apiName").and_then(|x| x.as_str()),
            it.get("name").and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        let icon_url = it.get("icon").and_then(|x| x.as_str())
            .map(|ic| format!(
                "https://raw.communitydragon.org/latest/game/{}",
                ic.to_lowercase().replace(".tex", ".png")
            ))
            .unwrap_or_default();
        let Some(comp) = it.get("composition").and_then(|x| x.as_array()) else {
            continue;
        };
        
        if comp.len() != 2 || name.is_empty() {
            continue;
        }

        // 컴포넌트 카운트 → category / is_damage
        let mut dmg = 0;
        let mut tnk = 0;
        let mut utl = 0;
        for c in comp {
            let Some(cid) = c.as_str() else { continue };
            if dmg_components.contains(cid) {
                dmg += 1;
            } else if tank_components.contains(cid) {
                tnk += 1;
            } else {
                utl += 1;
            }
        }
        let category = if dmg >= tnk && dmg >= utl {
            "damage"
        } else if tnk >= utl {
            "tank"
        } else {
            "utility"
        };
        let is_damage = dmg >= 1;

        // effects의 AD/AP 수치 (없으면 0)
        let effects = it.get("effects");
        let ad = effects
            .and_then(|e| e.get("AD"))
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        let ap = effects
            .and_then(|e| e.get("AP"))
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);

        // ── damage_type 5분류 ──
        // 우선순위: 브루저 목록 > 딜템 계열 > 순수방어/유틸
        let damage_type = if bruiser_set.contains(api) {
            "bruiser"
        } else if mixed_set.contains(api) {        // ← 추가
            "mixed"
        } else if is_damage {
            // AD는 비율, AP는 정수 → ×10으로 정규화 후 비교
            if ap > ad * 10.0 { "ap" } else { "ad" }
        } else if category == "tank" {
            "tank"
        } else {
            "utility"
        };
        // 예외 오버라이드 (섀도잉으로 덮어쓰기)
        let damage_type = override_map.get(api).copied().unwrap_or(damage_type);

        classified.insert(
            api.to_string(),
            (name.to_string(), category.to_string(), is_damage, damage_type.to_string()),
        );
        if !icon_url.is_empty() {
            icons.insert(api.to_string(), icon_url.clone());
        }
    }

    eprintln!("완성 아이템 {}종 분류됨", classified.len());

    // ── DB 저장 ─────────────────────────────────────────────
    for (item_id, (name, category, is_damage, damage_type)) in &classified {
        db::upsert_item_classification(&pool, item_id, name, category, *is_damage, damage_type)
            .await?;
        if let Some(url) = icons.get(item_id) {
            db::update_item_icon(&pool, item_id, url).await?;
        }
    }
    eprintln!("저장 완료\n");

    // ── 미리보기: 계열별 출력 (검수용) ──────────────────────
    for want in ["ad", "ap", "bruiser", "tank", "utility"] {
        let mut names: Vec<&str> = classified
            .values()
            .filter(|(_, _, _, dt)| dt == want)
            .map(|(name, _, _, _)| name.as_str())
            .collect();
        names.sort();
        eprintln!("[{want}] ({}종)", names.len());
        for n in names {
            eprintln!("  {n}");
        }
        eprintln!();
    }

    Ok(())
}