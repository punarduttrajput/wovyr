//! Sandbox warm pooling: a bounded set of pre-warmed, reusable sandbox instances.
//!
//! Constructing (and, for container/microVM backends, starting) a sandbox per tool
//! call adds cold-start latency ([sandbox runtime §warm pooling](../../docs/07-tool-runtime/sandbox-runtime.md)).
//! A [`SandboxPool`] keeps up to `max_size` instances alive, pre-warming `warm_count`
//! of them, and hands an idle one to each caller — reusing it on return instead of
//! building a fresh sandbox. Concurrency is bounded by a semaphore, so at most
//! `max_size` executions run at once; further `acquire`s wait for a return.
//!
//! The pooling mechanics (pre-warm, reuse, bounded concurrency, capped idle set) are
//! backend-agnostic and deterministic. Persistent *warm sessions* inside a single
//! long-running container/microVM are a separate concern that needs a session-capable
//! backend; this pool reuses constructed sandbox handles.

use crate::sandbox::{CommandOutcome, Sandbox, SandboxBackend, SandboxCommand, SandboxError};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Builds a fresh sandbox instance for the pool.
pub type SandboxFactory = Box<dyn Fn() -> Box<dyn Sandbox> + Send + Sync>;

struct PoolInner {
    idle: Mutex<Vec<Box<dyn Sandbox>>>,
    factory: SandboxFactory,
    permits: Arc<Semaphore>,
    max_size: usize,
    created: AtomicUsize,
    reused: AtomicUsize,
}

/// A bounded pool of reusable sandbox instances.
#[derive(Clone)]
pub struct SandboxPool {
    inner: Arc<PoolInner>,
}

impl SandboxPool {
    /// Build a pool holding at most `max_size` instances, pre-warming `warm_count`
    /// of them (clamped to `max_size`). `factory` constructs a fresh sandbox.
    pub fn new(max_size: usize, warm_count: usize, factory: SandboxFactory) -> Self {
        let max_size = max_size.max(1);
        let warm_count = warm_count.min(max_size);

        let mut idle: Vec<Box<dyn Sandbox>> = Vec::with_capacity(warm_count);
        for _ in 0..warm_count {
            idle.push(factory());
        }

        let inner = PoolInner {
            idle: Mutex::new(idle),
            factory,
            permits: Arc::new(Semaphore::new(max_size)),
            max_size,
            created: AtomicUsize::new(warm_count),
            reused: AtomicUsize::new(0),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Check out a sandbox, waiting for a free slot when the pool is at capacity.
    /// Returns a guard that returns the instance to the pool on drop.
    pub async fn acquire(&self) -> Result<PooledSandbox, SandboxError> {
        let permit = self
            .inner
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| SandboxError::Internal("sandbox pool is closed".into()))?;
        Ok(self.checkout(permit))
    }

    /// Check out a sandbox without waiting; `None` when the pool is at capacity.
    pub fn try_acquire(&self) -> Option<PooledSandbox> {
        let permit = self.inner.permits.clone().try_acquire_owned().ok()?;
        Some(self.checkout(permit))
    }

    /// Take an idle instance (reuse) or build a fresh one, under a held permit.
    fn checkout(&self, permit: OwnedSemaphorePermit) -> PooledSandbox {
        let sandbox = {
            let mut idle = self.inner.idle.lock().expect("pool mutex poisoned");
            idle.pop()
        };
        let sandbox = match sandbox {
            Some(sb) => {
                self.inner.reused.fetch_add(1, Ordering::Relaxed);
                sb
            }
            None => {
                self.inner.created.fetch_add(1, Ordering::Relaxed);
                (self.inner.factory)()
            }
        };
        PooledSandbox {
            inner: self.inner.clone(),
            sandbox: Some(sandbox),
            _permit: permit,
        }
    }

    /// Maximum concurrent in-use instances.
    pub fn max_size(&self) -> usize {
        self.inner.max_size
    }

    /// Number of warm instances currently idle.
    pub fn idle(&self) -> usize {
        self.inner.idle.lock().expect("pool mutex poisoned").len()
    }

    /// Instances currently checked out.
    pub fn in_use(&self) -> usize {
        self.inner.max_size - self.inner.permits.available_permits()
    }

    /// Total instances ever constructed by the factory (incl. pre-warmed).
    pub fn created(&self) -> usize {
        self.inner.created.load(Ordering::Relaxed)
    }

    /// Number of checkouts served by reusing a warm instance.
    pub fn reused(&self) -> usize {
        self.inner.reused.load(Ordering::Relaxed)
    }
}

/// A checked-out sandbox. Executes like a [`Sandbox`] and returns itself to the pool
/// when dropped (releasing its concurrency slot).
pub struct PooledSandbox {
    inner: Arc<PoolInner>,
    sandbox: Option<Box<dyn Sandbox>>,
    _permit: OwnedSemaphorePermit,
}

impl PooledSandbox {
    /// The backend of the underlying sandbox.
    pub fn backend(&self) -> SandboxBackend {
        self.sandbox.as_ref().expect("sandbox present").backend()
    }

