# Task runner for the Apex AI Platform.
# See docs/19-implementation-guide/build-system.md.

.PHONY: build test lint fmt run-hello docker-build docker-run-hello clean

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
	cargo run -p apex-cli -- agents run --local \
		-f examples/agents/hello.yaml \
		--input '{"message":"Hi, who are you?"}' --stream

# Build the single-binary dev image (see deployment/docker/Dockerfile).
docker-build:
	docker build -f deployment/docker/Dockerfile -t apex:dev .

# Run the hello agent inside the dev image (offline mock provider).
docker-run-hello: docker-build
	docker run --rm apex:dev agents run --local \
		-f examples/agents/hello.yaml \
		--input '{"message":"Hi, who are you?"}' --stream

clean:
	cargo clean
