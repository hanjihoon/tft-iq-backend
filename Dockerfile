# ============================================================
# 멀티스테이지 빌드
#   1) builder: 무거운 Rust 툴체인으로 컴파일
#   2) runtime: 가벼운 데비안 이미지에 바이너리만 복사
# edition 2024라 Rust 1.85+ 필요 → 최신 안정 이미지 사용
# ============================================================

# ---------- 1) 빌드 스테이지 ----------
# ---------- 0) chef 준비 ----------
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef
WORKDIR /app

# ---------- 1) 레시피 생성 (의존성 목록만 추출) ----------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---------- 2) 의존성 빌드 (여기가 캐시됨) ----------
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# 소스가 바뀌어도 recipe.json이 같으면 이 레이어는 캐시 히트
RUN cargo chef cook --release --recipe-path recipe.json

# ---------- 3) 실제 빌드 ----------
COPY . .
RUN cargo build --release \
    --bin server \
    --bin crawler \
    --bin scheduler \
    --bin aggregate_combos \
    --bin combo_quiz_gen \
    --bin special_item_class \
    --bin aggregate_special

# ---------- 4) 런타임 ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get upgrade -y && \
    apt-get install -y --no-install-recommends ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/server             /app/server
COPY --from=builder /app/target/release/crawler            /app/crawler
COPY --from=builder /app/target/release/scheduler          /app/scheduler
COPY --from=builder /app/target/release/aggregate_combos   /app/aggregate_combos
COPY --from=builder /app/target/release/combo_quiz_gen     /app/combo_quiz_gen
COPY --from=builder /app/target/release/special_item_class /app/special_item_class
COPY --from=builder /app/target/release/aggregate_special  /app/aggregate_special

ENV BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
CMD ["/app/server"]