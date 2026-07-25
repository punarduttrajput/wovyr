//! Wovyr plugin authoring SDK (RM-AIM-P3 ECO-302).
//!
//! Typed entry points for writing an Wovyr **tool capability** as a
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
//!     wovyr_plugin_sdk::run_tool(|req: Request| -> Result<Response, String> {
//!         let name = req.name.unwrap_or_else(|| "world".to_string());
//!         Ok(Response { greeting: format!("Hello, {name}!") })
//!     })
//! }
//! ```
//!
//! Built with `cargo build --release --target wasm32-wasip1` (or, from a
//! scaffolded project, `wovyr plugin build` — which also stages the package and
//! computes artifact digests). Untyped tools take and return
//! [`serde_json::Value`] through the same entry point.
//!
//! Secrets a capability's manifest declares (`secret:read:<name>` permissions)
//! arrive either inside the stdin request envelope (`{"__wovyr_abi": 1,
//! "params": …, "secrets": …}` — the platform's default request-scoped channel,
//! SEC-302, which keeps them out of this process's environment) or as
//! `WOVYR_SECRET_*` environment variables (the legacy channel). Read them with
//! [`secret`], which handles both transparently; [`run_tool`] likewise accepts
//! both the envelope and the bare-parameters stdin shape, so a tool never needs
//! to know which channel the platform used.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::io::Read;
use std::process::ExitCode;
use std::sync::OnceLock;

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
    // Unwrap the request-scoped secret envelope (SEC-302), if present, before the
    // handler sees the parameters; the secrets become readable via `secret`.
    let input = match unwrap_envelope(&input) {
        Ok((params, secrets)) => {
            if let Some(secrets) = secrets {
                let _ = stdin_secrets().set(secrets);
            }
            params
        }
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
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

/// The secrets delivered over the stdin envelope for this invocation (SEC-302),
/// keyed by their `WOVYR_SECRET_*` name. Set once by [`run_tool`]; a tool
/// process handles exactly one request, so there is nothing to reset.
fn stdin_secrets() -> &'static OnceLock<HashMap<String, String>> {
    static SECRETS: OnceLock<HashMap<String, String>> = OnceLock::new();
    &SECRETS
}

/// Split a raw stdin document into `(parameter JSON, envelope secrets)`.
///
/// The platform's request-scoped secret channel (SEC-302) wraps the parameters
/// as `{"__wovyr_abi": 1, "params": …, "secrets": {…}}`; anything without the
/// `__wovyr_abi` marker is the bare-parameters shape and passes through
/// untouched. Pure, so it is unit-testable without a stdin pipe; an envelope
/// from an ABI newer than this SDK understands fails closed.
pub fn unwrap_envelope(input: &str) -> Result<(String, Option<HashMap<String, String>>), String> {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(input) else {
        // Not JSON at all — let `respond` produce its normal parse error.
        return Ok((input.to_string(), None));
    };
    let Some(abi) = doc.get("__wovyr_abi") else {
        return Ok((input.to_string(), None));
    };
    if abi.as_u64() != Some(1) {
        return Err(format!(
            "request envelope ABI {abi} is newer than this SDK understands (max 1)"
        ));
    }
    let params = doc.get("params").cloned().unwrap_or(serde_json::json!({}));
    let params = serde_json::to_string(&params)
        .map_err(|e| format!("could not re-encode envelope params: {e}"))?;
    let secrets = doc.get("secrets").and_then(|s| s.as_object()).map(|map| {
        map.iter()
            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
            .collect()
    });
    Ok((params, secrets))
}

/// Read a secret the platform delivered for this capability — from the stdin
/// request envelope (the default channel) or the legacy `WOVYR_SECRET_*`
/// environment variable, whichever the platform used. The capability's
/// manifest must declare the matching `secret:read:<name>` permission, and the
/// run must be tenant-scoped — otherwise nothing is delivered and this returns
/// `None`.
pub fn secret(name: &str) -> Option<String> {
    let key = secret_env_var(name);
    if let Some(map) = stdin_secrets().get()
        && let Some(value) = map.get(&key)
    {
        return Some(value.clone());
    }
    std::env::var(key).ok()
}

