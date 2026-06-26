# TFT IQ — Backend

상위권(Challenger/Grandmaster) 매치 데이터를 수집해 의사결정 퍼즐을 만드는 백엔드.

## 구조

```
src/
├── lib.rs            공유 모듈 진입점
├── config.rs         환경변수 설정
├── error.rs          전역 에러 타입
├── puzzle/mod.rs     ★ 퍼즐 도메인 타입 (prompt/options/stats 스키마)
├── meta/mod.rs       Community Dragon 메타데이터 (한글 이름, 오답 풀)
├── riot/
│   ├── dto.rs        ★ Riot API 데이터 계약서
│   └── client.rs     rate-limit 지키는 API 클라이언트
├── db/mod.rs         DB 풀 + 리포지토리 함수
└── bin/
    ├── crawler.rs    [1] 데이터 수집
    ├── generator.rs  [2] 퍼즐 생성 (raw_matches → puzzles)
    └── server.rs     [3] HTTP API
migrations/
└── 0001_init.sql     DB 스키마
```

## 데이터 파이프라인

```
Riot API ──► crawler ──► raw_matches ──► generator ──► puzzles ──► server ──► client
                                            ▲
                                  Community Dragon (한글/오답 풀)
```

`raw_matches`에 매치를 통째로 보관하므로, 퍼즐 생성 로직을 바꿔도 재수집 없이
과거 데이터를 재가공할 수 있다.

## 실행 순서

```bash
# 0) 설정
cp .env.example .env          # RIOT_API_KEY, DATABASE_URL 채우기

# 1) DB
createdb tft_iq
psql "$DATABASE_URL" -f migrations/0001_init.sql

# 2) 수집 (cron 권장: 6시간마다)
cargo run --bin crawler

# 3) 퍼즐 생성 (수집 후 주기 실행)
cargo run --bin generator

# 4) API 서버
cargo run --bin server
```

## API

| 메서드 | 경로 | 설명 |
|---|---|---|
| GET | `/health` | 헬스체크 |
| GET | `/api/puzzles/daily` | 오늘의 퍼즐 (정답 숨김) |
| POST | `/api/puzzles/{id}/attempt` | 답안 제출 → 채점 + 통계 |
| GET | `/api/me/{puuid}/weakness` | 퍼즐 타입별 정답률 (약점 분석) |

## 퍼즐 스키마 (puzzles 테이블)

- `prompt`  : 문제 상황 (보드/특성/유닛 + 질문) — `puzzle::Prompt`
- `options` : 보기 목록 — `Vec<puzzle::OptionItem>`
- `answer`  : 정답 보기 id (보기 중 평균 등수 최저)
- `stats`   : 보기별 **평균 등수**(avg placement) + 표본 수 — `puzzle::Stats`

> ⚠️ 정책 주의: 오그먼트 **승률(win rate)** 표시는 금지. 항상 평균 등수를 쓴다.

## 스토리지 관리 (Supabase 무료 티어 등)

`db::delete_matches_before(pool, cutoff)`로 지난 패치 raw 매치를 정리하면
무료 티어 한도 안에서 운영 가능. crawler 끝에 주기 호출 권장.

## 다음 단계

- 퍼즐 타입 추가: `board_complete`, `item_combine`
- 특성(trait) 한글 매핑을 `meta`에 추가
- RSO 로그인 플로우 (현재는 puuid를 받는 자리만 마련)
- 오답 난이도 조절 (평균 등수가 비슷한 오그먼트로 구성 → 더 어려운 퍼즐)