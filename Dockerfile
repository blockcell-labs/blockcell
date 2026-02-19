# syntax=docker/dockerfile:1.6

FROM node:20-alpine AS webui-builder

WORKDIR /webui

COPY webui/package.json webui/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm \
    npm ci

COPY webui ./
RUN npm run build

FROM rust:1-slim-bookworm AS builder

RUN apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 update \
    && apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 install -y --no-install-recommends --fix-missing \
        ca-certificates \
        build-essential \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY bin ./bin

# webui/dist must exist at compile time for rust-embed in gateway.rs
COPY --from=webui-builder /webui/dist ./webui/dist

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build -p blockcell --bin blockcell --release --locked \
    && cp /app/target/release/blockcell /app/blockcell


FROM debian:bookworm-slim AS runtime

RUN apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 update \
    && apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 install -y --no-install-recommends --fix-missing ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system blockcell \
    && useradd --system --gid blockcell --create-home --home-dir /home/blockcell blockcell \
    && mkdir -p /home/blockcell/.blockcell/workspace \
    && chown -R blockcell:blockcell /home/blockcell

WORKDIR /home/blockcell

COPY --from=builder /app/blockcell /usr/local/bin/blockcell

USER blockcell

ENTRYPOINT ["/usr/local/bin/blockcell"]
