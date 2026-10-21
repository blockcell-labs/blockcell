# syntax=docker/dockerfile:1.6

# ============================================
# Stage 1: Build WebUI
# ============================================
FROM node:20-alpine AS webui-builder

WORKDIR /webui

# Copy package files first for better layer caching
COPY webui/package.json webui/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm \
    npm ci --prefer-offline

COPY webui ./
RUN npm run build

# ============================================
# Stage 2: Build Rust Application
# ============================================
FROM rust:1.96-slim-bookworm AS builder

# Install build dependencies
RUN apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 update \
    && apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 install -y --no-install-recommends --fix-missing \
        ca-certificates \
        build-essential \
        pkg-config \
        libssl-dev \
        libprotobuf-dev \
        protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy dependency files first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY bin ./bin
COPY skills ./skills

# Copy webui dist (required for rust-embed)
COPY --from=webui-builder /webui/dist ./webui/dist

# Build with caching
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build -p blockcell --bin blockcell --release --locked \
    && cp /app/target/release/blockcell /app/blockcell

# ============================================
# Stage 3: Runtime
# ============================================
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies
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
