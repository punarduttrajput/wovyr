//! A2UI v0.9.1 -> wovyr-ui frame adapter.
//!
//! Translates an A2UI message stream (`createSurface` / `updateComponents` /
//! `updateDataModel`) into a `UiFrame` the trust layer can evaluate.
//!
//! The adapter records every **lossy** mapping as a note, because the losses are
//! the point: A2UI declares no semantic action class, so every button is forced
//! to `Neutral`, and what that costs is exactly what issue a2ui-project/a2ui#2197
//! proposes to fix.

use serde_json::Value;
use std::collections::BTreeMap;
use wovyr_ui::{ActionClass, TextStyle, UiFrame, UiNode};

/// Result of adapting an A2UI message stream.
pub struct Adapted {
    pub frame: UiFrame,
    /// Lossy or noteworthy mappings, in encounter order.
    pub notes: Vec<String>,
}

/// A2UI has no semantic action class, so every control maps to this.
const FORCED_CLASS: ActionClass = ActionClass::Neutral;
const MAX_DEPTH: usize = 32;

pub fn adapt(messages: &[Value]) -> Result<Adapted, String> {
    let mut components: BTreeMap<String, Value> = BTreeMap::new();
    let mut data = Value::Object(Default::default());

    for msg in messages {
        if let Some(uc) = msg.get("updateComponents") {
            let list = uc
                .get("components")
                .and_then(Value::as_array)
                .ok_or("updateComponents.components is not an array")?;
            for c in list {
                let id = c
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("component without an id")?;
                components.insert(id.to_string(), c.clone());
            }
        }
        if let Some(ud) = msg.get("updateDataModel") {
            if let Some(v) = ud.get("value") {
                merge(&mut data, v);
            }
        }
    }

    let root = components
        .get("root")
        .ok_or("no component with id \"root\"")?
        .clone();

    let mut ctx = Ctx {
        components: &components,
        data: &data,
        notes: Vec::new(),
    };
    let node = ctx.node(&root, 0)?;

    if !ctx
        .notes
        .iter()
        .any(|n| n.starts_with("no semantic action class"))
    {
        // Only emitted when the surface actually has a control; a display-only
        // surface loses nothing.
    }

    Ok(Adapted {
        frame: UiFrame {
            schema_version: wovyr_ui::SCHEMA_VERSION.to_string(),
            title: None,
            provenance: Default::default(),
            root: node,
        },
        notes: ctx.notes,
    })
}

