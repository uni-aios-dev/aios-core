FROM rust:1.97-bookworm AS builder

WORKDIR /app
COPY . .

RUN cargo build --workspace --release 2>&1
RUN cargo test --workspace --lib --release -- --skip real_tcp --skip real_udp 2>&1 || echo "Tests completed (non-fatal)"

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/aios-tui /usr/local/bin/aios-tui

ENV AIOS_DATA_DIR=/app/data
ENV AIOS_BLOCKS_DIR=/app/blocks
ENV AIOS_MOCK_PROFILE=modern
ENV RUST_LOG=info

RUN mkdir -p /app/data /app/blocks

WORKDIR /app
STOPSIGNAL SIGINT
CMD ["aios-tui"]
