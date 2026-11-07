# Builds the Linux release files of the engine: a static (musl) binary, as a
# .tar.gz, a .deb and an .rpm, with SHA-256 checksums. No interface, no
# graphical dependency. The packages also come under names without the
# version (grafeno-linux-amd64.deb, grafeno-linux-x86_64.rpm), which
# install.sh asks the latest release for.
#
#   docker build -f packaging/linux/Dockerfile --output dist/linux .

FROM rust:1-slim-trixie AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools rpm dpkg-dev file \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install --locked cargo-deb cargo-generate-rpm
WORKDIR /src
COPY . .
ENV TARGET=x86_64-unknown-linux-musl
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --target "$TARGET" -p grafeno-server \
    && file "target/$TARGET/release/grafeno" \
    && mkdir -p /out \
    && cargo deb --no-build --target "$TARGET" -p grafeno-server --output /out/ \
    && cargo generate-rpm --target "$TARGET" -p crates/grafeno-server --output /out/ \
    && cp /out/grafeno_*_amd64.deb /out/grafeno-linux-amd64.deb \
    && cp /out/grafeno-*.x86_64.rpm /out/grafeno-linux-x86_64.rpm \
    && staging="$(mktemp -d)" \
    && cp "target/$TARGET/release/grafeno" packaging/linux/grafeno.service packaging/linux/setup.sh \
          README.md "$staging/" \
    && tar -czf "/out/grafeno-$TARGET.tar.gz" -C "$staging" . \
    && cd /out && for f in *; do sha256sum "$f" | sed "s| .*|  $f|" > "$f.sha256"; done && ls -l /out

FROM scratch
COPY --from=build /out /
