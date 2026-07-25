//! Redis-backed [`BreakerKv`] for fleet-wide circuit breaking.
//!
//! Implements the shared-state primitive behind [`SharedCircuitBreaker`](crate::resilience::SharedCircuitBreaker)
//! using Redis' atomic `INCR` and TTL-bearing `SET`, so concurrent gateways share a
//! single source of provider-health truth ([resilience §6](../../docs/05-llm-gateway/resilience.md)).
//! Enabled by the `redis` cargo feature.

use crate::resilience::BreakerKv;
use async_trait::async_trait;
use redis::aio::MultiplexedConnection;
use wovyr_common::{Error, Result};

fn redis_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::provider(format!("redis {context}: {e}"))
}

/// A [`BreakerKv`] over a Redis connection. The multiplexed connection is cheap to
/// clone and pipelines concurrent commands over one socket.
pub struct RedisKv {
    conn: MultiplexedConnection,
}

impl RedisKv {
    /// Connect to Redis at `url` (e.g. `redis://127.0.0.1:6379`).
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url).map_err(|e| redis_err("open", e))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| redis_err("connect", e))?;
        Ok(Self { conn })
    }
}

#[async_trait]
impl BreakerKv for RedisKv {
    async fn incr(&self, key: &str, ttl_ms: u64) -> Result<i64> {
        let mut conn = self.conn.clone();
        let n: i64 = redis::cmd("INCR")
            .arg(key)
            .query_async(&mut conn)
            .await
            .map_err(|e| redis_err("incr", e))?;
        // Set the lifetime only when we created the key (first increment).
        if n == 1 {
            let _: () = redis::cmd("PEXPIRE")
                .arg(key)
                .arg(ttl_ms)
                .query_async(&mut conn)
                .await
                .map_err(|e| redis_err("pexpire", e))?;
        }
        Ok(n)
    }

    async fn get_i64(&self, key: &str) -> Result<Option<i64>> {
        let mut conn = self.conn.clone();
        let v: Option<i64> = redis::cmd("GET")
            .arg(key)
            .query_async(&mut conn)
            .await
            .map_err(|e| redis_err("get", e))?;
        Ok(v)
    }

    async fn set_i64(&self, key: &str, value: i64, ttl_ms: u64) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("PX")
            .arg(ttl_ms)
            .query_async(&mut conn)
            .await
            .map_err(|e| redis_err("set", e))?;
        Ok(())
    }

    async fn del(&self, keys: &[&str]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.clone();
        let mut cmd = redis::cmd("DEL");
        for k in keys {
            cmd.arg(*k);
        }
        let _: i64 = cmd
            .query_async(&mut conn)
            .await
            .map_err(|e| redis_err("del", e))?;
        Ok(())
    }
}