    /// Execute a command in the pooled sandbox.
    pub async fn execute(&self, cmd: &SandboxCommand) -> Result<CommandOutcome, SandboxError> {
        self.sandbox
            .as_ref()
            .expect("sandbox present")
            .execute(cmd)
            .await
    }
}

impl Drop for PooledSandbox {
    fn drop(&mut self) {
        if let Some(sandbox) = self.sandbox.take() {
            let mut idle = self.inner.idle.lock().expect("pool mutex poisoned");
            // Keep the instance warm for reuse, but never exceed the pool's bound.
            if idle.len() < self.inner.max_size {
                idle.push(sandbox);
            }
        }
        // `_permit` drops here, freeing the slot for a waiting `acquire`.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::NativeSandbox;
    use std::time::Duration;

    fn native_factory() -> SandboxFactory {
        Box::new(|| Box::new(NativeSandbox::new(Duration::from_secs(5))) as Box<dyn Sandbox>)
    }

    fn echo() -> SandboxCommand {
        SandboxCommand {
            program: "echo".into(),
            args: vec!["hi".into()],
            workdir: ".".into(),
            limits: crate::sandbox::ResourceLimits {
                timeout: Duration::from_secs(5),
                ..Default::default()
            },
        }
    }

    #[test]
    fn prewarms_up_to_max_size() {
        let pool = SandboxPool::new(2, 5, native_factory());
        assert_eq!(pool.max_size(), 2, "warm count is clamped to max_size");
        assert_eq!(pool.idle(), 2, "two instances pre-warmed");
        assert_eq!(pool.created(), 2);
    }

    #[tokio::test]
    async fn reuses_warm_instances_instead_of_building_new() {
        let pool = SandboxPool::new(2, 1, native_factory());
        assert_eq!(pool.created(), 1);

        // First checkout reuses the pre-warmed instance.
        {
            let sb = pool.acquire().await.unwrap();
            assert_eq!(sb.backend(), SandboxBackend::Native);
            assert_eq!(pool.idle(), 0, "the warm instance is checked out");
            assert_eq!(pool.in_use(), 1);
        }
        // Dropped → returned to the pool.
        assert_eq!(pool.idle(), 1, "returned for reuse");
        assert_eq!(pool.in_use(), 0);

        // Second checkout reuses again — no new instance built.
        let _sb = pool.acquire().await.unwrap();
        assert_eq!(pool.reused(), 2);
        assert_eq!(pool.created(), 1, "no fresh construction needed");
    }

    #[tokio::test]
    async fn builds_on_demand_up_to_capacity_then_blocks() {
        let pool = SandboxPool::new(2, 0, native_factory());
        let a = pool.acquire().await.unwrap();
        let b = pool.acquire().await.unwrap();
        assert_eq!(pool.created(), 2, "built two on demand");
        assert_eq!(pool.in_use(), 2);

        // At capacity: a non-blocking acquire fails.
        assert!(pool.try_acquire().is_none(), "pool is exhausted");

        // Releasing one frees a slot.
        drop(a);
        assert!(pool.try_acquire().is_some(), "slot freed after return");
        drop(b);
    }

    #[tokio::test]
    async fn pooled_sandbox_executes() {
        let pool = SandboxPool::new(1, 1, native_factory());
        let sb = pool.acquire().await.unwrap();
        let out = sb.execute(&echo()).await.unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.contains("hi"));
    }
}
