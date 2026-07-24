//! Shared `~/.apex` directory layout, `APEX_*` env-var reading, and KMS/
//! secrets backend construction for `apex-server` and `apex-cli` (RM-GA-P4
//! HLTH-903).
//!
//! Both binaries used to reimplement `HOME`/`USERPROFILE` resolution and
//! backend-construction logic (KMS root key + tenant catalog, secret-vault
//! encrypted-vs-plaintext selection) independently and identically — a
//! drifted edit to either copy would silently fork what "the KMS root key"
//! or "the secrets file" means between the two processes. This crate is the
//! one implementation both consume.
//!
//! Deliberately does **not** centralize every `~/.apex`-adjacent decision:
//! the CLI's tiered Postgres/Qdrant memory backend and the server's
//! marketplace curation policy (`policy.json`) have no counterpart on the
//! other side, so there's no duplication to remove there; and the
//! plugin-engine/registry object construction itself stays with each
//! binary's own route/command modules, which only call into
//! [`paths`]/[`kms`]/[`secrets`] for the pieces that were genuinely
//! duplicated. See
//! `docs/18-roadmap/v1.0/phase4-contract-operability-tickets.md` (HLTH-903).

pub mod audit;
pub mod env;
pub mod kms;
pub mod paths;
pub mod root;
pub mod secrets;

pub use root::apex_dir;
