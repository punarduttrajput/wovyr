# Static-image Docker build for the Wovyr dashboard SPA (RM-GA-P4 OBS-805).
#
# A separate image from `deployment/docker/Dockerfile` (the Rust `wovyr` binary):
# the dashboard is a static Angular build, not compiled into that image. nginx
# serves it **and reverse-proxies the platform API onto the same origin** via
# `dashboard-nginx.conf.template` — see that file for why same-origin is the only
# working shape (the SPA has no configurable API base URL, and its
# tenantInterceptor only authenticates URLs beginning `/api/`, so a cross-origin
# deployment would send unauthenticated requests to the wrong host no matter how
# `WOVYR_CORS_ALLOWED_ORIGINS` is configured server-side).
#
# Build from the repo root:
#   docker build -f deployment/docker/dashboard.Dockerfile -t wovyr-dashboard:dev .
#
# Run it against a `wovyr` container on the same Docker network (the default):
#   docker run --rm -p 8081:80 --network <net> wovyr-dashboard:dev
#   curl http://localhost:8081/healthz
#
# Run it against a host-network `wovyr dev` (the single-node appliance shape):
#   docker run --rm --network host \
#     -e WOVYR_UPSTREAM=127.0.0.1:8080 -e WOVYR_LISTEN=127.0.0.1:8081 \
#     wovyr-dashboard:dev
#
# **Never run in a live container in this dev environment** (no Docker daemon here,
# same caveat as `deployment/docker/Dockerfile` and `docker-compose.yml`) — the
# underlying `npm run build` this Dockerfile's build stage runs *was* executed and
# verified directly in this environment (Node.js is available), producing a real
# `dist/dashboard/browser/` this Dockerfile then copies unchanged. The proxy
# configuration was validated separately against a real running server on an EC2
# instance (2026-08-03): SPA, `/healthz`, `/api/v1/*` and the SSE streaming path
# all served from one origin, with the Monitoring, Workflows, Memory and Surfaces
# panels reading live data through it.

# ---- build stage -------------------------------------------------------------
FROM node:20-slim AS build

WORKDIR /src

# The dashboard reaches outside its own tree in exactly three places, by three
# different mechanisms — all of them silent until a build actually runs, and each
# resolved relative to the repo root, which is why the working directory has to
# sit one level in rather than at `/src`:
#
#   1. `@wovyr/ui-react` — an npm `file:../sdks/ui-react` dependency
#      (dashboard/package.json). Its `prebuild` hook
#      (dashboard/scripts/ensure-ui-react-built.js) also *builds* that package on
#      demand, so it must be present before `npm ci`, not just before `ng build`.
#   2. `@wovyr/sdk-types` — a TypeScript path mapping to
#      `../sdks/typescript/src/types` (dashboard/tsconfig.json). Not an npm
#      dependency at all, so nothing in package.json hints that it is needed.
#   3. `../packages/tokens/wovyr-tokens.css` — a global stylesheet listed in
#      angular.json's `styles` array (in both the build and test configurations).
#
# Copying `dashboard/` alone (as this file did until 2026-08-03) fails on all
# three; copying only `sdks/ui-react` (until 2026-08-04) still failed on the
# other two, with four TS2307/unresolved-import errors that named the missing
# module rather than the missing directory. If a fourth such reference is ever
# added, `container-scan`'s dashboard leg in .github/workflows/ci.yml is what
# catches it.
COPY sdks/ui-react ./sdks/ui-react
COPY sdks/typescript ./sdks/typescript
COPY packages/tokens ./packages/tokens

# Manifests before sources, so the install layer caches across source-only edits.
COPY dashboard/package.json dashboard/package-lock.json* ./dashboard/
WORKDIR /src/dashboard
RUN npm ci

COPY dashboard ./
RUN npm run build

# ---- runtime stage ------------------------------------------------------------
FROM nginx:1.27-alpine AS runtime

# Where the SPA sends its `/api/`, `/healthz` and `/metrics` traffic. Overridden
# per topology at run time (see the header) — this default matches the service
# name in deployment/docker-compose.yml.
ENV WOVYR_UPSTREAM=wovyr:8080

# nginx's `listen` value. A bare port is right for a published-port run; a
# host-network run wants an explicit loopback address so the dashboard isn't
# reachable off-box.
ENV WOVYR_LISTEN=80

# Restrict the entrypoint's envsubst pass to WOVYR_* names, so nginx's own
# $uri/$host/$scheme/$proxy_add_x_forwarded_for are left intact.
ENV NGINX_ENVSUBST_FILTER=^WOVYR_

# Templates under /etc/nginx/templates are rendered into /etc/nginx/conf.d at
# container start. `default.conf.template` renders to `default.conf`, replacing
# the base image's stock site.
COPY deployment/docker/dashboard-nginx.conf.template /etc/nginx/templates/default.conf.template

COPY --from=build /src/dashboard/dist/dashboard/browser /usr/share/nginx/html

EXPOSE 80
