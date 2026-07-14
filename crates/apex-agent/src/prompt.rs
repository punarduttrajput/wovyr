//! Prompt template/versioning registry (RM-AIM-P2 SAF-202).
//!
//! Agent instructions were a raw YAML string with no versioning, variables, or
//! A/B story. A [`PromptTemplate`] is a **named, versioned** prompt body with
//! typed variable declarations (`{{name}}` placeholders — distinct from the
//! workflow engine's `${...}` interpolation, which binds activity outputs, not
//! prompt variables); a [`PromptRegistry`] holds every registered version and
//! resolves an agent's [`PromptSpec`] reference to a rendered system prompt.
//!
//! **Versions are immutable.** Registering an already-registered
//! `(name, version)` is a [`Error::Conflict`] — that immutability is what makes
//! a pinned version mean anything: an agent pinned to `version: 3` renders the
//! same instructions on every run, forever, no matter what is published later.
//! An *unpinned* reference resolves to the latest version at resolve time — a
//! deliberate opt-in to drift, documented rather than default-hidden.
//!
//! **A/B selection is deterministic** (coding-standards §7 — no ambient
//! randomness): an experiment assigns a caller-supplied *unit* (a user/session
//! id — whatever the experiment should be sticky per) to an arm by a stable
//! FNV-1a hash of `(template name, unit)` weighted across the arms. The same
//! unit gets the same version on every run and on every node — `DefaultHasher`
//! is explicitly not used, since its output isn't stable across builds (the
//! same reason the semantic cache embeds text verbatim instead of hashing it).
//!
//! **Rendering is fail-closed** (the guardrail/eval stance): a missing required
//! variable, a wrongly-typed value, a supplied-but-undeclared variable, or a
//! body placeholder that was never declared are all clear errors — never a
//! prompt with a literal `{{hole}}` silently shipped to the model.

use apex_common::{Error, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// The type a template variable's supplied value must have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariableType {
    /// A JSON string, rendered verbatim.
    #[default]
    String,
    /// A JSON integer (no fractional part).
    Integer,
    /// Any JSON number.
    Number,
    /// A JSON boolean.
    Boolean,
}

