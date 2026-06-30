//! Fair scheduling: weighted, tenant-fair admission of sandboxed work.
//!
//! [`SandboxPool`](crate::SandboxPool) bounds *how many* executions run at once; a
//! [`FairScheduler`] decides *whose* queued request gets the next freed slot so one
//! tenant can't monopolize capacity ([sandbox runtime §scheduling](../../docs/07-tool-runtime/sandbox-runtime.md)).
//!
//! Selection is **smooth weighted round-robin** (the nginx SWRR algorithm): each
//! admission credits every tenant with queued work by its weight and admits the one
//! with the highest running credit, then debits it — interleaving tenants fairly while
//! honoring weights, with no starvation. The policy is a deterministic state machine:
//! callers `submit` work, `poll` for the next admission when a slot is free, and
//! `complete` to release it.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

/// A shareable, tenant-fair admission scheduler bounded to `max_in_flight`
/// concurrent admissions. Cloning shares the same queue.
#[derive(Clone)]
pub struct FairScheduler<T> {
    inner: Arc<Mutex<Inner<T>>>,
}

struct Inner<T> {
    tenants: BTreeMap<String, TenantQueue<T>>,
    max_in_flight: usize,
    in_flight: usize,
}

struct TenantQueue<T> {
    weight: i64,
    current_weight: i64,
    items: VecDeque<T>,
}