/// The environment variable a named secret is injected as:
/// `WOVYR_SECRET_<UPPER_SNAKE>` (non-alphanumeric characters become `_`) —
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
    format!("WOVYR_SECRET_{upper}")
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
        let out = respond(r#"{"name": "Wovyr"}"#, greet).unwrap();
        assert_eq!(out, r#"{"greeting":"Hello, Wovyr!"}"#);
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
        // Mirrors wovyr-plugin's `resolve_secret_env` / `secret_env_var`.
        assert_eq!(secret_env_var("github-token"), "WOVYR_SECRET_GITHUB_TOKEN");
        assert_eq!(secret_env_var("db.url"), "WOVYR_SECRET_DB_URL");
        assert_eq!(secret_env_var("plain"), "WOVYR_SECRET_PLAIN");
    }

    /// SEC-302: the request-scoped envelope splits into params + secrets; the
    /// bare-parameters shape passes through untouched.
    #[test]
    fn envelope_unwraps_params_and_secrets() {
        let (params, secrets) = unwrap_envelope(
            r#"{"__wovyr_abi": 1, "params": {"name": "Wovyr"},
                "secrets": {"WOVYR_SECRET_API_TOKEN": "s3cr3t"}}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&params).unwrap(),
            json!({"name": "Wovyr"})
        );
        assert_eq!(
            secrets.unwrap().get("WOVYR_SECRET_API_TOKEN").unwrap(),
            "s3cr3t"
        );

        // Bare params (legacy env channel, or no secrets): untouched, no secrets.
        let (params, secrets) = unwrap_envelope(r#"{"name": "Wovyr"}"#).unwrap();
        assert_eq!(params, r#"{"name": "Wovyr"}"#);
        assert!(secrets.is_none());

        // Non-JSON passes through for `respond` to reject with its normal error.
        let (params, secrets) = unwrap_envelope("not json").unwrap();
        assert_eq!(params, "not json");
        assert!(secrets.is_none());
    }

    /// A future envelope ABI this SDK doesn't understand fails closed instead of
    /// being misread as parameters.
    #[test]
    fn newer_envelope_abi_fails_closed() {
        let err = unwrap_envelope(r#"{"__wovyr_abi": 2, "params": {}}"#).unwrap_err();
        assert!(err.contains("newer than this SDK"), "{err}");
    }

    /// End to end through `respond`: envelope params reach the handler, and the
    /// stashed secrets are readable via `secret` without any env var existing.
    #[test]
    fn envelope_secrets_are_readable_via_secret_without_env() {
        let (params, secrets) = unwrap_envelope(
            r#"{"__wovyr_abi": 1, "params": {"name": "Sec"},
                "secrets": {"WOVYR_SECRET_ENVELOPE_ONLY_TEST": "from-stdin"}}"#,
        )
        .unwrap();
        let _ = stdin_secrets().set(secrets.unwrap());

        assert!(std::env::var("WOVYR_SECRET_ENVELOPE_ONLY_TEST").is_err());
        assert_eq!(secret("envelope-only-test").as_deref(), Some("from-stdin"));

        let out = respond(&params, greet).unwrap();
        assert_eq!(out, r#"{"greeting":"Hello, Sec!"}"#);
    }

    #[test]
    fn secret_reads_the_injected_env_var() {
        // Set/remove is safe here: the name is unique to this test.
        unsafe { std::env::set_var("WOVYR_SECRET_SDK_TEST_ONLY", "s3cr3t") };
        assert_eq!(secret("sdk-test-only").as_deref(), Some("s3cr3t"));
        unsafe { std::env::remove_var("WOVYR_SECRET_SDK_TEST_ONLY") };
        assert_eq!(secret("sdk-test-only"), None);
    }
}
