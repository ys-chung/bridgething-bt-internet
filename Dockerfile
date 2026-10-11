# syntax=docker/dockerfile:1
FROM rust:1-trixie AS build
RUN apt-get update \
  && apt-get install -y --no-install-recommends cmake \
  && rm -rf /var/lib/apt/lists/*
# libopus is built from source by audiopus_sys and linked statically;
# its CMakeLists predates CMake 3.5.
ENV LIBOPUS_STATIC=1 CMAKE_POLICY_VERSION_MINIMUM=3.5
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
  && cp target/release/bt-gateway /usr/local/bin/bt-gateway

FROM debian:trixie-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates tzdata \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --system --uid 10001 --home-dir /data gateway \
  && mkdir /data && chown gateway:gateway /data
COPY --from=build /usr/local/bin/bt-gateway /usr/local/bin/bt-gateway
USER gateway
VOLUME /data
ENTRYPOINT ["/usr/local/bin/bt-gateway"]
