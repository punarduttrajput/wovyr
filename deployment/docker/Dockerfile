# Single-binary Docker dev image for the Wovyr AI Platform.
#
# Builds the `wovyr` CLI (the all-in-one local entrypoint for v0.1) as a static-ish
# release binary and ships it on a minimal Debian base. Multi-stage keeps the
# final image small per docs/12-deployment/docker.md and docs/19-implementation-guide/build-system.md.
#
# Build from the repo root:
#   docker build -f deployment/docker/Dockerfile -t wovyr:dev .
# Run the hello agent (offline mock provider, no key needed):
#   docker run --rm wovyr:dev agents run --local \
#     -f examples/agents/hello.yaml --input '{"message":"Hi"}' --stream
#
# Pass --build-arg FEATURES="tiered-memory,postgres" to build with the tiered
# Postgres/Qdrant backends compiled in (see deployment/docker-compose.yml, which
# does exactly this) — the default empty FEATURES keeps this identical to the
# plain build above.

# ---- build stage ------------------------------------------------------------
FROM rust:1.96-slim-bookworm AS build

WORKDIR /src

# Copy the full workspace and build the CLI in release mode.
COPY . .
ARG FEATURES=""
RUN if [ -n "$FEATURES" ]; then \
      cargo build --release -p wovyr-cli --features "$FEATURES"; \
    else \
      cargo build --release -p wovyr-cli; \
    fi

# ---- runtime stage ----------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# CA certificates so the OpenAI-compatible provider and http_get tool can use
# TLS; curl so compose/orchestrators can healthcheck `/healthz` over real HTTP.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Run as a non-root user.
RUN useradd --create-home --uid 10001 wovyr
USER wovyr
WORKDIR /home/wovyr

# Bundle the example agents so the image is runnable out of the box.
COPY --from=build /src/target/release/wovyr /usr/local/bin/wovyr
COPY --from=build /src/examples ./examples

# OCI image metadata. `org.opencontainers.image.source` is load-bearing here, not
# cosmetic: GHCR links a pushed package to the repository named by this label, and
# an *unlinked* package grants no write access to that repository's Actions token —
# `permissions: packages: write` in the workflow is necessary but not sufficient.
# That's exactly how the v0.3.2 release failed (`denied: permission_denied:
# write_package`): the package had first been created by a label-less local
# `docker push`, so it was owned by the user and connected to no repository at all.
# Setting it in the Dockerfile (rather than only via the release workflow's
# `labels:`) means a manual local push links the package the same way CI's does.
LABEL org.opencontainers.image.title="wovyr" \
      org.opencontainers.image.description="Wovyr AI Platform — single-binary agent, workflow, and memory runtime" \
      org.opencontainers.image.source="https://github.com/punarduttrajput/wovyr" \
      org.opencontainers.image.url="https://github.com/punarduttrajput/wovyr" \
      org.opencontainers.image.licenses="Apache-2.0"

ENTRYPOINT ["wovyr"]
CMD ["--help"]
