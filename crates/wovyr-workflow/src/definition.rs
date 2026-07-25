//! Workflow definition (DSL) and its compiled DAG.
//!
//! Parses the YAML form from the
//! [Workflow DSL spec](../../docs/03-workflow-engine/workflow-dsl.md) — the
//! `apiVersion`/`kind`/`metadata`/`spec` manifest with `activities` and
//! `transitions` — and validates it into an executable DAG (the spec's WIR,
//! [§25](../../docs/03-workflow-engine/workflow-dsl.md)). Validation enforces the
//! rules in [§24](../../docs/03-workflow-engine/workflow-dsl.md): unique ids,
//! transitions referencing real activities, and no cycles.

use crate::retry::RetryPolicy;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use wovyr_common::{Error, Result};

/// A parsed, validated workflow definition.
#[derive(Debug, Clone, Deserialize)]
pub struct Definition {
    /// Manifest API version, e.g. `workflow.wovyr.io/v1`.
    #[serde(rename = "apiVersion", default)]
    pub api_version: Option<String>,
    /// Manifest kind; expected to be `Workflow`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Identifying metadata.
    pub metadata: Metadata,
    /// Workflow body.
    pub spec: Spec,
    /// Stable content hash of the source the definition was parsed from, used to
    /// **pin** an execution to the exact definition it started with: `resume`
    /// rejects a definition whose hash (or version) drifted, so in-flight
    /// executions never silently run a changed DAG
    /// ([gap closure G7](../../docs/03-workflow-engine/temporal-gap-analysis.md#g7--in-flight-definition-versioning)).
    /// Set by [`from_yaml`](Self::from_yaml)/[`from_file`](Self::from_file);
    /// `None` for definitions built another way.
    #[serde(skip, default)]
    source_hash: Option<String>,
}

/// Workflow identity.
#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    /// Workflow name.
    pub name: String,
    /// Semantic version; defaults to `0.0.0` if omitted.
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_version() -> String {
    "0.0.0".to_string()
}

/// Workflow body: variables, activities, transitions, and a default retry policy.
#[derive(Debug, Clone, Deserialize)]
pub struct Spec {
    /// Initial workflow variables (mutable during execution).
    #[serde(default)]
    pub variables: BTreeMap<String, Value>,
    /// The activities (DAG nodes).
    pub activities: Vec<ActivityDef>,
    /// Directed edges `from -> to` between activity ids.
    #[serde(default)]
    pub transitions: Vec<Transition>,
    /// Default retry policy applied to activities without their own.
    #[serde(default)]
    pub retry: Option<RetryPolicy>,
}

/// A single activity (DAG node).
#[derive(Debug, Clone, Deserialize)]
pub struct ActivityDef {
    /// Unique id within the workflow.
    pub id: String,
    /// Activity type (`function`, `tool`, `ai`, …) — interpreted by the executor.
    #[serde(rename = "type")]
    pub activity_type: String,
    /// Human-readable name (e.g. the tool id for `tool` activities).
    #[serde(default)]
    pub name: Option<String>,
    /// Static inputs passed to the executor.
    #[serde(default)]
    pub inputs: Value,
    /// Per-activity retry override.
    #[serde(default)]
    pub retry: Option<RetryPolicy>,
    /// Id of the activity that compensates (rolls back) this one if the workflow
    /// later fails ([compensation §6](../../docs/03-workflow-engine/compensation-engine.md)).
    #[serde(default)]
    pub compensate: Option<String>,
}

/// Whether an activity type is the engine-native for-each expansion
/// (WFL-301/302) — `for_each`, with `map` as an alias.
pub fn is_for_each(activity_type: &str) -> bool {
    matches!(activity_type, "for_each" | "map")
}

/// Default cap on how many of a `for_each`'s item instances run concurrently.
pub const DEFAULT_FOR_EACH_CONCURRENCY: usize = 8;

/// Default hard bound on how many items one `for_each` may expand to — a
/// runtime collection larger than this fails the activity closed rather than
/// silently truncating or unboundedly fanning out.
pub const DEFAULT_FOR_EACH_MAX_ITEMS: usize = 1000;

/// Parsed inputs of a `for_each`/`map` activity (WFL-301/302): the collection
/// to expand over, the per-item body, and the expansion bounds.
///
/// Wire shape (under the activity's `inputs`):
///
/// ```yaml
/// - id: summarize_all
///   type: for_each
///   inputs:
///     items: "${fetch.docs}"        # a ${...} reference, or a literal array
///     max_concurrent: 4             # optional; default 8
///     max_items: 500                # optional; default 1000 (fail-closed bound)
///     activity:                     # the per-item body template
///       type: tool
///       name: summarize
///       inputs: { doc: "${item}" }  # `item`/`item_index` are injected per instance
/// ```
#[derive(Debug, Clone)]
pub struct ForEachSpec {
    /// The collection: a `${...}` reference string or a literal array
    /// (elements may themselves contain `${...}` references). Resolved against
    /// the live workflow variables at expansion time.
    pub items: Value,
    /// The per-item activity template.
    pub body: ForEachBody,
    /// How many item instances may run concurrently (≥ 1).
    pub max_concurrent: usize,
    /// Fail-closed bound on the resolved collection's size.
    pub max_items: usize,
}