impl VariableType {
    fn matches(self, value: &Value) -> bool {
        match self {
            VariableType::String => value.is_string(),
            VariableType::Integer => value.is_i64() || value.is_u64(),
            VariableType::Number => value.is_number(),
            VariableType::Boolean => value.is_boolean(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            VariableType::String => "string",
            VariableType::Integer => "integer",
            VariableType::Number => "number",
            VariableType::Boolean => "boolean",
        }
    }
}

/// A typed variable a template's body may reference. A variable with a
/// `default` is optional; one without is required at render time.
#[derive(Clone, Debug, Deserialize)]
pub struct VariableSpec {
    /// The placeholder name, referenced as `{{name}}` in the body.
    pub name: String,
    /// Required JSON type of the supplied value (default `string`).
    #[serde(rename = "type", default)]
    pub var_type: VariableType,
    /// Value used when the reference supplies none; its type must match.
    #[serde(default)]
    pub default: Option<Value>,
}

/// One immutable version of a named prompt.
#[derive(Clone, Debug, Deserialize)]
pub struct PromptTemplate {
    /// Registry name shared by every version of this prompt.
    pub name: String,
    /// Monotonic version (1-based by convention; any distinct u32 works).
    pub version: u32,
    /// The prompt body; `{{name}}` placeholders are substituted at render time.
    pub body: String,
    /// Declared variables. Placeholders in the body must be declared here.
    #[serde(default)]
    pub variables: Vec<VariableSpec>,
}

impl PromptTemplate {
    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::invalid("prompt template name must not be empty"));
        }
        if self.body.trim().is_empty() {
            return Err(Error::invalid(format!(
                "prompt template `{}` v{} has an empty body",
                self.name, self.version
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for var in &self.variables {
            if !seen.insert(var.name.as_str()) {
                return Err(Error::invalid(format!(
                    "prompt template `{}` v{} declares variable `{}` twice",
                    self.name, self.version, var.name
                )));
            }
            if let Some(default) = &var.default
                && !var.var_type.matches(default)
            {
                return Err(Error::invalid(format!(
                    "prompt template `{}` v{}: variable `{}` default is not a {}",
                    self.name,
                    self.version,
                    var.name,
                    var.var_type.label()
                )));
            }
        }
        for segment in segments(&self.body) {
            if let Segment::Placeholder(name) = segment
                && !seen.contains(name)
            {
                return Err(Error::invalid(format!(
                    "prompt template `{}` v{} references undeclared variable `{{{{{name}}}}}`",
                    self.name, self.version
                )));
            }
        }
        Ok(())
    }

    /// Render the body with `supplied` variable values, fail-closed: a missing
    /// required variable, a type mismatch, or a supplied-but-undeclared name is
    /// an error — never a partially-substituted prompt.
    pub fn render(&self, supplied: &BTreeMap<String, Value>) -> Result<String> {
        for name in supplied.keys() {
            if !self.variables.iter().any(|v| &v.name == name) {
                return Err(Error::invalid(format!(
                    "prompt template `{}` v{} declares no variable `{name}`",
                    self.name, self.version
                )));
            }
        }
        let mut out = String::with_capacity(self.body.len());
        for segment in segments(&self.body) {
            match segment {
                Segment::Text(text) => out.push_str(text),
                Segment::Placeholder(name) => {
                    // validate() guarantees every placeholder is declared.
                    let spec = self
                        .variables
                        .iter()
                        .find(|v| v.name == name)
                        .ok_or_else(|| {
                            Error::invalid(format!(
                                "prompt template `{}` v{} references undeclared variable `{name}`",
                                self.name, self.version
                            ))
                        })?;
                    let value = supplied.get(name).or(spec.default.as_ref()).ok_or_else(|| {
                        Error::invalid(format!(
                            "prompt template `{}` v{} requires variable `{name}` and no default is declared",
                            self.name, self.version
                        ))
                    })?;
                    if !spec.var_type.matches(value) {
                        return Err(Error::invalid(format!(
                            "prompt template `{}` v{}: variable `{name}` must be a {}, got {value}",
                            self.name,
                            self.version,
                            spec.var_type.label()
                        )));
                    }
                    match value.as_str() {
                        Some(s) => out.push_str(s),
                        // Non-string values render canonically (`42`, `true`).
                        None => out.push_str(&value.to_string()),
                    }
                }
            }
        }
        Ok(out)
    }
}

/// One arm of an A/B experiment: a registered version and its traffic weight.
#[derive(Clone, Debug, Deserialize)]
pub struct AbArm {
    /// The template version this arm serves.
    pub version: u32,
    /// Relative share of units assigned to this arm (weights need not sum to
    /// anything in particular; `0` receives no traffic).
    pub weight: u32,
}

/// An agent manifest's reference to a registered prompt
/// (`spec.prompt` — the SAF-202 alternative to a raw `spec.instructions`).
///
/// Exactly one selection mode applies: an explicit `version` pin, an `ab`
/// experiment (resolved with a per-run unit key), or — with neither — the
/// latest registered version at resolve time.
#[derive(Clone, Debug, Deserialize)]
pub struct PromptSpec {
    /// Name of the registered prompt.
    pub template: String,
    /// Pin to this exact version. Wins over drift: the rendered instructions
    /// never change once pinned, no matter what is registered later.
    #[serde(default)]
    pub version: Option<u32>,
    /// A/B experiment over registered versions (mutually exclusive with
    /// `version`). Resolution requires a unit key to assign deterministically.
    #[serde(default)]
    pub ab: Option<Vec<AbArm>>,
    /// Values for the template's declared variables.
    #[serde(default)]
    pub variables: BTreeMap<String, Value>,
}

impl PromptSpec {
    /// Structural validation, run at manifest load (fail-closed before any
    /// registry is consulted).
    pub fn validate(&self) -> Result<()> {
        if self.template.trim().is_empty() {
            return Err(Error::invalid("spec.prompt.template must not be empty"));
        }
        if let Some(arms) = &self.ab {
            if self.version.is_some() {
                return Err(Error::invalid(
                    "spec.prompt sets both `version` and `ab` — pin or experiment, not both",
                ));
            }
            if arms.is_empty() {
                return Err(Error::invalid(
                    "spec.prompt.ab must declare at least one arm",
                ));
            }
            if arms.iter().all(|a| a.weight == 0) {
                return Err(Error::invalid(
                    "spec.prompt.ab arms have zero total weight — no arm can be assigned",
                ));
            }
        }
        Ok(())
    }
}

/// The registry of named, versioned prompt templates.
#[derive(Clone, Debug, Default)]
pub struct PromptRegistry {
    templates: BTreeMap<String, BTreeMap<u32, PromptTemplate>>,
}

/// Wire shape of a registry YAML document (`prompts: [...]`).
#[derive(Deserialize)]
struct RegistryDoc {
    prompts: Vec<PromptTemplate>,
}

impl PromptRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a registry from a YAML document of the shape
    /// `prompts: [{name, version, body, variables: [...]}, ...]`.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let doc: RegistryDoc = serde_yaml::from_str(yaml)
            .map_err(|e| Error::invalid(format!("invalid prompt registry document: {e}")))?;
        let mut registry = Self::new();
        for template in doc.prompts {
            registry.register(template)?;
        }
        Ok(registry)
    }

    /// Register a template version. Fails with [`Error::Conflict`] if this
    /// `(name, version)` already exists — versions are immutable, which is
    /// what makes a pinned reference stable across runs.
    pub fn register(&mut self, template: PromptTemplate) -> Result<()> {
        template.validate()?;
        let versions = self.templates.entry(template.name.clone()).or_default();
        if versions.contains_key(&template.version) {
            return Err(Error::Conflict(format!(
                "prompt template `{}` v{} is already registered — versions are immutable, register a new version instead",
                template.name, template.version
            )));
        }
        versions.insert(template.version, template);
        Ok(())
    }

    /// Fetch an exact registered version.
    pub fn get(&self, name: &str, version: u32) -> Result<&PromptTemplate> {
        self.templates
            .get(name)
            .and_then(|versions| versions.get(&version))
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "prompt template `{name}` v{version} is not registered"
                ))
            })
    }

    /// Fetch the highest registered version of a named prompt.
    pub fn latest(&self, name: &str) -> Result<&PromptTemplate> {
        self.templates
            .get(name)
            .and_then(|versions| versions.values().next_back())
            .ok_or_else(|| Error::NotFound(format!("prompt template `{name}` is not registered")))
    }

    /// Resolve a [`PromptSpec`] reference to rendered instructions.
    ///
    /// `ab_unit` is the experiment's assignment unit (a user/session id —
    /// whatever the split should be sticky per); it is required when the spec
    /// declares an `ab` experiment and ignored otherwise. Fail-closed
    /// throughout: an unknown template/version, a zero-weight experiment, a
    /// missing unit, or a render error all surface rather than degrade.
    pub fn resolve(&self, spec: &PromptSpec, ab_unit: Option<&str>) -> Result<String> {
        spec.validate()?;
        let template = match (&spec.version, &spec.ab) {
            (Some(version), _) => self.get(&spec.template, *version)?,
            (None, Some(arms)) => {
                let unit = ab_unit.ok_or_else(|| {
                    Error::invalid(format!(
                        "prompt template `{}` declares an A/B experiment; resolution requires a unit key (user/session id) to assign an arm",
                        spec.template
                    ))
                })?;
                self.get(&spec.template, assign_arm(&spec.template, unit, arms))?
            }
            (None, None) => self.latest(&spec.template)?,
        };
        template.render(&spec.variables)
    }
}

