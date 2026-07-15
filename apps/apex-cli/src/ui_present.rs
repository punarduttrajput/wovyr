//! An interactive, stdin-based [`UiInteraction`] (PRD-005 HIL-304) for `apex
//! agents run --local --interactive-ui` — the CLI's trusted-first-party
//! context (the same stance `run_local`'s `with_privileged_builtins()`
//! already takes), so frames render **unrestricted**: no policy required
//! for a local debug run. A hosted deployment never uses this presenter.

use apex_tools::UiInteraction;
use apex_ui::{UiDecision, UiFrame, UiNode};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;

pub struct StdinUiPresenter;

#[async_trait]
impl UiInteraction for StdinUiPresenter {
    async fn present(&self, frame: &UiFrame) -> apex_common::Result<UiDecision> {
        // Stdin reads block a thread; run it on the blocking pool rather than
        // parking a runtime worker for however long a human takes to answer.
        let frame = frame.clone();
        tokio::task::spawn_blocking(move || present_blocking(&frame))
            .await
            .map_err(|e| apex_common::Error::Runtime(format!("ui prompt task panicked: {e}")))?
    }
}

fn present_blocking(frame: &UiFrame) -> apex_common::Result<UiDecision> {
    println!();
    println!("--- ui frame ---------------------------------------------");
    if let Some(title) = &frame.title {
        println!("{title}");
        println!("------------------------------------------------------------");
    }
    print_node(&frame.root, 0);
    println!("------------------------------------------------------------");

    let mut values: BTreeMap<String, Value> = BTreeMap::new();
    prompt_inputs(&frame.root, &mut values)?;

    let actions = frame.actions();
    if actions.is_empty() {
        return Err(apex_common::Error::Runtime(
            "ui frame declares no actions to choose from".into(),
        ));
    }
    loop {
        let choices: Vec<&str> = actions.iter().map(|(action, _, _)| *action).collect();
        print!("choose an action [{}]: ", choices.join(", "));
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let chosen = line.trim();
        if choices.contains(&chosen) {
            return Ok(UiDecision {
                action: chosen.to_string(),
                values: values.clone(),
            });
        }
        println!("unknown action `{chosen}` — try again");
    }
}

fn print_node(node: &UiNode, depth: usize) {
    let indent = "  ".repeat(depth);
    match node {
        UiNode::Column { children } | UiNode::Row { children } => {
            for child in children {
                print_node(child, depth);
            }
        }
        UiNode::Card { title, children } => {
            if let Some(title) = title {
                println!("{indent}[{title}]");
            }
            for child in children {
                print_node(child, depth + 1);
            }
        }
        UiNode::Divider {} => println!("{indent}----"),
        UiNode::Text { text, .. } => println!("{indent}{text}"),
        UiNode::Badge { text, .. } => println!("{indent}({text})"),
        UiNode::KeyValue { entries } => {
            for entry in entries {
                println!("{indent}{}: {}", entry.key, entry.value);
            }
        }
        UiNode::Image { alt, .. } => println!("{indent}[image: {alt}]"),
        UiNode::TextInput {
            label, required, ..
        } => {
            println!(
                "{indent}{label}{}",
                if *required { " (required)" } else { "" }
            );
        }
        UiNode::NumberInput {
            label, required, ..
        } => {
            println!(
                "{indent}{label}{}",
                if *required { " (required)" } else { "" }
            );
        }
        UiNode::Select {
            label,
            options,
            required,
            ..
        } => {
            let choices: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
            println!(
                "{indent}{label} [{}]{}",
                choices.join(", "),
                if *required { " (required)" } else { "" }
            );
        }
        UiNode::Checkbox { label, .. } => println!("{indent}[ ] {label}"),
        UiNode::Button { .. } => {} // actions are prompted separately, once, at the end
    }
}

/// Prompt for every declared input's value, in tree order, skipping optional
/// fields the human leaves blank. Values collected here are validated
/// against the frame again by `UiPresentTool::execute` (HIL-302) before
/// anything reaches the model — this function only needs to gather a
/// best-effort answer, not enforce the contract itself.
fn prompt_inputs(node: &UiNode, values: &mut BTreeMap<String, Value>) -> apex_common::Result<()> {
    match node {
        UiNode::Column { children } | UiNode::Row { children } => {
            for child in children {
                prompt_inputs(child, values)?;
            }
        }
        UiNode::Card { children, .. } => {
            for child in children {
                prompt_inputs(child, values)?;
            }
        }
        UiNode::TextInput {
            name,
            label,
            required,
            ..
        } => {
            if let Some(answer) = prompt_line(label, *required)? {
                values.insert(name.clone(), Value::String(answer));
            }
        }
        UiNode::NumberInput {
            name,
            label,
            required,
            ..
        } => {
            if let Some(answer) = prompt_line(label, *required)? {
                match answer.parse::<f64>() {
                    Ok(n) => {
                        values.insert(name.clone(), serde_json::json!(n));
                    }
                    Err(_) => println!("  (not a number — leaving `{name}` unset)"),
                }
            }
        }
        UiNode::Select {
            name,
            label,
            options,
            required,
        } => {
            let choices: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
            let prompt = format!("{label} [{}]", choices.join(", "));
            if let Some(answer) = prompt_line(&prompt, *required)? {
                if choices.contains(&answer.as_str()) {
                    values.insert(name.clone(), Value::String(answer));
                } else {
                    println!("  (not one of the options — leaving `{name}` unset)");
                }
            }
        }
        UiNode::Checkbox { name, label, .. } => {
            if let Some(answer) = prompt_line(&format!("{label} (y/N)"), false)? {
                values.insert(
                    name.clone(),
                    Value::Bool(matches!(
                        answer.to_lowercase().as_str(),
                        "y" | "yes" | "true"
                    )),
                );
            }
        }
        UiNode::Divider {}
        | UiNode::Text { .. }
        | UiNode::Badge { .. }
        | UiNode::KeyValue { .. }
        | UiNode::Image { .. }
        | UiNode::Button { .. } => {}
    }
    Ok(())
}

/// Prompt once for `label`; a required field re-prompts on an empty answer,
/// an optional one returns `None` for a blank line.
fn prompt_line(label: &str, required: bool) -> apex_common::Result<Option<String>> {
    loop {
        print!("{label}: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if required {
                println!("  (required)");
                continue;
            }
            return Ok(None);
        }
        return Ok(Some(trimmed.to_string()));
    }
}