/// The per-item activity a `for_each` runs: an inline activity template
/// executed once per element, with `item` and `item_index` exposed as
/// variables to each instance.
#[derive(Debug, Clone, Deserialize)]
pub struct ForEachBody {
    /// Activity type (`function`, `tool`, `ai`, `agent`, …). Engine-native
    /// types (`wait`/`workflow`/`for_each`/`map`) cannot nest here.
    #[serde(rename = "type")]
    pub activity_type: String,
    /// Activity name (e.g. the tool id), if any.
    #[serde(default)]
    pub name: Option<String>,
    /// Static inputs passed to each instance (typically referencing `${item}`).
    #[serde(default)]
    pub inputs: Value,
    /// Per-item retry override (else the workflow default applies).
    #[serde(default)]
    pub retry: Option<RetryPolicy>,
}

/// Raw wire shape of a `for_each` activity's `inputs`.
#[derive(Deserialize)]
struct ForEachInputs {
    items: Value,
    activity: ForEachBody,
    #[serde(default)]
    max_concurrent: Option<u64>,
    #[serde(default)]
    max_items: Option<u64>,
}

impl ForEachSpec {
    /// Parse and validate a `for_each` activity's inputs, fail-closed — run at
    /// definition load so a malformed loop is a load error, not a runtime one.
    pub fn parse(activity: &ActivityDef) -> Result<Self> {
        let parsed: ForEachInputs =
            serde_json::from_value(activity.inputs.clone()).map_err(|e| {
                Error::invalid(format!(
                    "for_each activity `{}` has invalid inputs ({e}); expected \
                     {{items: <${{...}} or array>, activity: {{type, ...}}}}",
                    activity.id
                ))
            })?;
        match &parsed.items {
            Value::String(_) | Value::Array(_) => {}
            _ => {
                return Err(Error::invalid(format!(
                    "for_each activity `{}`: `items` must be a `${{...}}` reference \
                     or a literal array",
                    activity.id
                )));
            }
        }
        let body_type = parsed.activity.activity_type.as_str();
        if is_for_each(body_type) || matches!(body_type, "wait" | "workflow") {
            return Err(Error::invalid(format!(
                "for_each activity `{}`: body type `{body_type}` is engine-native and \
                 cannot run as a per-item body",
                activity.id
            )));
        }
        let max_concurrent = match parsed.max_concurrent {
            Some(0) => {
                return Err(Error::invalid(format!(
                    "for_each activity `{}`: max_concurrent must be at least 1",
                    activity.id
                )));
            }
            Some(n) => n as usize,
            None => DEFAULT_FOR_EACH_CONCURRENCY,
        };
        let max_items = match parsed.max_items {
            Some(0) => {
                return Err(Error::invalid(format!(
                    "for_each activity `{}`: max_items must be at least 1",
                    activity.id
                )));
            }
            Some(n) => n as usize,
            None => DEFAULT_FOR_EACH_MAX_ITEMS,
        };
        Ok(Self {
            items: parsed.items,
            body: parsed.activity,
            max_concurrent,
            max_items,
        })
    }
}

/// A directed edge between two activities.
#[derive(Debug, Clone, Deserialize)]
pub struct Transition {
    /// Source activity id.
    pub from: String,
    /// Destination activity id.
    pub to: String,
    /// Optional guard expression ([conditional branching §11](../../docs/03-workflow-engine/workflow-dsl.md#11-conditional-branching)).
    /// The edge is only followed when the guard evaluates true against the current
    /// workflow variables; an absent guard is always followed.
    #[serde(default)]
    pub when: Option<String>,
}

