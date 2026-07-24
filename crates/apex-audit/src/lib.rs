//! # apex-audit — tamper-evident audit logging
//!
//! Implements [Audit Logging](../../docs/13-security/audit.md): the security-grade record
//! of *who did what, when, and with what outcome*, distinct from operational logs.
//!
//! - [`AuditEvent`] — the structured record ([§3](../../docs/13-security/audit.md#3-record-schema)):
//!   actor (principal/type/tenant), action, resource (referenced **by id, never value**),
//!   outcome, reason, and a correlation `request_id`.
//! - [`AuditLog`] — an append-only, **hash-chained** log
//!   ([§4](../../docs/13-security/audit.md#4-integrity-tamper-evidence)): each entry commits
//!   to the prior entry's hash, so deletion/modification is detectable via
//!   [`AuditLog::verify`]. Backed by an [`AuditSink`] ([`InMemoryAuditSink`] /
//!   [`FileAuditSink`]). Opened with a key
//!   ([`AuditLog::open_keyed`], SEC-403) the chain becomes a **keyed HMAC** an actor
//!   with write access can't forge, plus a durable **head anchor** so `verify` catches
//!   tail truncation as well as interior edits; [`NotarizationHook`] is the landed
//!   interface for an optional external (WORM/transparency-log) anchor.
//!
//! Timestamps are supplied on the event (read at the request boundary), so the chain math
//! is deterministic. Depends only on `apex-common`, keeping the dependency spine
//! one-directional.

mod event;
mod log;

pub use event::{Actor, ActorType, AuditEvent, Outcome, Resource};
pub use log::{
    AuditEntry, AuditFilter, AuditLog, AuditPage, AuditSink, FileAuditSink, InMemoryAuditSink,
    NotarizationHook, NotarizedHead,
};
