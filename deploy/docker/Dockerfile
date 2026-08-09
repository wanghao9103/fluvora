# syntax=docker/dockerfile:1.7
FROM rust:1.96.0-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release --workspace \
    && cargo build --locked --release -p fluvora-media-node --features openssl-backend \
    && mkdir -p /out \
    && cp target/release/fluvora-admin /out/ \
    && cp target/release/fluvora-api-server /out/ \
    && cp target/release/fluvora-event-dispatcher /out/ \
    && cp target/release/fluvora-media-gateway /out/ \
    && cp target/release/fluvora-media-node /out/ \
    && cp target/release/fluvora-media-worker /out/ \
    && cp target/release/fluvora-status-service /out/ \
    && cp target/release/fluvora-turn-server /out/

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg libssl3 openssl tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 fluvora \
    && useradd --uid 10001 --gid fluvora --create-home --shell /usr/sbin/nologin fluvora \
    && mkdir -p /data/control /data/input /data/output /data/live /run/fluvora \
    && chown -R fluvora:fluvora /data /run/fluvora

COPY --from=builder /out/ /usr/local/bin/

USER 10001:10001
WORKDIR /data
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["fluvora-api-server"]
