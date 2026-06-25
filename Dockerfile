# syntax=docker/dockerfile:1

# ---------------------------------------------------------------------------
# Builder stage
#
# rust:1-bookworm ships a full GNU/C toolchain (gcc, libc headers), which the
# "bundled-sqlite" matrix-sdk feature needs because it compiles SQLite from C
# source. TLS is rustls, so no OpenSSL/system crypto libs are required.
#
# The floating `rust:1-bookworm` tag tracks the latest 1.x release on Debian
# bookworm (>= the project's required toolchain). Pin to a digest here if you
# need byte-for-byte reproducible builds.
# ---------------------------------------------------------------------------
FROM rust:1-bookworm AS builder

WORKDIR /app

# Copy the full source tree (Cargo.toml, Cargo.lock, src/, etc.). The build
# context MUST be trimmed with the companion .dockerignore at the repo root so
# that target/, .git/, *.log, and secret-bearing local files (.env,
# *.session.json) are never shipped to the daemon or baked into a builder layer.
COPY . .

# Build the release binary. Cache mounts keep the registry and target dir
# warm across builds without baking them into the image layer. --locked
# enforces the committed Cargo.lock. The binary is copied out of the cached
# target dir to a stable path so the next stage can pick it up (anything in a
# cache mount disappears once the RUN finishes).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked && \
    cp /app/target/release/matrix-mcp /usr/local/bin/matrix-mcp

# ---------------------------------------------------------------------------
# Runtime stage
#
# debian:bookworm-slim matches the builder's glibc so the dynamically linked
# binary runs unchanged. ca-certificates provides the system trust roots for
# outbound HTTPS to Matrix homeservers.
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

LABEL org.opencontainers.image.title="matrix-mcp" \
      org.opencontainers.image.description="A Model Context Protocol (MCP) server for the Matrix chat protocol, built on the official Rust SDKs (rmcp + matrix-sdk)." \
      org.opencontainers.image.source="https://github.com/qechris/matrix-mcp" \
      org.opencontainers.image.url="https://github.com/qechris/matrix-mcp" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.vendor="qechris"

# Install runtime dependencies, then drop apt lists to keep the image small.
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Create a dedicated non-root user/group to run the server. The fixed UID/GID
# 10001 gives predictable ownership for the /data volume across hosts.
RUN groupadd --system --gid 10001 matrix && \
    useradd --system --uid 10001 --gid 10001 --home-dir /data --no-create-home matrix

# Copy the statically-named release binary from the builder.
COPY --from=builder /usr/local/bin/matrix-mcp /usr/local/bin/matrix-mcp

# Default to the SSE / streamable-HTTP transport bound to all interfaces so the
# server is reachable from outside the container. The MCP endpoint is served at
# MATRIX_MCP_PATH (/mcp) on port 8000; it has no authentication of its own, so
# operators must front it with auth/TLS before exposing it to untrusted
# networks. Persist the encrypted SQLite crypto/state store and the session
# file under /data so they survive restarts.
ENV MATRIX_MCP_TRANSPORT=sse \
    MATRIX_MCP_ADDRESS=0.0.0.0:8000 \
    MATRIX_MCP_PATH=/mcp \
    MATRIX_STORE_PATH=/data/store \
    MATRIX_SESSION_FILE=/data/session.json \
    RUST_LOG=info

# Persistent data directory, owned by the non-root user so the server can write
# its store/session even when /data is a fresh (root-owned) volume mount point.
RUN mkdir -p /data/store && chown -R matrix:matrix /data
VOLUME ["/data"]

EXPOSE 8000

USER matrix

# Absolute path avoids any PATH-resolution surprises.
ENTRYPOINT ["/usr/local/bin/matrix-mcp"]
