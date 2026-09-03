# Builds the `maddo live` web UI as a container image.
#
# The build stage needs more than a plain Rust toolchain: wreq's TLS backend
# (btls-sys, a BoringSSL fork) compiles C and generates bindings, so cmake,
# clang/libclang, perl, and Go all have to be present. The runtime stage carries
# none of that, only the static-enough binary plus CA certificates.
#
# --browser mode is not supported in this image: no Chromium-based browser is
# installed, and the whole point of that fallback is driving a real, headed one.

FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        clang \
        cmake \
        golang-go \
        libclang-dev \
        perl \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Cache mounts keep the registry and target dir between builds; the binary is
# copied out of the cached target dir so the next stage can still see it.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
    && cp target/release/maddo /usr/local/bin/maddo

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --system --create-home --uid 10001 maddo

COPY --from=builder /usr/local/bin/maddo /usr/local/bin/maddo

USER maddo
WORKDIR /home/maddo
EXPOSE 8080

ENTRYPOINT ["maddo"]
CMD ["live", "--host", "0.0.0.0", "--port", "8080"]
