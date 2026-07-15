//! The generative-UI frame protocol ([PRD-005](../../docs/01-product/prd-generative-ui-runtime.md)
//! workstream UIP-1xx, [ADR-0011](../../docs/17-adr/ADR-0011-generative-ui-repositioning.md)).
//!
//! A [`UiFrame`] is the *only* shape an agent-generated interface is ever
//! rendered from: a versioned, declarative, JSON-serializable tree over a
//! **constrained component vocabulary** ([`UiNode`]). There is deliberately no
//! raw-HTML, script, or style-injection node — the load-bearing security
//! decision (ADR-0011 §2.4) is that most deception and injection classes are
//! *structurally impossible* rather than detected after the fact.
//!
//! Fail-closed throughout, the workspace stance: an unknown node type, an
//! unknown field, a schema version newer than this runtime understands
//! (UIP-106, the MIG-A1 version-skew posture), or an out-of-vocabulary
//! decision (HIL-302) is a hard error — never a best-effort render.
//!
//! This crate is pure protocol: no I/O, no clock, no policy. Policy lives in
//! `apex-ui-guard`; transport/persistence live with the platform (server).

mod decision;
mod frame;

pub use decision::{UiDecision, validate_decision};
pub use frame::{
    ActionClass, KeyValueEntry, MAX_DEPTH, MAX_NODES, Provenance, SCHEMA_VERSION, SelectOption,
    TextStyle, Tone, UiFrame, UiNode,
};
