# ============================================================
# 멀티스테이지 빌드
#   1) builder: 무거운 Rust 툴체인으로 컴파일
#   2) runtime: 가벼운 데비안 이미지에 바이너리만 복사
# edition 2024라 Rust 1.85+ 필요 → 최신 안정 이미지 사용
# ============================================================

# ---------- 1) 빌드 스테이지 ----------
FROM rust:1-bookworm AS builder

WORKDIR /app

# 의존성 캐싱: 매니페스트 먼저 복사해 의존성만 미리 빌드하면,
# 소스만 바뀔 때 의존성 재컴파일을 건너뛴다(빌드 속도↑).
COPY Cargo.toml Cargo.lock ./
# 더미 소스로 의존성 레이어를 먼저 굳힘
RUN mkdir -p src/bin && \
    echo "fn main() {}" > src/bin/server.rs && \
    echo "" > src/lib.rs && \
    cargo build --release --bin server 2>/dev/null || true

# 실제 소스 복사 후 본 빌드
COPY . .
# 더미 빌드 산물 무효화 (타임스탬프 갱신)
RUN touch src/lib.rs src/bin/server.rs && \
    cargo build --release \
    --bin server \
    --bin crawler \
    --bin item_quiz_gen \
    --bin scheduler

# ---------- 2) 런타임 스테이지 ----------
FROM debian:bookworm-slim AS runtime

# TLS(reqwest HTTPS) + CA 인증서에 필요한 최소 패키지
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 빌드 산물(server 바이너리)만 복사
COPY --from=builder /app/target/release/server        /app/server
COPY --from=builder /app/target/release/crawler       /app/crawler
COPY --from=builder /app/target/release/item_quiz_gen /app/item_quiz_gen
COPY --from=builder /app/target/release/scheduler     /app/scheduler


# Fly.io가 넘겨주는 포트. 기본 8080.
ENV BIND_ADDR=0.0.0.0:8080
EXPOSE 8080

CMD ["/app/server"]
