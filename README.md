# Apex AI Platform

> An Enterprise AI Agent Operating System written in Rust.

![Version](https://img.shields.io/badge/version-0.1.0-blue)
![Rust](https://img.shields.io/badge/Rust-Edition%202024-orange)
![License](https://img.shields.io/badge/license-Apache%202.0-green)

---

## Overview

Apex is a next-generation AI Agent Operating System designed for building, deploying, and orchestrating intelligent autonomous agents at enterprise scale.

Unlike traditional AI frameworks that focus only on LLM orchestration, Apex provides a complete runtime platform featuring:

- AI Agent Runtime
- Workflow Engine
- Distributed Scheduler
- Tool Execution Engine
- Memory Engine
- Plugin Framework
- Multi-LLM Gateway
- Visual Workflow Studio
- Enterprise Security
- Cloud Native Deployment

Apex is designed from the ground up using Rust to provide high performance, memory safety, and scalability.

---

# Vision

To become the Linux of AI Agents.

Apex provides everything required to build enterprise-grade AI systems while remaining modular, extensible, secure, and cloud-native.

---

# Mission

Enable developers to build intelligent autonomous software using modern software engineering principles rather than prompt engineering alone.

---

# Core Principles

- Rust First
- API First
- Plugin First
- Event Driven
- Cloud Native
- AI Native
- Secure by Default
- Distributed by Design
- Observable by Default
- Developer Friendly

---

# Project Goals

The project aims to provide:

- AI Agent Framework
- Distributed Workflow Engine
- Long-Term Memory
- Semantic Search
- Tool Execution Framework
- Multi-Agent Collaboration
- Human-in-the-loop Workflows
- Visual Workflow Builder
- Enterprise Dashboard
- SDKs
- CLI
- Marketplace

---

# High-Level Architecture

                    Users
                      │
               Angular Dashboard
                      │
                REST / gRPC API
                      │
      ┌─────────────────────────────────┐
      │         Agent Runtime           │
      ├─────────────────────────────────┤
      │ Planner                         │
      │ Executor                        │
      │ Reasoner                        │
      │ Reflection                      │
      │ Tool Calling                    │
      │ Memory                          │
      └─────────────────────────────────┘
                      │
      ┌─────────────────────────────────┐
      │      Workflow Engine            │
      ├─────────────────────────────────┤
      │ Scheduler                       │
      │ Runtime                         │
      │ DAG                             │
      │ Checkpoint                      │
      │ Retry                           │
      └─────────────────────────────────┘
                      │
─────────────────────────────────────────────
 PostgreSQL
 Redis
 Qdrant
 Object Storage
 NATS

---

# Repository Structure

apex/

apps/
gateway/
dashboard/
worker/
scheduler/
cli/

crates/
agent-runtime/
workflow-engine/
planner/
executor/
memory/
plugin-sdk/
tool-runtime/
llm-gateway/
telemetry/
security/
storage/
eventbus/
scheduler/
config/
common/

docs/

examples/

plugins/

sdk/

deployment/

scripts/

---

# Technology Stack

Backend

- Rust

API

- Axum
- tonic (gRPC)

Frontend

- Angular

Database

- PostgreSQL

Cache

- Redis

Vector Database

- Qdrant

Messaging

- NATS

Observability

- OpenTelemetry
- Prometheus
- Grafana

Deployment

- Docker
- Kubernetes

---

# Key Features

## AI Agent Runtime

Supports

- Planning
- Reflection
- Tool Calling
- Context Management
- Goal Tracking
- Multi-Agent Collaboration

---

## Workflow Engine

Supports

- DAG
- State Machine
- Retry
- Compensation
- Long Running Workflows
- Event Driven Workflows
- Human Approval
- Parallel Execution

---

## Memory Engine

Supports

- Short-Term Memory
- Long-Term Memory
- Semantic Memory
- Episodic Memory
- Knowledge Graph

---

## Plugin Framework

Supports

- Dynamic Loading
- Versioning
- Permissions
- Sandboxed Execution

---

## Security

Supports

- OAuth2
- JWT
- RBAC
- Secrets Management
- Encryption
- Audit Logs

---

# Documentation

Full documentation lives in [`docs/`](docs/), indexed by [`docs/SUMMARY.md`](docs/SUMMARY.md).

The documentation is organized into the following sections:

00 Executive

01 Product

02 Architecture

03 Workflow Engine

04 Agent Framework

05 LLM Gateway

06 Memory Engine

07 Tool Runtime

08 Plugin SDK

09 API

10 Dashboard

11 CLI

12 Deployment

13 Security

14 Observability

15 Testing

16 Examples

17 ADR

18 Roadmap

19 Implementation Guide

---

# Development Status

Current Phase

v0.1 Foundations complete; **v0.2 (durability) in progress**. A Cargo workspace
implements: an agent runtime, an LLM gateway (chat + embeddings, mock +
OpenAI-compatible), a tool runtime (`echo`/`fs_read`/`http_get`/`shell` over a
native-process sandbox), a **durable workflow engine** (event-sourced DAG with
checkpointing, retry, resume, and saga **compensation**), a **memory engine**
(hybrid vector + keyword retrieval with ranking), a single-node HTTP server, and the
`apex` CLI (`login`/`dev`/`agents run`/`workflows run`/`memory`).

Current Version

0.1.0

## Quickstart (code)

```bash
# Build, lint, test
cargo build --workspace
cargo test --workspace

# Run the hello agent locally. With no API key it uses a deterministic mock
# provider; set OPENAI_API_KEY (and optionally APEX_OPENAI_BASE_URL) for a real model.
cargo run -p apex-cli -- agents run --local \
  -f examples/agents/hello.yaml \
  --input '{"message":"Hi, who are you?"}' --stream

# Or run against a single-node server:
cargo run -p apex-cli -- dev &                       # start the server
cargo run -p apex-cli -- agents run --server http://127.0.0.1:8080 \
  -f examples/agents/hello.yaml --input '{"message":"Hi"}'

# Run a durable workflow (event-sourced DAG with checkpoints + retry):
cargo run -p apex-cli -- workflows run --local -f examples/workflows/greet-and-fetch.yaml

# Saga rollback: a failing step triggers reverse-order compensation:
cargo run -p apex-cli -- workflows run --local -f examples/workflows/saga-order.yaml

# Store and query memory (hybrid retrieval, persisted under ~/.apex/memory):
cargo run -p apex-cli -- memory put --namespace kb --content "Refund window is 30 days." --importance 0.9
cargo run -p apex-cli -- memory query "refund policy" --namespace kb
```

See [`docs/16-examples/hello-agent.md`](docs/16-examples/hello-agent.md) and
[`docs/18-roadmap/v0.1.md`](docs/18-roadmap/v0.1.md).

---

# Roadmap

Phase 1

Documentation

Phase 2

Architecture

Phase 3

Workflow Engine

Phase 4

Agent Runtime

Phase 5

Memory Engine

Phase 6

Plugin SDK

Phase 7

Dashboard

Phase 8

Cloud Deployment

---

# Contributing

See CONTRIBUTING.md

---

# License

Apache License 2.0

---

# Acknowledgements

Apex is inspired by advances in distributed systems, workflow orchestration, cloud-native platforms, and modern AI agent architectures while being designed as an original, modular implementation focused on enterprise software engineering.
