# Static-image Docker build for the Wovyr dashboard SPA (RM-GA-P4 OBS-805).
#
# A separate image from `deployment/docker/Dockerfile` (the Rust `wovyr` binary) —
# the dashboard is a static build served cross-origin against `wovyr-server` (see
# `dashboard/README.md`'s "Cross-origin deployment" section for the matching
# `WOVYR_CORS_ALLOWED_ORIGINS` server-side config), not compiled into that image.
#
# Build from the repo root:
#   docker build -f deployment/docker/dashboard.Dockerfile -t wovyr-dashboard:dev .
# Run it:
#   docker run --rm -p 8081:80 wovyr-dashboard:dev
#   curl http://localhost:8081/
#
# **Never run in a live container in this dev environment** (no Docker daemon here,
# same caveat as `deployment/docker/Dockerfile` and `docker-compose.yml`) — the
# underlying `npm run build` this Dockerfile's build stage runs *was* executed and
# verified directly in this environment (Node.js is available), producing a real
# `dist/dashboard/browser/` this Dockerfile then copies unchanged.

# ---- build stage -------------------------------------------------------------
FROM node:20-slim AS build

WORKDIR /src
COPY dashboard/package.json dashboard/package-lock.json* ./
RUN npm ci
COPY dashboard/ .
RUN npm run build

# ---- runtime stage ------------------------------------------------------------
FROM nginx:1.27-alpine AS runtime

# SPA fallback: any path not matching a real static file serves `index.html` so
# Angular's client-side router (app.routes.ts) handles it instead of nginx 404ing.
RUN printf 'server {\n\
    listen 80;\n\
    server_name _;\n\
    root /usr/share/nginx/html;\n\
    location / {\n\
        try_files $uri $uri/ /index.html;\n\
    }\n\
}\n' > /etc/nginx/conf.d/default.conf

COPY --from=build /src/dist/dashboard/browser /usr/share/nginx/html

EXPOSE 80