/// Deterministically assign `unit` to one of `arms` by weight: a stable hash of
/// `(template, unit)` picks a bucket in the total-weight range, so the same
/// unit lands on the same arm on every run and every node. Callers validate
/// that total weight is non-zero ([`PromptSpec::validate`]).
fn assign_arm(template: &str, unit: &str, arms: &[AbArm]) -> u32 {
    let total: u64 = arms.iter().map(|a| u64::from(a.weight)).sum();
    let mut bucket = fnv1a64(format!("{template}\x1f{unit}").as_bytes()) % total;
    for arm in arms {
        let weight = u64::from(arm.weight);
        if bucket < weight {
            return arm.version;
        }
        bucket -= weight;
    }
    // Unreachable: bucket < total = sum of weights. Fall back to the last arm.
    arms[arms.len() - 1].version
}

/// FNV-1a 64-bit — a tiny, dependency-free hash whose output is stable across
/// builds and platforms (unlike `DefaultHasher`, whose algorithm is
/// unspecified), so A/B assignment is reproducible fleet-wide.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A parsed slice of a template body.
enum Segment<'a> {
    /// Literal text, emitted verbatim.
    Text(&'a str),
    /// An identifier-shaped `{{name}}` placeholder.
    Placeholder(&'a str),
}

/// Split a body into literal text and `{{identifier}}` placeholders. Braced
/// content that is not identifier-shaped (a JSON example, `{{ {"k": 1} }}`)
/// stays literal text, so prompt bodies can contain structured examples
/// without escaping.
fn segments(body: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        let (head, braced) = rest.split_at(open);
        if !head.is_empty() {
            out.push(Segment::Text(head));
        }
        let inner = &braced[2..];
        match inner.find("}}") {
            Some(close) => {
                let name = inner[..close].trim();
                if is_identifier(name) {
                    out.push(Segment::Placeholder(name));
                } else {
                    out.push(Segment::Text(&braced[..close + 4]));
                }
                rest = &inner[close + 2..];
            }
            None => {
                // Unclosed `{{` — literal to the end.
                out.push(Segment::Text(braced));
                rest = "";
            }
        }
    }
    if !rest.is_empty() {
        out.push(Segment::Text(rest));
    }
    out
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn greeting(version: u32, body: &str) -> PromptTemplate {
        PromptTemplate {
            name: "greeting".into(),
            version,
            body: body.into(),
            variables: vec![
                VariableSpec {
                    name: "tone".into(),
                    var_type: VariableType::String,
                    default: None,
                },
                VariableSpec {
                    name: "max_words".into(),
                    var_type: VariableType::Integer,
                    default: Some(json!(50)),
                },
            ],
        }
    }

    #[test]
    fn render_substitutes_typed_variables_and_defaults() {
        let t = greeting(1, "Be {{tone}}. Answer in at most {{ max_words }} words.");
        let rendered = t
            .render(&BTreeMap::from([("tone".to_string(), json!("friendly"))]))
            .unwrap();
        assert_eq!(rendered, "Be friendly. Answer in at most 50 words.");

        // A supplied value overrides the default; integers render canonically.
        let rendered = t
            .render(&BTreeMap::from([
                ("tone".to_string(), json!("terse")),
                ("max_words".to_string(), json!(10)),
            ]))
            .unwrap();
        assert_eq!(rendered, "Be terse. Answer in at most 10 words.");
    }

    #[test]
    fn render_fails_closed_on_missing_wrongly_typed_or_undeclared_variables() {
        let t = greeting(1, "Be {{tone}}.");

        // Required variable absent (no default) → error, never a literal hole.
        let err = t.render(&BTreeMap::new()).unwrap_err();
        assert!(
            err.to_string().contains("requires variable `tone`"),
            "{err}"
        );

        // Wrong type → error naming the expected type.
        let err = t
            .render(&BTreeMap::from([("tone".to_string(), json!(3))]))
            .unwrap_err();
        assert!(err.to_string().contains("must be a string"), "{err}");

        // Supplied-but-undeclared → error (typo protection).
        let err = t
            .render(&BTreeMap::from([
                ("tone".to_string(), json!("kind")),
                ("tonee".to_string(), json!("kind")),
            ]))
            .unwrap_err();
        assert!(err.to_string().contains("no variable `tonee`"), "{err}");
    }

    #[test]
    fn validation_rejects_undeclared_placeholders_but_keeps_json_examples_literal() {
        let mut registry = PromptRegistry::new();
        let err = registry
            .register(PromptTemplate {
                name: "bad".into(),
                version: 1,
                body: "Hello {{who}}".into(),
                variables: vec![],
            })
            .unwrap_err();
        assert!(err.to_string().contains("undeclared variable"), "{err}");

        // Non-identifier braced content is literal text, not a placeholder —
        // a body may embed structured examples without escaping.
        let t = PromptTemplate {
            name: "json-example".into(),
            version: 1,
            body: r#"Reply as {{ {"answer": "..."} }} exactly."#.into(),
            variables: vec![],
        };
        registry.register(t.clone()).unwrap();
        assert_eq!(t.render(&BTreeMap::new()).unwrap(), t.body);
    }

    #[test]
    fn versions_are_immutable_and_latest_tracks_the_highest() {
        let mut registry = PromptRegistry::new();
        registry.register(greeting(1, "v1 {{tone}}")).unwrap();
        registry.register(greeting(2, "v2 {{tone}}")).unwrap();

        // Re-registering an existing version conflicts — pins stay meaningful.
        let err = registry
            .register(greeting(1, "rewritten {{tone}}"))
            .unwrap_err();
        assert!(matches!(err, Error::Conflict(_)), "{err}");

        assert_eq!(registry.latest("greeting").unwrap().version, 2);
        assert_eq!(registry.get("greeting", 1).unwrap().body, "v1 {{tone}}");
        assert!(registry.get("greeting", 9).is_err());
        assert!(registry.latest("nope").is_err());
    }

    /// The SAF-202 acceptance criterion: a versioned template resolves with
    /// variables, and a pinned reference renders identically across runs even
    /// after a newer version is registered — while an unpinned reference drifts.
    #[test]
    fn a_pinned_version_resolves_identically_across_runs() {
        let mut registry = PromptRegistry::new();
        registry
            .register(greeting(1, "Be {{tone}} (v1, cap {{max_words}})."))
            .unwrap();

        let pinned = PromptSpec {
            template: "greeting".into(),
            version: Some(1),
            ab: None,
            variables: BTreeMap::from([("tone".to_string(), json!("warm"))]),
        };
        let first_run = registry.resolve(&pinned, None).unwrap();
        assert_eq!(first_run, "Be warm (v1, cap 50).");

        // A newer version lands between runs.
        registry.register(greeting(2, "Be {{tone}} (v2).")).unwrap();

        // The pin holds; the unpinned reference picks up the new latest.
        assert_eq!(registry.resolve(&pinned, None).unwrap(), first_run);
        let unpinned = PromptSpec {
            version: None,
            ..pinned.clone()
        };
        assert_eq!(registry.resolve(&unpinned, None).unwrap(), "Be warm (v2).");
    }

    #[test]
    fn ab_selection_is_deterministic_per_unit_and_splits_across_units() {
        let mut registry = PromptRegistry::new();
        registry.register(greeting(1, "v1 {{tone}}")).unwrap();
        registry.register(greeting(2, "v2 {{tone}}")).unwrap();
        let spec = PromptSpec {
            template: "greeting".into(),
            version: None,
            ab: Some(vec![
                AbArm {
                    version: 1,
                    weight: 1,
                },
                AbArm {
                    version: 2,
                    weight: 1,
                },
            ]),
            variables: BTreeMap::from([("tone".to_string(), json!("calm"))]),
        };

        // Sticky: the same unit resolves the same arm on every run.
        let assigned = registry.resolve(&spec, Some("user-42")).unwrap();
        for _ in 0..10 {
            assert_eq!(registry.resolve(&spec, Some("user-42")).unwrap(), assigned);
        }

        // Split: across many units, both arms receive traffic.
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..64 {
            seen.insert(registry.resolve(&spec, Some(&format!("user-{i}"))).unwrap());
        }
        assert_eq!(seen.len(), 2, "an even 1:1 split must reach both arms");

        // A zero-weight arm receives none.
        let one_sided = PromptSpec {
            ab: Some(vec![
                AbArm {
                    version: 1,
                    weight: 0,
                },
                AbArm {
                    version: 2,
                    weight: 1,
                },
            ]),
            ..spec.clone()
        };
        for i in 0..64 {
            assert_eq!(
                registry
                    .resolve(&one_sided, Some(&format!("user-{i}")))
                    .unwrap(),
                "v2 calm"
            );
        }
    }

    #[test]
    fn ab_resolution_fails_closed_without_a_unit_key_and_on_bad_specs() {
        let mut registry = PromptRegistry::new();
        registry.register(greeting(1, "v1 {{tone}}")).unwrap();
        let arms = Some(vec![AbArm {
            version: 1,
            weight: 1,
        }]);
        let spec = PromptSpec {
            template: "greeting".into(),
            version: None,
            ab: arms.clone(),
            variables: BTreeMap::from([("tone".to_string(), json!("calm"))]),
        };
        let err = registry.resolve(&spec, None).unwrap_err();
        assert!(err.to_string().contains("requires a unit key"), "{err}");

        // Pin + experiment together is a validation error.
        let both = PromptSpec {
            version: Some(1),
            ab: arms,
            ..spec.clone()
        };
        assert!(registry.resolve(&both, Some("u")).is_err());

        // All-zero weights can never assign — rejected up front.
        let zero = PromptSpec {
            ab: Some(vec![AbArm {
                version: 1,
                weight: 0,
            }]),
            ..spec
        };
        assert!(registry.resolve(&zero, Some("u")).is_err());
    }

    #[test]
    fn from_yaml_loads_a_registry_document() {
        let registry = PromptRegistry::from_yaml(
            r#"
prompts:
  - name: support
    version: 1
    body: "You are a {{tone}} support agent."
    variables:
      - name: tone
        type: string
        default: friendly
  - name: support
    version: 2
    body: "You are a {{tone}} support agent. Cap: {{limit}} words."
    variables:
      - name: tone
        type: string
        default: friendly
      - name: limit
        type: integer
"#,
        )
        .unwrap();
        assert_eq!(registry.latest("support").unwrap().version, 2);
        assert_eq!(
            registry
                .get("support", 1)
                .unwrap()
                .render(&BTreeMap::new())
                .unwrap(),
            "You are a friendly support agent."
        );

        // A duplicate (name, version) in the document conflicts like register().
        assert!(
            PromptRegistry::from_yaml(
                "prompts:\n  - {name: a, version: 1, body: x}\n  - {name: a, version: 1, body: y}\n"
            )
            .is_err()
        );
    }
}
