# syntax=docker/dockerfile:1.7
# Chef's Prodigy recipe node (and the llm-gateway binary, same image).
#   build: docker build -t chefsprodigy .
#   run:   docker run -d -p 8080:8080 -v chefs-data:/data -e NODE_API_KEY=... chefsprodigy
#
# Stage 1 compiles the binary with a pinned toolchain (rig-core 0.45 needs rustc 1.97+)
# on Debian bookworm, the same glibc generation as the distroless runtime below.
# rust-toolchain.toml is left out through .dockerignore so the image's own toolchain
# is used instead of downloading one at build time.
FROM rust:1.97-slim-bookworm AS build
WORKDIR /src
COPY . .
# Registry and target caches survive between builds on the same BuildKit; the finished
# binary is copied out because cache mounts are not part of the resulting layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p node-server -p llm-gateway \
    && cp target/release/node-server /node-server \
    && cp target/release/llm-gateway /llm-gateway \
    && mkdir -p /data

# Stage 2: distroless runtime. No shell, no package manager, non-root (uid 65532).
# cc-debian12 carries libc, libgcc and libstdc++, which the bundled SQLite and the Rust
# runtime need; TLS is rustls, so there is no OpenSSL to patch.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /node-server /usr/local/bin/node-server
# The LLM gateway (docs/SPEC-hosted-platform.md §5.2) ships in the same image; the
# Compose stack runs it with `entrypoint: /usr/local/bin/llm-gateway`.
COPY --from=build /llm-gateway /usr/local/bin/llm-gateway
COPY --from=build --chown=65532:65532 /data /data
ENV NODE_HOST=0.0.0.0 \
    NODE_PORT=8080 \
    NODE_DATA_DIR=/data \
    GATEWAY_HOST=0.0.0.0 \
    GATEWAY_PORT=8080 \
    GATEWAY_DATA_DIR=/data \
    TOKIO_WORKER_THREADS=2 \
    RUST_LOG=info,tower_http=info
VOLUME ["/data"]
EXPOSE 8080
# The binary probes itself (docs/SPEC-hosted-platform.md §5.1); no curl or wget needed.
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["/usr/local/bin/node-server", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/node-server"]
CMD ["serve"]