impl Definition {
    /// Parse and validate a definition from YAML.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let mut def: Definition = serde_yaml::from_str(yaml)
            .map_err(|e| Error::invalid(format!("invalid workflow manifest: {e}")))?;
        def.validate()?;
        def.source_hash = Some(content_hash(yaml));
        Ok(def)
    }

    /// The content hash of the source this definition was parsed from, if known.
    /// Used by the engine to pin an execution to its original definition.
    pub fn source_hash(&self) -> Option<&str> {
        self.source_hash.as_deref()
    }

    /// Load and validate a definition from a file.
    pub fn from_file(path: &str) -> Result<Self> {
        let yaml = std::fs::read_to_string(path)
            .map_err(|e| Error::config(format!("could not read workflow file {path}: {e}")))?;
        Self::from_yaml(&yaml)
    }

    /// Look up an activity by id.
    pub fn activity(&self, id: &str) -> Option<&ActivityDef> {
        self.spec.activities.iter().find(|a| a.id == id)
    }

    /// The set of activity ids used as compensation handlers. These run only
    /// during rollback, never as part of the forward DAG.
    pub fn compensation_targets(&self) -> BTreeSet<String> {
        self.spec
            .activities
            .iter()
            .filter_map(|a| a.compensate.clone())
            .collect()
    }

    /// The ids of activities that must complete before `id` may run.
    pub fn predecessors(&self, id: &str) -> Vec<String> {
        self.spec
            .transitions
            .iter()
            .filter(|t| t.to == id)
            .map(|t| t.from.clone())
            .collect()
    }

    /// The transitions entering `id` (its inbound edges).
    pub fn inbound(&self, id: &str) -> Vec<&Transition> {
        self.spec
            .transitions
            .iter()
            .filter(|t| t.to == id)
            .collect()
    }

    /// Effective retry policy for an activity: its own, else the workflow default,
    /// else the library default.
    pub fn retry_for(&self, id: &str) -> RetryPolicy {
        self.activity(id)
            .and_then(|a| a.retry.clone())
            .or_else(|| self.spec.retry.clone())
            .unwrap_or_default()
    }

    /// Validate the definition per the DSL rules.
    fn validate(&self) -> Result<()> {
        if let Some(kind) = &self.kind
            && kind != "Workflow"
        {
            return Err(Error::invalid(format!(
                "unsupported kind `{kind}`, expected `Workflow`"
            )));
        }
        if self.metadata.name.trim().is_empty() {
            return Err(Error::invalid("metadata.name must not be empty"));
        }
        if self.spec.activities.is_empty() {
            return Err(Error::invalid(
                "a workflow must declare at least one activity",
            ));
        }

        // Unique activity ids.
        let mut ids = BTreeSet::new();
        for a in &self.spec.activities {
            if a.id.trim().is_empty() {
                return Err(Error::invalid("activity id must not be empty"));
            }
            // Brackets are reserved for `for_each` instance ids (`<id>[<index>]`,
            // WFL-301) — a declared id using them could collide with one.
            if a.id.contains('[') || a.id.contains(']') {
                return Err(Error::invalid(format!(
                    "activity id `{}` must not contain `[`/`]` (reserved for for_each \
                     item-instance ids)",
                    a.id
                )));
            }
            if !ids.insert(a.id.as_str()) {
                return Err(Error::invalid(format!("duplicate activity id `{}`", a.id)));
            }
        }

        // `for_each` inputs are structurally validated at load, fail-closed.
        for a in &self.spec.activities {
            if is_for_each(&a.activity_type) {
                ForEachSpec::parse(a)?;
            }
        }

        // Compensation handlers must reference declared activities.
        for a in &self.spec.activities {
            if let Some(comp) = &a.compensate
                && !ids.contains(comp.as_str())
            {
                return Err(Error::invalid(format!(
                    "activity `{}` compensates with unknown activity `{comp}`",
                    a.id
                )));
            }
        }

        // Transitions must reference declared activities.
        for t in &self.spec.transitions {
            if !ids.contains(t.from.as_str()) {
                return Err(Error::invalid(format!(
                    "transition references unknown activity `{}`",
                    t.from
                )));
            }
            if !ids.contains(t.to.as_str()) {
                return Err(Error::invalid(format!(
                    "transition references unknown activity `{}`",
                    t.to
                )));
            }
        }

        self.check_acyclic()?;
        Ok(())
    }

    /// Reject cycles via Kahn's algorithm (a workflow DAG must be acyclic).
    fn check_acyclic(&self) -> Result<()> {
        let mut indegree: BTreeMap<&str, usize> = self
            .spec
            .activities
            .iter()
            .map(|a| (a.id.as_str(), 0))
            .collect();
        for t in &self.spec.transitions {
            *indegree.get_mut(t.to.as_str()).expect("validated above") += 1;
        }

        let mut queue: Vec<&str> = indegree
            .iter()
            .filter(|entry| *entry.1 == 0)
            .map(|entry| *entry.0)
            .collect();
        let mut visited = 0usize;

        while let Some(node) = queue.pop() {
            visited += 1;
            for t in self.spec.transitions.iter().filter(|t| t.from == node) {
                let d = indegree.get_mut(t.to.as_str()).expect("validated above");
                *d -= 1;
                if *d == 0 {
                    queue.push(t.to.as_str());
                }
            }
        }

        if visited != self.spec.activities.len() {
            return Err(Error::invalid("workflow graph contains a cycle"));
        }
        Ok(())
    }
}

