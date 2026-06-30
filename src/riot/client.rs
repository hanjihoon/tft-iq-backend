//! Rate limit을 지키는 Riot API 클라이언트 (Personal Key 한도 기준).
//!
//! 엔드포인트마다 한도가 달라서 limiter를 분리한다. 단일 limiter면 모든 호출이
//! 가장 빡빡한 값(리그 30/10초)에 묶여 매치 수집(250/10초)이 느려지기 때문.
//!
//!   매치 상세  GET /tft/match/v1/matches/{id}            250 / 10초
//!   매치 ids   GET /tft/match/v1/matches/by-puuid/.../ids 600 / 10초
//!   리그       GET /tft/league/v1/{tier}                  30 / 10초  +  500 / 10분
//!
//! 각 한도에 안전 마진(약 90%)을 둬서 429를 피한다.

use crate::error::{AppError, Result};
use crate::riot::dto::{LeagueList, Match};
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

/// "n회 / period" 쿼터 헬퍼.
/// 한 칸을 period/n 마다 보충하고, 버스트(순간 최대)는 n으로 둔다.
fn quota(n: u32, period: Duration) -> Quota {
    Quota::with_period(period / n)
        .unwrap()
        .allow_burst(NonZeroU32::new(n).unwrap())
}

#[derive(Clone)]
pub struct RiotClient {
    http: reqwest::Client,
    api_key: String,
    platform: String, // 리그/소환사 라우팅 (kr ...)
    region: String,   // 매치 라우팅 (asia ...)

    // 엔드포인트별 limiter. Arc라 clone해도 한도는 공유된다.
    l_match_detail: Arc<Limiter>, // 250/10초
    l_match_ids: Arc<Limiter>,    // 600/10초
    l_league_short: Arc<Limiter>, // 30/10초
    l_league_long: Arc<Limiter>,  // 500/10분
}

impl RiotClient {
    pub fn new(api_key: String, platform: String, region: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;

        let ten_s = Duration::from_secs(10);
        let ten_min = Duration::from_secs(600);

        Ok(Self {
            http,
            api_key,
            platform,
            region,
            // 안전 마진 ~90%
            l_match_detail: Arc::new(RateLimiter::direct(quota(230, ten_s))),
            l_match_ids: Arc::new(RateLimiter::direct(quota(550, ten_s))),
            l_league_short: Arc::new(RateLimiter::direct(quota(28, ten_s))),
            l_league_long: Arc::new(RateLimiter::direct(quota(480, ten_min))),
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        limiters: &[&Limiter],
    ) -> Result<T> {
        loop {
            for l in limiters {
                l.until_ready().await;
            }

            let resp = self.http.get(url)
                .header("X-Riot-Token", &self.api_key)
                .send().await?;

            let status = resp.status();

            // 429 → Retry-After 만큼 대기 후 재시도
            if status.as_u16() == 429 {
                let wait = resp.headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(2);
                eprintln!("  429 — {}초 대기 후 재시도", wait);
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                continue;
            }

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(AppError::RiotStatus { status: status.as_u16(), body });
            }
            return Ok(resp.json::<T>().await?);
        }
    }

    fn platform_host(&self) -> String {
        format!("https://{}.api.riotgames.com", self.platform)
    }

    fn region_host(&self) -> String {
        format!("https://{}.api.riotgames.com", self.region)
    }

    /// 티어별 상위 리그. tier: "challenger" | "grandmaster" | "master"
    /// 리그는 10초·10분 두 한도가 동시 적용 → limiter 둘 다 통과해야 함.
    pub async fn league(&self, tier: &str) -> Result<LeagueList> {
        let url = format!(
            "{}/tft/league/v1/{}?queue=RANKED_TFT",
            self.platform_host(),
            tier.to_lowercase()
        );
        self.get_json(&url, &[&self.l_league_short, &self.l_league_long])
            .await
    }

    pub async fn challenger_league(&self) -> Result<LeagueList> {
        self.league("challenger").await
    }

    pub async fn grandmaster_league(&self) -> Result<LeagueList> {
        self.league("grandmaster").await
    }

    pub async fn master_league(&self) -> Result<LeagueList> {
        self.league("master").await
    }

    /// puuid의 최근 매치 id 목록 (count 최대 200, 한 호출 권장 ≤100).
    pub async fn match_ids(&self, puuid: &str, count: u32) -> Result<Vec<String>> {
        let url = format!(
            "{}/tft/match/v1/matches/by-puuid/{}/ids?count={}",
            self.region_host(),
            puuid,
            count
        );
        self.get_json(&url, &[&self.l_match_ids]).await
    }

    /// 매치 상세.
    pub async fn match_detail(&self, match_id: &str) -> Result<Match> {
        let url = format!("{}/tft/match/v1/matches/{}", self.region_host(), match_id);
        self.get_json(&url, &[&self.l_match_detail]).await
    }

    /// startTime(epoch seconds) 이후의 매치 id만. 지난 패치 매치를 원천 차단.
    pub async fn match_ids_since(
        &self,
        puuid: &str,
        count: u32,
        start_time: Option<i64>,
    ) -> Result<Vec<String>> {
        let mut url = format!(
            "{}/tft/match/v1/matches/by-puuid/{}/ids?count={}",
            self.region_host(),
            puuid,
            count
        );
        // start_time이 있으면 쿼리 파라미터 추가. None이면 전체 조회(첫 수집용).
        if let Some(t) = start_time {
            url.push_str(&format!("&startTime={t}"));
        }
        self.get_json(&url, &[&self.l_match_ids]).await
    }
}