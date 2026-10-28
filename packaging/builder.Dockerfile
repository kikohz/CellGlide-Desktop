# Builds CellGlide packages against glibc 2.35 (Ubuntu 22.04), so the binaries run on
# every supported distribution.
FROM ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
# dpkg-dev provides dpkg-shlibdeps for cargo-deb's $auto; rpm is used to inspect the rpm.
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential pkg-config curl ca-certificates git dpkg-dev rpm \
 && rm -rf /var/lib/apt/lists/*
# Pinned so builds are reproducible; keep in step with rust-toolchain.toml and rust-version in Cargo.toml.
ARG RUST_VERSION=1.99.0
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "$RUST_VERSION"
ENV PATH=/root/.cargo/bin:$PATH
# Same version as rust-toolchain.toml; set explicitly so the image never builds with another one.
ENV RUSTUP_TOOLCHAIN=$RUST_VERSION
RUN cargo install --locked cargo-deb --version 3.8.0 \
 && cargo install --locked cargo-generate-rpm --version 0.21.0
WORKDIR /src