/// Stable, dependency-free FNV-1a 64-bit hash of the source text, rendered hex.
/// Deterministic across processes (fixed offset basis + prime), so it is a sound
/// drift detector for definition pinning.
fn content_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINEAR: &str = r#"
apiVersion: workflow.wovyr.io/v1
kind: Workflow
metadata:
  name: linear
  version: 1.0.0
spec:
  activities:
    - { id: a, type: function }
    - { id: b, type: function }
  transitions:
    - { from: a, to: b }
"#;

    #[test]
    fn parses_and_validates_linear() {
        let def = Definition::from_yaml(LINEAR).unwrap();
        assert_eq!(def.metadata.name, "linear");
        assert_eq!(def.predecessors("b"), vec!["a"]);
        assert!(def.predecessors("a").is_empty());
    }

    #[test]
    fn rejects_duplicate_ids() {
        let yaml = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: a, type: function}\n";
        assert!(Definition::from_yaml(yaml).is_err());
    }

    #[test]
    fn rejects_unknown_transition() {
        let yaml = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: a, type: function}\n  transitions:\n    - {from: a, to: ghost}\n";
        assert!(Definition::from_yaml(yaml).is_err());
    }

    #[test]
    fn rejects_cycle() {
        let yaml = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n  transitions:\n    - {from: a, to: b}\n    - {from: b, to: a}\n";
        let err = Definition::from_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }

    // -----------------------------------------------------------------
    // WFL-301/302 — for_each/map: definition-load-time validation
    // -----------------------------------------------------------------

    #[test]
    fn for_each_with_a_reference_and_a_literal_array_both_parse() {
        let by_ref = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: fetch, type: function}\n    - {id: loop, type: for_each, inputs: {items: \"${fetch}\", activity: {type: function}}}\n  transitions:\n    - {from: fetch, to: loop}\n";
        assert!(Definition::from_yaml(by_ref).is_ok());

        let literal = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: loop, type: for_each, inputs: {items: [1, 2, 3], activity: {type: function}}}\n";
        assert!(Definition::from_yaml(literal).is_ok());
    }

    #[test]
    fn map_is_accepted_as_an_alias_for_for_each() {
        let yaml = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: loop, type: map, inputs: {items: [1], activity: {type: function}}}\n";
        assert!(Definition::from_yaml(yaml).is_ok());
    }

    #[test]
    fn for_each_rejects_missing_inputs() {
        let yaml = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: loop, type: for_each}\n";
        let err = Definition::from_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("loop"));
    }

    #[test]
    fn for_each_rejects_a_non_array_non_reference_items_value() {
        // A bare number is neither a `${...}` reference string nor a literal array.
        let yaml = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: loop, type: for_each, inputs: {items: 5, activity: {type: function}}}\n";
        let err = Definition::from_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("must be a"), "{err}");
    }

    #[test]
    fn for_each_rejects_an_engine_native_body_type() {
        for body_type in ["wait", "workflow", "for_each", "map"] {
            let yaml = format!(
                "metadata:\n  name: x\nspec:\n  activities:\n    - {{id: loop, type: for_each, inputs: {{items: [1], activity: {{type: {body_type}}}}}}}\n"
            );
            let result = Definition::from_yaml(&yaml);
            assert!(result.is_err(), "body type `{body_type}` must be rejected");
            assert!(result.unwrap_err().to_string().contains("engine-native"));
        }
    }

    #[test]
    fn for_each_rejects_zero_max_concurrent() {
        let yaml = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: loop, type: for_each, inputs: {items: [1], max_concurrent: 0, activity: {type: function}}}\n";
        let err = Definition::from_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("max_concurrent"), "{err}");
    }

    #[test]
    fn for_each_rejects_zero_max_items() {
        let yaml = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: loop, type: for_each, inputs: {items: [1], max_items: 0, activity: {type: function}}}\n";
        let err = Definition::from_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("max_items"), "{err}");
    }

    #[test]
    fn for_each_defaults_are_the_documented_constants() {
        let yaml = "metadata:\n  name: x\nspec:\n  activities:\n    - {id: loop, type: for_each, inputs: {items: [1], activity: {type: function}}}\n";
        let def = Definition::from_yaml(yaml).unwrap();
        let spec = ForEachSpec::parse(def.activity("loop").unwrap()).unwrap();
        assert_eq!(spec.max_concurrent, DEFAULT_FOR_EACH_CONCURRENCY);
        assert_eq!(spec.max_items, DEFAULT_FOR_EACH_MAX_ITEMS);
    }

    #[test]
    fn activity_id_with_brackets_is_rejected() {
        // Reserved for for_each's `<id>[<index>]` instance ids.
        let yaml =
            "metadata:\n  name: x\nspec:\n  activities:\n    - {id: \"a[0]\", type: function}\n";
        let err = Definition::from_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("["), "{err}");
    }
}
