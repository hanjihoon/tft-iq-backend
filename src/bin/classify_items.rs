//! 아이템 분류기 (일회성/패치마다 실행).
//!
//! Community Dragon items를 받아 각 완성 아이템의 composition(컴포넌트 구성)을 보고
//! 딜/탱/유틸로 분류해 item_classifications 테이블에 채운다.
//!
//! 분류 규칙: composition에 든 컴포넌트의 다수결.
//!   - 딜 컴포넌트가 1개라도 있으면 is_damage = true (캐리 게이트 통과 조건)
//!   - category는 딜/탱/유틸 중 더 많은 쪽 (동률이면 딜 우선)
//!
//! 실행:  cargo run --bin classify_items

use std::collections::{HashMap, HashSet};

use tft_iq::{Config, db};

const CDRAGON_KO: &str = "https://raw.communitydragon.org/latest/cdragon/tft/ko_kr.json";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Config 로드 실패: {e}");
        std::process::exit(1);
    });
    let pool = db::connect(&cfg.database_url).await?;

    // ── 컴포넌트 9개 분류 (기본 아이템) ──────────────────────
    // 이 id들은 세트가 바뀌어도 거의 안 변하는 고정 상수.
    let damage: HashSet<&str> = [
        "TFT_Item_BFSword",             // BF대검 (AD)
        "TFT_Item_RecurveBow",          // 곡궁 (공속)
        "TFT_Item_NeedlesslyLargeRod",  // 쇠막대 (AP)
        "TFT_Item_SparringGloves",      // 졸업장갑 (치명)
    ]
    .into_iter()
    .collect();

    let tank: HashSet<&str> = [
        "TFT_Item_ChainVest",      // 쇠사슬조끼 (방어)
        "TFT_Item_NegatronCloak",  // 음전자망토 (마저)
        "TFT_Item_GiantsBelt",     // 거인의허리띠 (체력)
    ]
    .into_iter()
    .collect();
    // 나머지(눈물, 스패출러)는 유틸로 자동 분류됨.

    // ── Community Dragon items 로드 ─────────────────────────
    eprintln!("Community Dragon 로드 중...");
    let v: serde_json::Value = reqwest::get(CDRAGON_KO).await?.json().await?;
    let items = v
        .get("items")
        .and_then(|x| x.as_array())
        .ok_or_else(|| anyhow::anyhow!("items 배열 없음"))?;

    // ── 분류 ────────────────────────────────────────────────
    let mut classified: HashMap<String, (String, String, bool)> = HashMap::new();

    for it in items {
        let (Some(api), Some(name)) = (
            it.get("apiName").and_then(|x| x.as_str()),
            it.get("name").and_then(|x| x.as_str()),
        ) else {
            continue;
        };

        // 오그먼트/컴포넌트/특수 아이템은 스킵 — composition 2개짜리 완성템만 분류
        let comp = it.get("composition").and_then(|x| x.as_array());
        let Some(comp) = comp else { continue };
        if comp.len() != 2 || name.is_empty() {
            continue;
        }

        let mut dmg = 0;
        let mut tnk = 0;
        let mut utl = 0;
        for c in comp {
            let Some(cid) = c.as_str() else { continue };
            if damage.contains(cid) {
                dmg += 1;
            } else if tank.contains(cid) {
                tnk += 1;
            } else {
                utl += 1;
            }
        }

        // category: 다수결 (동률 시 딜 > 탱 > 유틸 우선)
        let category = if dmg >= tnk && dmg >= utl {
            "damage"
        } else if tnk >= utl {
            "tank"
        } else {
            "utility"
        };
        let is_damage = dmg >= 1; // ★ 캐리 게이트: 딜 컴포넌트 1개라도 있으면 true

        classified.insert(
            api.to_string(),
            (name.to_string(), category.to_string(), is_damage),
        );
    }

    eprintln!("완성 아이템 {}종 분류됨", classified.len());

    // ── DB 저장 ─────────────────────────────────────────────
    let mut saved = 0;
    for (item_id, (name, category, is_damage)) in &classified {
        db::upsert_item_classification(&pool, item_id, name, category, *is_damage).await?;
        saved += 1;
    }
    eprintln!("item_classifications에 {saved}건 저장 완료");

    // 미리보기: 딜 아이템 몇 개 출력
    eprintln!("\n[딜 아이템 샘플]");
    for (id, (name, cat, dmg)) in classified.iter().take(40) {
        if *dmg {
            eprintln!("  {name}  ({cat})  <- {id}");
        }
    }

    Ok(())
}