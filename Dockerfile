# syntax=docker/dockerfile:1

FROM rust:1.89-bookworm AS builder
WORKDIR /build

RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /var/lib/mcp-moonraker \
        --shell /usr/sbin/nologin mcp-moonraker \
    && install -d -o mcp-moonraker -g mcp-moonraker -m 0700 /var/lib/mcp-moonraker

COPY --from=builder /build/target/release/mcp-moonraker /usr/local/bin/mcp-moonraker

USER mcp-moonraker
WORKDIR /var/lib/mcp-moonraker

# HTTP is the only useful transport in a container. A token is still required
# when binding to a non-loopback address.
ENV MCP_MOONRAKER_TRANSPORT=http \
    MCP_MOONRAKER_HTTP_HOST=0.0.0.0 \
    MCP_MOONRAKER_HTTP_PORT=8765 \
    MCP_MOONRAKER_STATE_DIR=/var/lib/mcp-moonraker \
    MCP_MOONRAKER_LOG_LEVEL=info

EXPOSE 8765

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8765/health || exit 1

ENTRYPOINT ["mcp-moonraker"]
CMD ["serve"]
