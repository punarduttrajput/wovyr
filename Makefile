# Task runner for the Wovyr AI Platform.
# See docs/19-implementation-guide/build-system.md.

.PHONY: setup dev build test lint fmt run-hello docker-build docker-run-hello compose-up compose-down clean \
	dashboard-build dashboard-test dashboard-dev

# One-time contributor setup. Everything here is idempotent — safe to re-run.
# `wasm32-wasip1` is needed only for plugin-SDK work (`wovyr plugin new|build`)
# and by the scaffold round-trip test, which skips cleanly without it; it is
# installed here so a contributor's first `make test` matches CI.
setup:
	rustup component add rustfmt clippy
	rustup target add wasm32-wasip1
	cargo build --workspace
	@echo
	@echo "Setup complete. Next: 'make dev' (server) or 'make run-hello' (one agent)."
	@echo "Both run offline against the deterministic mock provider — no API key needed."

# Run the all-in-one local server on 127.0.0.1:8080.
# WOVYR_ALLOW_ANONYMOUS=1 skips credential setup for local development; it only
# works on a loopback bind (the server refuses to start otherwise), so it cannot
# reach the network. See docs/12-deployment/ for a real deployment's auth setup.
dev:
	WOVYR_ALLOW_ANONYMOUS=1 cargo run -p wovyr-cli -- dev

build:
	cargo build --workspace

test:
	cargo test --workspace

lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --all --check

fmt:
	cargo fmt --all

# Run the hello agent locally with the embedded runtime (mock provider unless
# OPENAI_API_KEY is set). See docs/16-examples/hello-agent.md.
run-hello:
	cargo run -p wovyr-cli -- agents run --local \
		-f examples/agents/hello.yaml \
		--input '{"message":"Hi, who are you?"}' --stream

# Build the single-binary dev image (see deployment/docker/Dockerfile).
docker-build:
	docker build -f deployment/docker/Dockerfile -t wovyr:dev .

# Run the hello agent inside the dev image (offline mock provider).
docker-run-hello: docker-build
	docker run --rm wovyr:dev agents run --local \
		-f examples/agents/hello.yaml \
		--input '{"message":"Hi, who are you?"}' --stream

# Bring up wovyr + Postgres + Qdrant (see deployment/docker-compose.yml).
compose-up:
	docker compose -f deployment/docker-compose.yml up -d --build

compose-down:
	docker compose -f deployment/docker-compose.yml down

clean:
	cargo clean

# DX-501: the dashboard depends on `@wovyr/ui-react` (sdks/ui-react) via a
# `file:` dependency and resolves its dist/ output — these targets build that
# dependency first (via dashboard's own prebuild/pretest/prestart npm hooks,
# see dashboard/scripts/ensure-ui-react-built.js) so `make dashboard-build` etc.
# work the same from a clean checkout as they do in CI.
dashboard-build:
	cd dashboard && npm ci && npm run build

dashboard-test:
	cd dashboard && npm ci && npm test -- --watch=false --browsers=ChromeHeadless

dashboard-dev:
	cd dashboard && npm ci && npm start
