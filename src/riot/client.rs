//! Rate limit을 지키는 Riot API 클라이언트.
//!
//! Riot 개발용 키 한도: 초당 20회 + 2분당 100회 (동시 적용).
//! governor로 두 한도를 모두 통과해야 요청을 보내도록 막는다.

use crate::error::{AppError, Result};
use crate::riot::dto::{LeagueList, Match};
use governor::{Quota, RateLimiter};
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

#[derive(Clone)]
pub struct RiotClient {
    http: reqwest::Client,
    api_key: String,
    platform: String, // 리그/소환사 라우팅 (kr ...)
    region: String,   // 매치 라우팅 (asia ...)
    // Arc로 감싸 여러 task가 같은 limiter를 공유 (Clone 해도 한도는 하나)
    per_second: Arc<Limiter>,
    per_two_min: Arc<Limiter>,
}

impl RiotClient {
    pub fn new(api_key: String, platform: String, region: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        // 안전 마진을 두고 살짝 낮게 설정 (18/s, 95/2min)
        let per_second = Arc::new(RateLimiter::direct(Quota::per_second(
            NonZeroU32::new(18).unwrap(),
        )));
        let per_two_min = Arc::new(RateLimiter::direct(
            Quota::with_period(Duration::from_secs(120))
                .unwrap()
                .allow_burst(NonZeroU32::new(95).unwrap()),
        ));

        Ok(Self {
            http,
            api_key,
            platform,
            region,
            per_second,
            per_two_min,
        })
    }

    /// 두 rate limiter를 모두 통과할 때까지 대기한 뒤 GET.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        self.per_second.until_ready().await;
        self.per_two_min.until_ready().await;

        let resp = self
            .http
            .get(url)
            .header("X-Riot-Token", &self.api_key)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::RiotStatus {
                status: status.as_u16(),
                body,
            });
        }
        Ok(resp.json::<T>().await?)
    }

    fn platform_host(&self) -> String {
        format!("https://{}.api.riotgames.com", self.platform)
    }

    fn region_host(&self) -> String {
        format!("https://{}.api.riotgames.com", self.region)
    }

    /// 챌린저 리그 전체 (entries 안에 puuid 들어있음).
    pub async fn challenger_league(&self) -> Result<LeagueList> {
        let url = format!(
            "{}/tft/league/v1/challenger?queue=RANKED_TFT",
            self.platform_host()
        );
        // 임시: 역직렬화 전에 raw JSON 찍기
        self.per_second.until_ready().await;
        self.per_two_min.until_ready().await;
        let resp = self.http.get(&url)
            .header("X-Riot-Token", &self.api_key)
            .send().await?;
        let text = resp.text().await?;
        eprintln!("CHALLENGER raw JSON (첫 500자): {}", &text[..text.len().min(500)]);
        let parsed = serde_json::from_str::<LeagueList>(&text)?;
        Ok(parsed)
    }

    /// 그랜드마스터 리그.
    pub async fn grandmaster_league(&self) -> Result<LeagueList> {
        let url = format!(
            "{}/tft/league/v1/grandmaster?queue=RANKED_TFT",
            self.platform_host()
        );
        self.get_json(&url).await
    }

    /// puuid의 최근 매치 id 목록.
    pub async fn match_ids(&self, puuid: &str, count: u32) -> Result<Vec<String>> {
        let url = format!(
            "{}/tft/match/v1/matches/by-puuid/{}/ids?count={}",
            self.region_host(),
            puuid,
            count
        );
        self.get_json(&url).await
    }

    /// 매치 상세.
    pub async fn match_detail(&self, match_id: &str) -> Result<Match> {
        let url = format!(
            "{}/tft/match/v1/matches/{}",
            self.region_host(),
            match_id
        );
        self.per_second.until_ready().await;
        self.per_two_min.until_ready().await;
        let resp = self.http.get(&url)
            .header("X-Riot-Token", &self.api_key)
            .send().await?;
        let text = resp.text().await?;
        eprintln!("매치 raw JSON (첫 500자): {}", &text[..text.len().min(500)]);
        let parsed = serde_json::from_str::<Match>(&text)
            .map_err(|e| {
                eprintln!("역직렬화 실패 상세: {e}");
                crate::error::AppError::Json(e)
            })?;
        Ok(parsed)
    }

    /// 티어별 상위 리그 조회. tier: "challenger" | "grandmaster" | "master"
    pub async fn league(&self, tier: &str) -> Result<LeagueList> {
        let url = format!(
            "{}/tft/league/v1/{}?queue=RANKED_TFT",
            self.platform_host(),
            tier.to_lowercase()
        );
        self.get_json(&url).await
    }
}