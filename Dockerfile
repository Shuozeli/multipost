FROM rust:1.95-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
RUN cargo build --release -p multipost-server -p multipost-cli

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --home-dir /var/lib/multipost --shell /usr/sbin/nologin multipost

COPY --from=builder /src/target/release/multipost-server /usr/local/bin/multipost-server
COPY --from=builder /src/target/release/multipost /usr/local/bin/multipost

ENV MULTIPOST_GRPC_ADDR=0.0.0.0:8188 \
    MULTIPOST_HTTP_ADDR=0.0.0.0:8189 \
    MULTIPOST_DATA_DIR=/var/lib/multipost \
    PWRIGHT_BIN=/usr/local/bin/pwright

VOLUME ["/var/lib/multipost"]
EXPOSE 8188 8189

USER multipost
CMD ["/usr/local/bin/multipost-server"]
