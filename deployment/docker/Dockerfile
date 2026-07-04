# Single-binary Docker dev image for the Apex AI Platform.
#
# Builds the `apex` CLI (the all-in-one local entrypoint for v0.1) as a static-ish
# release binary and ships it on a minimal Debian base. Multi-stage keeps the
# final image small per docs/12-deployment/docker.md and docs/19-implementation-guide/build-system.md.
#
# Build from the repo root:
#   docker build -f deployment/docker/Dockerfile -t apex:dev .
# Run the hello agent (offline mock provider, no key needed):
#   docker run --rm apex:dev agents run --local \
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
      cargo build --release -p apex-cli --features "$FEATURES"; \
    else \
      cargo build --release -p apex-cli; \
    fi

# ---- runtime stage ----------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# CA certificates so the OpenAI-compatible provider and http_get tool can use
# TLS; curl so compose/orchestrators can healthcheck `/healthz` over real HTTP.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Run as a non-root user.
RUN useradd --create-home --uid 10001 apex
USER apex
WORKDIR /home/apex

# Bundle the example agents so the image is runnable out of the box.
COPY --from=build /src/target/release/apex /usr/local/bin/apex
COPY --from=build /src/examples ./examples

ENTRYPOINT ["apex"]
CMD ["--help"]
