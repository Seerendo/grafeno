# Grafeno server image: `grafeno serve` on port 7810 (Grafeno Studio at
# /studio/), data in /data.
#
#   docker build -t grafeno .
#   docker run -p 7810:7810 -v grafeno-data:/data -e GRAFENO_TOKEN=change-me grafeno

FROM node:24-slim AS studio
WORKDIR /src
COPY clients/typescript clients/typescript
COPY studio/package.json studio/package-lock.json studio/
RUN cd studio && npm ci --no-audit --no-fund
COPY studio studio
RUN cd studio && npm run build

FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY . .
COPY --from=studio /src/studio/dist studio/dist
# The workspace's own sources are touched first: the cached target may hold
# a build of another version whose files look newer than these.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    find crates -name '*.rs' -exec touch {} + \
    && cargo build --release --locked -p grafeno-server \
    && cp target/release/grafeno /usr/local/bin/grafeno

FROM debian:trixie-slim
RUN useradd --system --uid 10001 --no-create-home grafeno \
    && mkdir /data && chown grafeno /data
COPY --from=build /usr/local/bin/grafeno /usr/local/bin/grafeno
USER grafeno
ENV GRAFENO_DB=/data/grafeno.redb \
    GRAFENO_ADDR=0.0.0.0:7810
VOLUME /data
EXPOSE 7810
ENTRYPOINT ["grafeno"]
CMD ["serve"]
