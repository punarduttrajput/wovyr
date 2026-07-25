//! OBS-302: the engine, durable store, and queue/worker paths emit `tracing`
//! spans that nest into one end-to-end trace — submit→enqueue→lease→resume→
//! append/checkpoint all hang off a common ancestor, so an OTLP backend (the
//! `wovyr-telemetry` `otlp` feature) can show a workflow's full causal chain.
//!
//! The collector below is a minimal hand-rolled `tracing::Subscriber` (no
//! `tracing-subscriber` dev-dep): it records each span's name and contextual
//! parent. It is installed thread-locally (`set_default`) and the test runs on
//! a current-thread runtime, so parentage tracking via an enter/exit stack is
//! deterministic.

use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};
use wovyr_workflow::{
    ClosureExecutor, Definition, DefinitionResolver, Engine, FileStore, InMemoryWorkQueue,
    WorkQueue, Worker,
};

#[derive(Default)]
struct CollectorInner {
    next_id: u64,
    /// Enter/exit stack — the contextual parent for a new span.
    stack: Vec<u64>,
    names: HashMap<u64, &'static str>,
    parents: HashMap<u64, Option<u64>>,
}

#[derive(Clone, Default)]
struct Collector {
    inner: Arc<Mutex<CollectorInner>>,
}

impl Collector {
    /// Every collected span name.
    fn names(&self) -> Vec<&'static str> {
        let inner = self.inner.lock().expect("collector poisoned");
        inner.names.values().copied().collect()
    }

    /// Whether some span named `child` has an ancestor named `ancestor`.
    fn has_ancestry(&self, ancestor: &str, child: &str) -> bool {
        let inner = self.inner.lock().expect("collector poisoned");
        'spans: for (id, name) in &inner.names {
            if *name != child {
                continue;
            }
            let mut cur = *id;
            loop {
                match inner.parents.get(&cur).copied().flatten() {
                    Some(parent) => {
                        if inner.names.get(&parent).copied() == Some(ancestor) {
                            return true;
                        }
                        cur = parent;
                    }
                    None => continue 'spans,
                }
            }
        }
        false
    }
}

impl Subscriber for Collector {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let mut inner = self.inner.lock().expect("collector poisoned");
        inner.next_id += 1;
        let id = inner.next_id;
        let parent = if attrs.is_contextual() {
            inner.stack.last().copied()
        } else {
            attrs.parent().map(Id::into_u64)
        };
        inner.names.insert(id, attrs.metadata().name());
        inner.parents.insert(id, parent);
        Id::from_u64(id)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}
    fn event(&self, _event: &Event<'_>) {}

    fn enter(&self, span: &Id) {
        self.inner
            .lock()
            .expect("collector poisoned")
            .stack
            .push(span.into_u64());
    }

    fn exit(&self, span: &Id) {
        let mut inner = self.inner.lock().expect("collector poisoned");
        if let Some(pos) = inner.stack.iter().rposition(|s| *s == span.into_u64()) {
            inner.stack.remove(pos);
        }
    }
}

fn def() -> Definition {
    Definition::from_yaml(
        "metadata:\n  name: traced\nspec:\n  activities:\n    - {id: a, type: function}\n",
    )
    .expect("valid definition")
}

fn executor() -> ClosureExecutor {
    ClosureExecutor::new().on("a", |_| async { Ok(json!({"ok": true})) })
}

/// Submit→enqueue→worker-lease→resume over the durable `FileStore`: the store
/// and engine spans must nest under the worker's step span (the end-to-end
/// trace OBS-302 asks for), and the submit path must trace its own writes.
#[tokio::test]
async fn worker_driven_run_produces_one_nested_trace() {
    let collector = Collector::default();
    let dispatch = tracing::Dispatch::new(collector.clone());
    let _guard = tracing::dispatcher::set_default(&dispatch);

    let dir = std::env::temp_dir().join(format!("wovyr_trace_spans_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(FileStore::new(&dir).expect("file store"));
    let queue = Arc::new(InMemoryWorkQueue::new());

    // Submit: durably start (no activities yet) and enqueue.
    let submitter = Engine::new(store.clone(), store.clone(), Arc::new(executor()));
    submitter
        .start(&def(), "wf-traced", json!({}))
        .await
        .expect("start");
    queue.enqueue("wf-traced").await.expect("enqueue");

    // A worker leases and drives it to completion.
    let resolver: DefinitionResolver = Arc::new(|name: &str| (name == "traced").then(def));
    let worker = Worker::new(
        "w1",
        Engine::new(store.clone(), store.clone(), Arc::new(executor())),
        queue.clone() as Arc<dyn WorkQueue>,
        store.clone(),
        resolver,
    );
    let stepped = worker.step().await.expect("worker step");
    assert!(stepped.is_some(), "worker processed the queued execution");

    // Every layer emitted its span…
    let names = collector.names();
    for expected in [
        "workflow.start",
        "workflow.store.append",
        "workflow.store.checkpoint",
        "workflow.worker.step",
        "workflow.resume",
        "workflow.activity",
        "workflow.store.checkpoint_load",
    ] {
        assert!(
            names.contains(&expected),
            "missing span `{expected}` (got {names:?})"
        );
    }

    // …and they nest into one causal chain: the submit path traces its durable
    // writes, and the worker's step is the ancestor of the resume, the activity,
    // and the store writes the resume performs.
    for (ancestor, child) in [
        ("workflow.start", "workflow.store.append"),
        ("workflow.start", "workflow.store.checkpoint"),
        ("workflow.worker.step", "workflow.resume"),
        ("workflow.worker.step", "workflow.store.checkpoint_load"),
        ("workflow.resume", "workflow.activity"),
        ("workflow.resume", "workflow.store.append"),
        ("workflow.resume", "workflow.store.checkpoint"),
    ] {
        assert!(
            collector.has_ancestry(ancestor, child),
            "span `{child}` should nest under `{ancestor}`"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
