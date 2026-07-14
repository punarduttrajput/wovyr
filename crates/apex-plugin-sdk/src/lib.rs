//! Apex plugin authoring SDK (RM-AIM-P3 ECO-302).
//!
//! Typed entry points for writing an Apex **tool capability** as a
//! `wasm32-wasip1` command, without hand-rolling the platform's capability ABI
//! ([sandbox loading model](../../docs/08-plugin-sdk/sandbox.md)): the loader
//! writes the tool-call parameters as JSON to the guest's **stdin** and reads
//! the response as JSON from its **stdout** — exit `0` with parseable JSON is
//! success, anything else is a tool error whose detail is taken from stderr.
//!
//! A complete tool is a `main` that hands a typed handler to [`run_tool`]:
//!
//! ```no_run
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Deserialize)]
//! struct Request {
//!     name: Option<String>,
//! }
//!
//! #[derive(Serialize)]
//! struct Response {
//!     greeting: String,
//! }
//!
//! fn main() -> std::process::ExitCode {
//!     apex_plugin_sdk::run_tool(|req: Request| -> Result<Response, String> {
//!         let name = req.name.unwrap_or_else(|| "world".to_string());
//!         Ok(Response { greeting: format!("Hello, {name}!") })
//!     })
//! }
//! ```
//!
//! Built with `cargo build --release --target wasm32-wasip1` (or, from a
//! scaffolded project, `apex plugin build` — which also stages the package and
//! computes artifact digests). Untyped tools take and return
//! [`serde_json::Value`] through the same entry point.
//!
//! Secrets a capability's manifest declares (`secret:read:<name>` permissions)
//! are injected by the platform as environment variables; read them with
//! [`secret`] rather than hand-mangling names.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::Read;
use std::process::ExitCode;

/// Run a tool capability: read the request JSON from stdin, invoke `handler`,
/// and write the response JSON to stdout. Returns the process exit code —
/// hand it straight back from `main`.
///
/// A handler error (or unparseable input) prints its message to **stderr**
/// and exits non-zero, which the platform surfaces as the tool failure's
/// detail — never as a mangled success payload.
pub fn run_tool<Req, Resp, E>(handler: impl FnOnce(Req) -> Result<Resp, E>) -> ExitCode
where
    Req: DeserializeOwned,
    Resp: Serialize,
    E: std::fmt::Display,
{
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("could not read the request from stdin: {e}");
        return ExitCode::FAILURE;
    }
    match respond(&input, handler) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// The pure request→response core behind [`run_tool`] — no process I/O, so a
/// handler is unit-testable on the host without a wasm build or a stdin pipe.
///
/// Empty input is treated as the conventional no-argument call (`{}`).
pub fn respond<Req, Resp, E>(
    input: &str,
    handler: impl FnOnce(Req) -> Result<Resp, E>,
) -> Result<String, String>
where
    Req: DeserializeOwned,
    Resp: Serialize,
    E: std::fmt::Display,
{
    let input = if input.trim().is_empty() { "{}" } else { input };
    let request: Req =
        serde_json::from_str(input).map_err(|e| format!("invalid request parameters: {e}"))?;
    let response = handler(request).map_err(|e| e.to_string())?;
    serde_json::to_string(&response).map_err(|e| format!("could not encode the response: {e}"))
}

/// Read a secret the platform injected for this capability. The capability's
/// manifest must declare the matching `secret:read:<name>` permission, and the
/// run must be tenant-scoped — otherwise nothing is injected and this returns
/// `None`.
pub fn secret(name: &str) -> Option<String> {
    std::env::var(secret_env_var(name)).ok()
}

/// The environment variable a named secret is injected as:
/// `APEX_SECRET_<UPPER_SNAKE>` (non-alphanumeric characters become `_`) —
/// mirrors the platform's `resolve_secret_env` mangling exactly
/// ([secret-management §5](../../docs/13-security/secret-management.md#5-injection-into-tools--plugins)).
pub fn secret_env_var(name: &str) -> String {
    let upper: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("APEX_SECRET_{upper}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};

    #[derive(Deserialize)]
    struct Req {
        name: Option<String>,
    }

    #[derive(Serialize)]
    struct Resp {
        greeting: String,
    }

    fn greet(req: Req) -> Result<Resp, String> {
        Ok(Resp {
            greeting: format!("Hello, {}!", req.name.unwrap_or_else(|| "world".into())),
        })
    }

    #[test]
    fn typed_round_trip() {
        let out = respond(r#"{"name": "Apex"}"#, greet).unwrap();
        assert_eq!(out, r#"{"greeting":"Hello, Apex!"}"#);
    }

    #[test]
    fn empty_input_is_the_no_argument_call() {
        let out = respond("", greet).unwrap();
        assert_eq!(out, r#"{"greeting":"Hello, world!"}"#);
    }

    #[test]
    fn invalid_input_and_handler_errors_are_clear_messages() {
        let err = respond("not json", greet).unwrap_err();
        assert!(err.contains("invalid request parameters"), "{err}");

        let err = respond("{}", |_: Value| -> Result<Value, String> {
            Err("the upstream API said no".into())
        })
        .unwrap_err();
        assert_eq!(err, "the upstream API said no");
    }

    #[test]
    fn untyped_value_handlers_work_through_the_same_entry_point() {
        let out = respond(r#"{"n": 2}"#, |v: Value| -> Result<Value, String> {
            Ok(json!({ "doubled": v["n"].as_i64().unwrap_or(0) * 2 }))
        })
        .unwrap();
        assert_eq!(out, r#"{"doubled":4}"#);
    }

    #[test]
    fn secret_env_var_mangling_matches_the_platform() {
        // Mirrors apex-plugin's `resolve_secret_env` / `secret_env_var`.
        assert_eq!(secret_env_var("github-token"), "APEX_SECRET_GITHUB_TOKEN");
        assert_eq!(secret_env_var("db.url"), "APEX_SECRET_DB_URL");
        assert_eq!(secret_env_var("plain"), "APEX_SECRET_PLAIN");
    }

    #[test]
    fn secret_reads_the_injected_env_var() {
        // Set/remove is safe here: the name is unique to this test.
        unsafe { std::env::set_var("APEX_SECRET_SDK_TEST_ONLY", "s3cr3t") };
        assert_eq!(secret("sdk-test-only").as_deref(), Some("s3cr3t"));
        unsafe { std::env::remove_var("APEX_SECRET_SDK_TEST_ONLY") };
        assert_eq!(secret("sdk-test-only"), None);
    }
}
