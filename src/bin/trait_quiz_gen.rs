use std::collections::HashSet;

use rand::seq::SliceRandom;
use tft_iq::{db, meta::Meta, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;

    let Some(info) = db::current_patch_info(&pool).await? else {
        eprintln!("패치 정보 없음.");
        return Ok(());
    };

    let use_pbe = std::env::args().any(|a| a == "--pbe" || a == "pbe");
    if use_pbe {
        eprintln!("🔮 PBE(프리섭) 데이터로 생성합니다");
    }

    let (set_number, patch) = (info.set_number, info.patch.clone());
    let meta = Meta::load_with_lang(set_number, "ko_kr", use_pbe).await?;

    // 전체 특성 한글명 (오답 풀)
    let all_traits: Vec<String> = meta.traits.keys().cloned().collect();
    if all_traits.len() < 6 {
        eprintln!("특성이 너무 적음 ({}) — 스킵", all_traits.len());
        return Ok(());
    }

    let mut rng = rand::thread_rng();
    let mut created = 0;

    for (uid, u) in &meta.units {
        if u.traits.is_empty() {
            continue;
        }
        if !uid.starts_with(&format!("TFT{}_", set_number))
            || uid.contains("Summon")
            || uid.contains("Minion")
        {
            continue;
        }

        let mut answer: Vec<String> = u.traits.clone();
        answer.sort();  // 정렬해서 저장

        // 오답 = 전체 특성 중 정답 아닌 것에서 랜덤 4개
        let pool_distract: Vec<&String> =
            all_traits.iter().filter(|t| !answer.contains(t)).collect();
        let distractors: Vec<String> = pool_distract
            .choose_multiple(&mut rng, 4)
            .map(|s| (*s).clone())
            .collect();

        // 보기 = 정답 + 오답 섞기
        let options: Vec<String> = answer.clone();
        let mut options: Vec<String> = normalize_traits(&options);
        options.extend(distractors);
        options.shuffle(&mut rng);

        let prompt = serde_json::json!({
            "question": format!("{}의 특성을 모두 고르세요", meta.unit_name(uid)),
            "unit": {
                "id": uid,
                "name": meta.unit_name(uid),
                "icon": unit_icon(uid, set_number),
            },
            "options": options,
            "answer": answer,
            "patch": patch,
        });

        let stats = serde_json::json!({ "trait_count": answer.len() });
        
        let mut answer_apis: Vec<String> = u.traits.clone();  // 이미 apiName
        answer_apis.sort();
        let trait_apis: Vec<String> = normalize_traits(&answer_apis);

        let answer = trait_apis.join(",");  // "TFT17_Sniper,TFT17_XXX"

        // ... prompt, stats 만들기 (기존) ...

        db::insert_trait_puzzle(
            &pool, "trait_quiz", &patch, set_number, uid,
            &normalize_trait(&answer),
            &prompt, &stats,
        )
        .await?;
        created += 1;
    }

    eprintln!("특성 퀴즈 {} 개 생성 (패치 {})", created, patch);
    Ok(())
}

fn unit_icon(id: &str, set: i32) -> String {
    let low = id.to_lowercase();
    let file_base: &str = match low.as_str() {
        "tft17_rhaast" => "tft17_kayn_slay",
        other => other,
    };
    format!(
        "https://raw.communitydragon.org/latest/game/assets/characters/{low}/hud/{file_base}_square.tft_set{set}.png"
    )
}

fn normalize_trait(api: &str) -> String {
    // 별돌보미 변종 통합: TFT17_Stargazer_XXX → TFT17_Stargazer
    if api.starts_with("TFT17_Stargazer_") {
        return "TFT17_Stargazer".to_string();
    }
    api.to_string()
}

fn normalize_traits(traits: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for t in traits {
        let normalized = normalize_trait(t);
        // 중복 아니면 추가 (순서 유지)
        if seen.insert(normalized.clone()) {
            result.push(normalized);
        }
    }
    result
}