# ---- build stage ----
FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json


# сначала копируем только манифесты — для кэша слоёв зависимостей
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
RUN cargo build --release -p ob_core

# ---- runtime stage ----
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/ob_core /usr/local/bin/ob_core

ENV OBSISTENT_CONFIG=/etc/obscape/config.toml
EXPOSE 11080
ENTRYPOINT ["/usr/local/bin/ob_core"]
