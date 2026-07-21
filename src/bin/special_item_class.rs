use tft_iq::{Config, db};

const CDRAGON_KO: &str = "https://raw.communitydragon.org/latest/cdragon/tft/ko_kr.json";

/// 특수템 판정: 유물/상징이면 category 반환, 아니면 None
fn special_category(api: &str) -> Option<&'static str> {
    if api.contains("_Artifact_") {
        Some("artifact")
    } else if api.ends_with("EmblemItem") {
        Some("emblem")
    } else {
        None
    }
}

fn stat(effects: Option<&serde_json::Value>, key: &str) -> f64 {
    effects
        .and_then(|e| e.get(key))
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;

    eprintln!("Community Dragon 로드 중...");
    let v: serde_json::Value = reqwest::get(CDRAGON_KO).await?.json().await?;
    let items = v.get("items").and_then(|x| x.as_array())
        .ok_or_else(|| anyhow::anyhow!("items 배열 없음"))?;

    // 특수템만 추출
    struct Special {
        item_id: String, name: String, category: String, icon: String,
        ad: f64, ap: f64, as_bonus: f64, armor: f64, mr: f64, hp: f64,
    }
    let mut specials: Vec<Special> = Vec::new();

    for it in items {
        let (Some(api), Some(name)) = (
            it.get("apiName").and_then(|x| x.as_str()),
            it.get("name").and_then(|x| x.as_str()),
        ) else { continue };

        let Some(category) = special_category(api) else { continue };
        if name.is_empty() { continue; }

        let icon = it.get("icon").and_then(|x| x.as_str())
            .map(|ic| format!(
                "https://raw.communitydragon.org/latest/game/{}",
                ic.to_lowercase().replace(".tex", ".png")
            ))
            .unwrap_or_default();

        let e = it.get("effects");
        specials.push(Special {
            item_id: api.to_string(),
            name: name.to_string(),
            category: category.to_string(),
            icon,
            ad: stat(e, "AD"),
            ap: stat(e, "AP"),
            as_bonus: stat(e, "AS"),
            armor: stat(e, "Armor"),
            mr: stat(e, "MagicResist"),
            hp: stat(e, "Health"),
        });
    }

    eprintln!("특수템 {}종 (유물 {}, 상징 {})",
        specials.len(),
        specials.iter().filter(|s| s.category == "artifact").count(),
        specials.iter().filter(|s| s.category == "emblem").count(),
    );

    // 배치 upsert
    let mut tx = pool.begin().await?;
    for s in &specials {
        sqlx::query(
            r#"
            INSERT INTO special_items (item_id, name, category, icon_url, ad, ap, as_bonus, armor, mr, hp)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
            ON CONFLICT (item_id) DO UPDATE SET
                name=EXCLUDED.name, category=EXCLUDED.category, icon_url=EXCLUDED.icon_url,
                ad=EXCLUDED.ad, ap=EXCLUDED.ap, as_bonus=EXCLUDED.as_bonus,
                armor=EXCLUDED.armor, mr=EXCLUDED.mr, hp=EXCLUDED.hp
            "#,
        )
        .bind(&s.item_id).bind(&s.name).bind(&s.category).bind(&s.icon)
        .bind(s.ad).bind(s.ap).bind(s.as_bonus).bind(s.armor).bind(s.mr).bind(s.hp)
        .execute(&mut *tx).await?;
    }
    tx.commit().await?;

    eprintln!("저장 완료");
    pool.close().await;
    Ok(())
}