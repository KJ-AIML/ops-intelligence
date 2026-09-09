# syntax=docker/dockerfile:1

FROM node:22-alpine AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY adapters ./adapters
COPY apps ./apps
COPY migrations ./migrations
RUN cargo build --release --locked -p ops-server -p ops-worker

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --no-create-home --shell /usr/sbin/nologin app
WORKDIR /app
COPY --from=build /src/target/release/ops-server /src/target/release/ops-worker /usr/local/bin/
COPY --from=web /web/dist ./web/dist
USER app
ENV APP_HOST=0.0.0.0 \
    APP_PORT=8080 \
    WEB_DIST_DIR=/app/web/dist
EXPOSE 8080
CMD ["ops-server"]
