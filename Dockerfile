FROM rust:1.97-bookworm AS builder

WORKDIR /app
COPY . .

RUN cargo build --release -p aios-daemon 2>&1

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/aiosd /usr/local/bin/aiosd

ENV AIOS_DATA_DIR=/app/data
ENV AIOS_BLOCKS_DIR=/app/blocks
ENV AIOS_MOCK_PROFILE=modern
ENV RUST_LOG=info

RUN mkdir -p /app/data /app/blocks

WORKDIR /app
STOPSIGNAL SIGINT
CMD ["aiosd"]
