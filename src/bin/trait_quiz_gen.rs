use rand::seq::SliceRandom;
use tft_iq::{db, Config};

const LANG: &str = "ko_kr";  // 특성 id·아이콘은 언어 무관, 이름만 표시용

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;

    let Some(info) = db::current_patch_info(&pool).await? else {
        eprintln!("패치 정보 없음.");
        return Ok(());
    };
    let (set_number, patch) = (info.set_number, info.patch.clone());

    let units = db::units_for_trait_quiz(&pool, set_number, LANG).await?;
    let all_traits = db::all_trait_ids(&pool, set_number, LANG).await?;

    eprintln!("유닛 {}종, 특성 {}종", units.len(), all_traits.len());
    if all_traits.len() < 5 {
        eprintln!("특성이 너무 적음 — 스킵");
        return Ok(());
    }

    let mut rng = rand::thread_rng();
    let mut created = 0;

    for u in &units {
        // 정답 특성 (중복 제거 + 정렬)
        let mut answer: Vec<String> = u.traits.clone();
        answer.sort();
        answer.dedup();
        if answer.is_empty() {
            continue;
        }

        // 오답 = 정답 아닌 특성 중 랜덤 4개
        let distractors: Vec<String> = all_traits.iter()
            .filter(|t| !answer.contains(t))
            .cloned()
            .collect::<Vec<_>>()
            .choose_multiple(&mut rng, 4)
            .cloned()
            .collect();

        if distractors.len() < 4 {
            continue;   // 오답 못 채우면 스킵
        }

        // 보기 = 정답 + 오답 섞기
        let mut options: Vec<String> = answer.clone();
        options.extend(distractors);
        options.shuffle(&mut rng);

        let prompt = serde_json::json!({
            "question": format!("{}의 특성을 모두 고르세요", u.name),
            "unit": { "id": u.unit_id, "name": u.name, "icon": u.icon },
            "options": options,
            "answer": answer,
            "patch": patch,
        });
        let stats = serde_json::json!({ "trait_count": answer.len() });

        db::insert_trait_puzzle(
            &pool, "trait_quiz", &patch, set_number, &u.unit_id,
            &answer.join(","),
            &prompt, &stats,
        ).await?;
        created += 1;
    }

    eprintln!("특성 퀴즈 {created}개 생성 (패치 {patch})");
    pool.close().await;
    Ok(())
}