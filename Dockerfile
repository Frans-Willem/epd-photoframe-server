FROM rust:1.94-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates tzdata && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/epd-photoframe-server /usr/local/bin/
USER nobody
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/epd-photoframe-server"]
CMD ["/config.toml"]
