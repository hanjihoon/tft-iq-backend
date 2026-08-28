use std::collections::HashMap;
use tft_iq::{Config, db};

const CDRAGON_KO: &str = "https://raw.communitydragon.org/latest/cdragon/tft/ko_kr.json";

/// 조합 완성템이 아닌 것. Set 18은 cdragon composition이 비어 있어
/// id 규칙으로 판별한다 — 완성템만 표식 접두사가 없다.
fn is_special_item(api: &str) -> bool {
    api.contains("_Component_")      // 기본템
        || api.contains("_Artifact_")    // 유물
        || api.contains("Emblem")        // 상징
        || api.contains("Radiant")       // 찬란
        || api.contains("Tacticians")    // 전략가의 왕관/망토/방패
        || api.contains("Potion")        // 소모품 물약
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Config 로드 실패: {e}");
        std::process::exit(1);
    });
    let pool = db::connect(&cfg.database_url).await?;

    eprintln!("Community Dragon 로드 중...");
    let v: serde_json::Value = reqwest::get(CDRAGON_KO).await?.json().await?;
    let items = v
        .get("items")
        .and_then(|x| x.as_array())
        .ok_or_else(|| anyhow::anyhow!("items 배열 없음"))?;

    struct ItemRow {
        item_id: String,
        name: String,
        icon: String,
    }
    let mut rows: Vec<ItemRow> = Vec::new();
    let mut skipped = 0;

    for it in items {
        let (Some(api), Some(name)) = (
            it.get("apiName").and_then(|x| x.as_str()),
            it.get("name").and_then(|x| x.as_str()),
        ) else {
            continue;
        };

        if name.is_empty() {
            continue;
        }
        // 증강은 아이템이 아니다
        if it.get("isAugment").and_then(|x| x.as_bool()).unwrap_or(false) {
            continue;
        }

        // Set 18 조합 완성템은 DA_ 뒤에 바로 이름이 온다: DA_InfinityEdge
        // DA_18_ 접두사는 증강(DA_18_AfterShock)과 상징(DA_18_EmblemFae) 계열이다.
        if !api.starts_with("DA_") || api.starts_with("DA_18_") {
            continue;
        }
        if is_special_item(api) {
            skipped += 1;
            continue;
        }

        let icon = it
            .get("icon")
            .and_then(|x| x.as_str())
            .map(|ic| {
                format!(
                    "https://raw.communitydragon.org/latest/game/{}",
                    ic.to_lowercase().replace(".tex", ".png")
                )
            })
            .unwrap_or_default();

        rows.push(ItemRow {
            item_id: api.to_string(),
            name: name.to_string(),
            icon,
        });
    }

    eprintln!("조합 완성템 {}종 (특수템 {skipped}종 제외)", rows.len());

    // 분류(category/damage_type)는 cdragon effects가 비어 있어 판정 불가.
    // 미분류로 저장하고, cdragon이 채워지면 이 bin을 다시 실행한다.
    let mut tx = pool.begin().await?;
    for r in &rows {
        sqlx::query(
            r#"
            INSERT INTO item_classifications (item_id, name, category, is_damage, damage_type, icon_url)
            VALUES ($1, $2, 'unknown', false, 'unknown', $3)
            ON CONFLICT (item_id) DO UPDATE SET
                name = EXCLUDED.name,
                icon_url = EXCLUDED.icon_url
            "#,
        )
        .bind(&r.item_id)
        .bind(&r.name)
        .bind(&r.icon)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    eprintln!("저장 완료");

    // 검수용 출력
    for r in rows.iter().take(50) {
        eprintln!("  {} — {}", r.item_id, r.name);
    }

    pool.close().await;
    Ok(())
}