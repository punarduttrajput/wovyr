# DAG Engine Specification

**Document ID:** WF-004
**Version:** 1.0.0
**Status:** Draft
**Owner:** Workflow Engine Team
**Last Updated:** 2026-06-26

---

# 1. Purpose

The DAG Engine transforms a validated Workflow Intermediate Representation (WIR) into an executable graph.

It is responsible for:

* Graph construction
* Dependency analysis
* Execution planning
* Parallel scheduling
* Dynamic graph expansion
* Cycle detection
* Node activation
* Execution ordering

The DAG Engine is independent of activity implementations.

---

# 2. Objectives

The DAG Engine must provide:

* Deterministic execution
* Parallel scheduling
* Efficient dependency resolution
* Runtime graph expansion
* Replay compatibility
* Horizontal scalability

---

# 3. Core Concepts

The execution graph consists of:

* Nodes
* Edges
* Dependencies
* Conditions
* Execution metadata

Each workflow execution owns a graph instance.

---

# 4. Graph Model

```text
                 Workflow Graph

            ┌──────────────┐
            │    Start     │
            └──────┬───────┘
                   │
        ┌──────────┴──────────┐
        ▼                     ▼
   Validate              Load Profile
        │                     │
        └──────────┬──────────┘
                   ▼
            AI Classification
                   │
          ┌────────┴────────┐
          ▼                 ▼
      Auto Approve      Manager Review
          │                 │
          └────────┬────────┘
                   ▼
                  Store
                   │
                   ▼
                  End
```

The graph must be acyclic after expansion.

---

# 5. Node Types

Supported node categories:

| Type        | Description              |
| ----------- | ------------------------ |
| Start       | Entry point              |
| End         | Terminal node            |
| Activity    | Executable task          |
| Decision    | Conditional branch       |
| Merge       | Join parallel branches   |
| Split       | Create parallel branches |
| Event       | Wait for external event  |
| Timer       | Delay execution          |
| AI          | LLM inference            |
| Human       | Manual task              |
| SubWorkflow | Nested workflow          |
| Dynamic     | Runtime-generated node   |

---

# 6. Edge Types

Edges define execution relationships.

Supported types:

* Sequential
* Conditional
* Parallel
* Event-triggered
* Retry
* Compensation

Each edge may contain activation rules.

---

# 7. Node State Machine

Each node transitions through:

```text
Created
   │
   ▼
Ready
   │
   ▼
Scheduled
   │
   ▼
Running
   │
 ┌─┴──────────────┐
 ▼                ▼
Completed      Failed
                  │
                  ▼
              Retrying
```

Completed nodes never execute again unless replay explicitly requires it.

---

# 8. Graph Construction

Construction steps:

1. Parse WIR
2. Create nodes
3. Create edges
4. Validate references
5. Detect cycles
6. Build adjacency lists
7. Compute dependency counts
8. Persist graph metadata

---

# 9. Graph Representation

Recommended Rust structures:

```rust
pub struct Dag {
    nodes: HashMap<NodeId, Node>,
    edges: Vec<Edge>,
    incoming: HashMap<NodeId, Vec<NodeId>>,
    outgoing: HashMap<NodeId, Vec<NodeId>>,
}
```

The representation must support efficient traversal and updates.

---

# 10. Topological Ordering

The engine computes a topological order before execution.

Algorithm requirements:

* O(V + E) complexity
* Deterministic ordering
* Stable output for identical graphs

Kahn's Algorithm is recommended as the default implementation.

---

# 11. Dependency Resolution

A node becomes executable only when:

* All required predecessors have completed.
* Conditional expressions evaluate to true.
* Resource constraints are satisfied.
* Security checks pass.

Dependency counts are updated after each completed node.

---

# 12. Parallel Scheduling

Independent nodes execute concurrently.

Example:

```text
          Start
            │
      ┌─────┴─────┐
      ▼           ▼
   Fraud      Inventory
      │           │
      └─────┬─────┘
            ▼
         Shipping
```

The scheduler dispatches both branches as soon as they are ready.

---

# 13. Fan-Out / Fan-In

## Fan-Out

One node activates multiple successors.

## Fan-In

Execution continues only after all required predecessors complete.

Merge policies may specify:

* All branches
* Any branch
* Configurable quorum

---

# 14. Conditional Execution

Decision nodes evaluate expressions.

Example:

```yaml
decision:
  when: riskScore > 80
  goto: manualReview
```

Conditions are evaluated exactly once unless replayed.

---

# 15. Dynamic Graph Expansion

Certain nodes may generate new nodes during execution.

Example:

```text
AI Planner
     │
     ▼
Generate Tasks
     │
 ┌───┼────┐
 ▼   ▼    ▼
A    B    C
```

Rules:

* Expansion occurs only at designated Dynamic nodes.
* New nodes must preserve acyclic structure.
* Expansion events are persisted for deterministic replay.

---

# 16. Cycle Detection

Graphs must not contain cycles.

Validation occurs:

* During compilation
* After dynamic expansion

Detected cycles prevent execution.

---

# 17. Execution Planning

Planning algorithm:

1. Compute initial ready queue.
2. Dispatch eligible nodes.
3. Persist node results.
4. Update dependency counters.
5. Activate newly ready nodes.
6. Repeat until completion.

---

# 18. Ready Queue

Ready nodes are stored in a priority-aware queue.

Priority may consider:

* Workflow policy
* Node priority
* Resource requirements
* Deadlines

FIFO ordering is used among equal priorities.

---

# 19. Failure Propagation

Node failures may:

* Retry
* Trigger compensation
* Skip downstream nodes
* Abort workflow
* Redirect execution

Propagation behavior is defined by workflow policy.

---

# 20. Replay Behavior

Replay reconstructs the graph from:

* Workflow definition
* Dynamic expansion events
* Checkpoints
* Execution history

Replay must produce the same executable graph.

---

# 21. Performance Targets

| Metric             | Target   |
| ------------------ | -------- |
| Graph construction | < 10 ms  |
| Topological sort   | O(V + E) |
| Ready queue update | O(log N) |
| Node activation    | < 1 ms   |
| Dynamic expansion  | < 20 ms  |

Targets apply to typical enterprise workflows (<10,000 nodes).

---

# 22. Observability

Expose metrics for:

* Active nodes
* Completed nodes
* Failed nodes
* Queue depth
* Graph expansion count
* Parallel branch count
* Critical path duration

Each graph execution is traceable through the workflow Correlation ID.

---

# 23. Rust Crate Mapping

Recommended module layout:

```text
engine-workflow/
└── dag/
    ├── graph.rs
    ├── node.rs
    ├── edge.rs
    ├── planner.rs
    ├── scheduler.rs
    ├── ready_queue.rs
    ├── expansion.rs
    ├── topology.rs
    ├── validator.rs
    └── mod.rs
```

Each module should have a single, well-defined responsibility.

---

# 24. Design Constraints

* Graphs must remain acyclic.
* Execution order must be deterministic.
* Dynamic expansion must be replayable.
* Node execution must be idempotent.
* Scheduling must not depend on wall-clock timing.

---

# 25. Related Documents

* Workflow Overview
* Execution Model
* Workflow DSL
* Scheduler
* State Machine
* Checkpointing
* Retry Engine
* Distributed Execution
* Persistence
* Rust Crate Design

---

# 26. Revision History

| Version | Date       | Description                      |
| ------- | ---------- | -------------------------------- |
| 1.0.0   | 2026-06-26 | Initial DAG Engine Specification |