impl<T> FairScheduler<T> {
    /// A scheduler admitting at most `max_in_flight` items concurrently.
    pub fn new(max_in_flight: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                tenants: BTreeMap::new(),
                max_in_flight: max_in_flight.max(1),
                in_flight: 0,
            })),
        }
    }

    /// Enqueue `item` for `tenant`. The tenant's relative `weight` (min 1) is set on
    /// its first submission and reused thereafter.
    pub fn submit(&self, tenant: &str, weight: u32, item: T) {
        let mut inner = self.inner.lock().expect("scheduler mutex poisoned");
        inner
            .tenants
            .entry(tenant.to_string())
            .or_insert_with(|| TenantQueue {
                weight: weight.max(1) as i64,
                current_weight: 0,
                items: VecDeque::new(),
            })
            .items
            .push_back(item);
    }

    /// Admit the next item if a concurrency slot is free, choosing the tenant by
    /// smooth weighted round-robin. Returns `(tenant, item)`, or `None` when at the
    /// concurrency bound or nothing is queued. Each `Some` consumes a slot until a
    /// matching [`Self::complete`].
    pub fn poll(&self) -> Option<(String, T)> {
        let mut inner = self.inner.lock().expect("scheduler mutex poisoned");
        if inner.in_flight >= inner.max_in_flight {
            return None;
        }
        let total: i64 = inner
            .tenants
            .values()
            .filter(|q| !q.items.is_empty())
            .map(|q| q.weight)
            .sum();
        if total == 0 {
            return None;
        }
        // Credit every tenant that has queued work...
        for q in inner.tenants.values_mut() {
            if !q.items.is_empty() {
                q.current_weight += q.weight;
            }
        }
        // ...and admit the one with the highest running credit.
        let best = inner
            .tenants
            .iter()
            .filter(|(_, q)| !q.items.is_empty())
            .max_by_key(|(_, q)| q.current_weight)
            .map(|(name, _)| name.clone())?;
        let q = inner.tenants.get_mut(&best).expect("tenant exists");
        q.current_weight -= total;
        let item = q.items.pop_front().expect("non-empty queue");
        inner.in_flight += 1;
        Some((best, item))
    }

    /// Release a slot held by a previously admitted item.
    pub fn complete(&self) {
        let mut inner = self.inner.lock().expect("scheduler mutex poisoned");
        inner.in_flight = inner.in_flight.saturating_sub(1);
    }

    /// Items currently admitted (in flight).
    pub fn in_flight(&self) -> usize {
        self.inner
            .lock()
            .expect("scheduler mutex poisoned")
            .in_flight
    }

    /// Items queued but not yet admitted, across all tenants.
    pub fn pending(&self) -> usize {
        self.inner
            .lock()
            .expect("scheduler mutex poisoned")
            .tenants
            .values()
            .map(|q| q.items.len())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drain the scheduler (admitting + completing immediately) into the admission
    /// order of tenant names.
    fn drain_order(sched: &FairScheduler<u32>) -> Vec<String> {
        let mut order = Vec::new();
        while let Some((tenant, _)) = sched.poll() {
            order.push(tenant);
            sched.complete();
        }
        order
    }

    fn counts(order: &[String]) -> BTreeMap<String, usize> {
        let mut c = BTreeMap::new();
        for t in order {
            *c.entry(t.clone()).or_insert(0) += 1;
        }
        c
    }

    #[test]
    fn equal_weights_admit_round_robin() {
        let sched = FairScheduler::new(100);
        for i in 0..3 {
            sched.submit("a", 1, i);
            sched.submit("b", 1, i);
            sched.submit("c", 1, i);
        }
        let order = drain_order(&sched);
        // Every tenant admitted the same number of times...
        let c = counts(&order);
        assert_eq!(c["a"], 3);
        assert_eq!(c["b"], 3);
        assert_eq!(c["c"], 3);
        // ...and interleaved: no tenant appears twice within any window of 3.
        for w in order.windows(3) {
            assert_eq!(
                w.iter().collect::<std::collections::BTreeSet<_>>().len(),
                3,
                "tenants should round-robin, got window {w:?}"
            );
        }
    }

    #[test]
    fn weights_bias_admission_proportionally() {
        let sched = FairScheduler::new(100);
        for i in 0..6 {
            sched.submit("light", 1, i);
            sched.submit("heavy", 2, i);
        }
        let c = counts(&drain_order(&sched));
        // Both drain fully (6 each), but heavy is admitted earlier/more often: over
        // the first 6 admissions, heavy should lead ~2:1.
        assert_eq!(c["light"], 6);
        assert_eq!(c["heavy"], 6);

        let sched2 = FairScheduler::new(100);
        for i in 0..100 {
            sched2.submit("light", 1, i);
            sched2.submit("heavy", 2, i);
        }
        // Admit only the first 30 (without completing past the bound is irrelevant
        // here since we complete each); check the prefix ratio.
        let mut prefix = Vec::new();
        for _ in 0..30 {
            let (t, _) = sched2.poll().unwrap();
            prefix.push(t);
            sched2.complete();
        }
        let pc = counts(&prefix);
        assert!(
            pc["heavy"] > pc["light"],
            "heavier tenant should be admitted more: {pc:?}"
        );
        assert!(
            pc["heavy"] as f64 >= 1.7 * pc["light"] as f64,
            "expected ~2:1 weighting, got {pc:?}"
        );
    }

    #[test]
    fn respects_concurrency_bound() {
        let sched = FairScheduler::new(2);
        for i in 0..5 {
            sched.submit("a", 1, i);
        }
        assert!(sched.poll().is_some());
        assert!(sched.poll().is_some());
        assert_eq!(sched.in_flight(), 2);
        assert!(sched.poll().is_none(), "at the concurrency bound");

        sched.complete();
        assert!(sched.poll().is_some(), "a freed slot admits the next");
        assert_eq!(sched.in_flight(), 2);
        assert_eq!(sched.pending(), 2);
    }

    #[test]
    fn empty_scheduler_polls_none() {
        let sched: FairScheduler<u32> = FairScheduler::new(4);
        assert!(sched.poll().is_none());
        sched.submit("a", 1, 7);
        assert_eq!(sched.poll(), Some(("a".to_string(), 7)));
        assert!(sched.poll().is_none());
    }

    #[tokio::test]
    async fn drives_work_through_a_sandbox_pool() {
        use crate::{NativeSandbox, ResourceLimits, Sandbox, SandboxCommand, SandboxPool};
        use std::time::Duration;

        let pool = SandboxPool::new(
            2,
            2,
            Box::new(|| Box::new(NativeSandbox::new(Duration::from_secs(5))) as Box<dyn Sandbox>),
        );
        let sched: FairScheduler<String> = FairScheduler::new(2);
        for i in 0..2 {
            sched.submit("a", 1, format!("a{i}"));
            sched.submit("b", 1, format!("b{i}"));
        }

        // Admit fairly, run each admitted item on a pooled sandbox, then release.
        let mut ran = Vec::new();
        while let Some((tenant, tag)) = sched.poll() {
            let sb = pool.acquire().await.unwrap();
            let cmd = SandboxCommand {
                program: "echo".into(),
                args: vec![tag.clone()],
                workdir: ".".into(),
                env: vec![],
                limits: ResourceLimits {
                    timeout: Duration::from_secs(5),
                    ..ResourceLimits::default()
                },
            };
            let out = sb.execute(&cmd).await.unwrap();
            assert!(out.stdout.contains(&tag));
            ran.push(tenant);
            sched.complete();
        }

        assert_eq!(ran.len(), 4, "all submitted work ran");
        assert_eq!(ran.iter().filter(|t| *t == "a").count(), 2);
        assert_eq!(ran.iter().filter(|t| *t == "b").count(), 2);
    }
}
