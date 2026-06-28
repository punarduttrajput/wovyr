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
use apex_common::{Error, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// A parsed, validated workflow definition.
#[derive(Debug, Clone, Deserialize)]
pub struct Definition {
    /// Manifest API version, e.g. `workflow.apex.io/v1`.
    #[serde(rename = "apiVersion", default)]
    pub api_version: Option<String>,
    /// Manifest kind; expected to be `Workflow`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Identifying metadata.
    pub metadata: Metadata,
    /// Workflow body.
    pub spec: Spec,
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

/// A directed edge between two activities.
#[derive(Debug, Clone, Deserialize)]
pub struct Transition {
    /// Source activity id.
    pub from: String,
    /// Destination activity id.
    pub to: String,
}

impl Definition {
    /// Parse and validate a definition from YAML.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let def: Definition = serde_yaml::from_str(yaml)
            .map_err(|e| Error::invalid(format!("invalid workflow manifest: {e}")))?;
        def.validate()?;
        Ok(def)
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
            if !ids.insert(a.id.as_str()) {
                return Err(Error::invalid(format!("duplicate activity id `{}`", a.id)));
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

#[cfg(test)]
mod tests {
    use super::*;

    const LINEAR: &str = r#"
apiVersion: workflow.apex.io/v1
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
}