fn merge(target: &mut Value, src: &Value) {
    match (target, src) {
        (Value::Object(t), Value::Object(s)) => {
            for (k, v) in s {
                merge(t.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (t, s) => *t = s.clone(),
    }
}

struct Ctx<'a> {
    components: &'a BTreeMap<String, Value>,
    data: &'a Value,
    notes: Vec<String>,
}

impl<'a> Ctx<'a> {
    fn note(&mut self, s: String) {
        if !self.notes.contains(&s) {
            self.notes.push(s);
        }
    }

    /// Resolve an A2UI DynamicString: a literal, a `{"path": "/a/b"}` data
    /// binding, or a `{"call": ...}` function. A data-bound string is resolved
    /// against the surface's data model — a policy layer that skipped this would
    /// be trivially evaded by moving the text into `updateDataModel`.
    fn dynamic_string(&mut self, v: Option<&Value>, what: &str) -> String {
        match v {
            None => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(Value::Object(o)) if o.contains_key("path") => {
                let p = o["path"].as_str().unwrap_or("");
                match lookup(self.data, p) {
                    Some(Value::String(s)) => {
                        self.note(format!(
                            "{what} was data-bound to `{p}` and resolved from the data model; \
                             a policy that inspects components only would see no text here"
                        ));
                        s.clone()
                    }
                    _ => {
                        self.note(format!(
                            "{what} is data-bound to `{p}` with no value in the data model — \
                             unresolvable at policy time"
                        ));
                        String::new()
                    }
                }
            }
            Some(_) => {
                self.note(format!(
                    "{what} is a computed function call — not resolvable at policy time"
                ));
                String::new()
            }
        }
    }

    fn child_of(&self, c: &Value, key: &str) -> Option<Value> {
        c.get(key)
            .and_then(Value::as_str)
            .and_then(|id| self.components.get(id))
            .cloned()
    }

    fn children_of(&self, c: &Value) -> Vec<Value> {
        c.get("children")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .filter_map(|id| self.components.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn node(&mut self, c: &Value, depth: usize) -> Result<UiNode, String> {
        if depth > MAX_DEPTH {
            return Err("component tree deeper than 32 (cycle?)".into());
        }
        let kind = c
            .get("component")
            .and_then(Value::as_str)
            .ok_or("component without a `component` type")?;

        Ok(match kind {
            "Column" | "List" => UiNode::Column {
                children: self.kids(c, depth)?,
            },
            "Row" => UiNode::Row {
                children: self.kids(c, depth)?,
            },
            "Card" => {
                let inner = match self.child_of(c, "child") {
                    Some(k) => vec![self.node(&k, depth + 1)?],
                    None => self.kids(c, depth)?,
                };
                UiNode::Card {
                    title: None,
                    children: inner,
                }
            }
            "Divider" => UiNode::Divider {},
            "Text" | "Heading" => {
                let text = self.dynamic_string(c.get("text"), "Text.text");
                if text.contains("](") {
                    self.note(
                        "Text contains a Markdown link; A2UI Text supports Markdown, and no \
                         origin allow-list covers link targets"
                            .into(),
                    );
                }
                UiNode::Text {
                    text,
                    style: TextStyle::default(),
                }
            }
            "Image" => {
                let url = self.dynamic_string(c.get("url"), "Image.url");
                let alt = self.dynamic_string(c.get("description"), "Image.description");
                if alt.is_empty() {
                    self.note(
                        "Image has no `description`; A2UI makes it optional, so alt text cannot \
                         be relied on"
                            .into(),
                    );
                }
                UiNode::Image {
                    url,
                    alt: if alt.is_empty() {
                        "(no description)".to_string()
                    } else {
                        alt
                    },
                }
            }
            "TextField" => {
                let label = self.dynamic_string(c.get("label"), "TextField.label");
                // A2UI has no `name`; the data-binding path is the nearest analogue
                // and often carries the more honest signal (/payment/cardNumber).
                let name = c
                    .get("value")
                    .and_then(|v| v.get("path"))
                    .and_then(Value::as_str)
                    .map(|p| p.trim_start_matches('/').replace('/', "_"))
                    .unwrap_or_else(|| {
                        self.note(
                            "TextField has no data binding; only its label is available to policy"
                                .into(),
                        );
                        format!("field_{}", c.get("id").and_then(Value::as_str).unwrap_or("x"))
                    });
                let multiline = c.get("variant").and_then(Value::as_str) == Some("longText");
                UiNode::TextInput {
                    name,
                    label,
                    placeholder: None,
                    required: false,
                    multiline,
                }
            }
            "CheckBox" | "Checkbox" => UiNode::Checkbox {
                name: c
                    .get("value")
                    .and_then(|v| v.get("path"))
                    .and_then(Value::as_str)
                    .map(|p| p.trim_start_matches('/').replace('/', "_"))
                    .unwrap_or_else(|| "checkbox".into()),
                label: self.dynamic_string(c.get("label"), "CheckBox.label"),
                checked: false,
            },
            "Button" => {
                // The label is a *child Text node*, not a prop — resolving it needs
                // a lookup over the flat component list.
                let label = match self.child_of(c, "child") {
                    Some(k) => self.dynamic_string(k.get("text"), "Button label (child Text)"),
                    None => {
                        self.note("Button `child` does not resolve to a component".into());
                        String::new()
                    }
                };
                let action = c
                    .get("action")
                    .and_then(|a| {
                        a.get("event")
                            .and_then(|e| e.get("name"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| {
                                a.get("functionCall").and_then(|f| {
                                    let call =
                                        f.get("call").and_then(Value::as_str).unwrap_or("call");
                                    self.notes.push(format!(
                                        "Button action is a local functionCall `{call}` — a \
                                         client-side navigation primitive with no origin \
                                         allow-list in the spec"
                                    ));
                                    Some(call.to_string())
                                })
                            })
                    })
                    .unwrap_or_else(|| "unknown".into());

                self.note(
                    "no semantic action class in A2UI: every Button is forced to `neutral`, so \
                     policy cannot tell an affirmative control from a cancel or a destructive one"
                        .into(),
                );

                UiNode::Button {
                    action,
                    label,
                    class: FORCED_CLASS,
                }
            }
            other => {
                return Err(format!(
                    "component type `{other}` has no wovyr-ui equivalent in this adapter"
                ))
            }
        })
    }

    fn kids(&mut self, c: &Value, depth: usize) -> Result<Vec<UiNode>, String> {
        let raw = self.children_of(c);
        let mut out = Vec::with_capacity(raw.len());
        for k in raw {
            out.push(self.node(&k, depth + 1)?);
        }
        Ok(out)
    }
}

fn lookup<'a>(data: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = data;
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        cur = cur.get(seg)?;
    }
    Some(cur)
}
