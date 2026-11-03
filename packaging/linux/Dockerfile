# Builds the Linux release files: a static (musl) binary with Grafeno Studio
# inside, as a .tar.gz, a .deb and an .rpm, with SHA-256 checksums.
#
#   docker build -f packaging/linux/Dockerfile --output dist/linux .

FROM node:24-slim AS studio
WORKDIR /src
COPY clients/typescript clients/typescript
COPY studio/package.json studio/package-lock.json studio/
RUN cd studio && npm ci --no-audit --no-fund
COPY studio studio
RUN cd studio && npm run build

FROM rust:1-slim-trixie AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools rpm dpkg-dev file \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install --locked cargo-deb cargo-generate-rpm
WORKDIR /src
COPY . .
COPY --from=studio /src/studio/dist studio/dist
ENV TARGET=x86_64-unknown-linux-musl
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --target "$TARGET" -p grafeno-server \
    && file "target/$TARGET/release/grafeno" \
    && mkdir -p /out \
    && cargo deb --no-build --target "$TARGET" -p grafeno-server --output /out/ \
    && cargo generate-rpm --target "$TARGET" -p crates/grafeno-server --output /out/ \
    && staging="$(mktemp -d)" \
    && cp "target/$TARGET/release/grafeno" packaging/linux/grafeno.service packaging/linux/setup.sh \
          README.md "$staging/" \
    && tar -czf "/out/grafeno-$TARGET.tar.gz" -C "$staging" . \
    && cd /out && for f in *; do sha256sum "$f" | sed "s| .*|  $f|" > "$f.sha256"; done && ls -l /out

FROM scratch
COPY --from=build /out /
