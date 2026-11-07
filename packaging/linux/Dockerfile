# Builds the Linux release files of the engine, for x86_64 and aarch64: a
# static (musl) binary as a .tar.gz, a .deb and an .rpm, with SHA-256
# checksums. No interface, no graphical dependency. The packages also come
# under names without the version (grafeno-linux-amd64.deb,
# grafeno-linux-x86_64.rpm, grafeno-linux-arm64.deb,
# grafeno-linux-aarch64.rpm), which install.sh asks the latest release for.
# The .rpm carries the SELinux policy module, the .deb the AppArmor profile.
#
#   docker build -f packaging/linux/Dockerfile --output dist/linux .
#
# One architecture only: --build-arg TARGETS=x86_64-unknown-linux-musl

# The SELinux module, compiled with the policy tools of RHEL's family.
FROM almalinux:9 AS selinux
RUN dnf install -y -q selinux-policy-devel make && dnf clean all
COPY packaging/linux/selinux /selinux
WORKDIR /selinux
RUN make -f /usr/share/selinux/devel/Makefile grafeno.pp

FROM rust:1-slim-trixie AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools rpm dpkg-dev file python3-pip \
    && pip install --break-system-packages --no-cache-dir ziglang==0.13.0 \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install --locked cargo-deb cargo-generate-rpm cargo-zigbuild
WORKDIR /src
COPY . .
COPY --from=selinux /selinux/grafeno.pp /selinux/grafeno.pp
ARG TARGETS="x86_64-unknown-linux-musl aarch64-unknown-linux-musl"
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    TARGETS="$TARGETS" sh packaging/linux/build-packages.sh

FROM scratch
COPY --from=build /out /
