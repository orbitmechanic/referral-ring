# Build the binary
FROM rust:1-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# Ship just the binary and the static site
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=build /app/target/release/referral-ring /usr/local/bin/referral-ring
COPY web ./web

ENV DB_PATH=/data/ring.db \
    PORT=8080 \
    STATIC_DIR=/app/web

VOLUME ["/data"]
EXPOSE 8080

# The database is a single file in /data. Back it up by copying it.
CMD ["referral-ring", "serve"]
