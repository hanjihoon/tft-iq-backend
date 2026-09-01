// src/bin/meta_sync.rs
use tft_iq::{db, meta::Meta, Config};

const LANGS: &[&str] = &[
    "ko_kr", "en_us", "ja_jp", "zh_cn", "pt_br",
    "es_mx", "fr_fr", "de_de", "ru_ru", "vi_vn", "th_th",
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;

    // 서빙 패치와 무관하게 최신 세트를 받아둔다.
    // (세트 전환기엔 서빙이 옛 세트여도 새 세트 메타가 필요하다)
    let set_number = 18;

    for lang in LANGS {
        let meta = match Meta::load_with_lang(set_number, lang, false).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("{lang} 로드 실패: {e}");
                continue;
            }
        };
        eprintln!("{lang}: 유닛 {}종, 특성 {}종", meta.units.len(), meta.trait_details.len());

        db::upsert_unit_meta(&pool, set_number, lang, &meta).await?;
        db::upsert_trait_meta(&pool, set_number, lang, &meta).await?;
    }

    pool.close().await;
    Ok(())
}