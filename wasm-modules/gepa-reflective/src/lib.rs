//! GEPA Reflective Dataset WASM module.
//!
//! Analyzes full agent trajectories per task to build workflow-level
//! reflective triplets. Captures both successes AND failures to give
//! the LLM complete context for proposing spec mutations.
//!
//! Each triplet represents a complete agent workflow attempt:
//! - input: what the agent was trying to accomplish (goal + reasoning chain)
//! - output: what happened (action sequence, final state, breakdown point)
//! - feedback: structured analysis with specific improvement suggestions
//!
//! Also extracts cross-trajectory patterns: common failure points,
//! successful patterns to preserve, missing capabilities, guard friction.
//!
//! Build: `cargo build -p gepa-reflective-module --target wasm32-unknown-unknown --release`

use temper_wasm_sdk::prelude::*;

temper_module! {
    fn run(ctx: Context) -> Result<Value> {
        ctx.log("info", "gepa-reflective: building workflow-level reflective dataset");

        let fields = ctx.entity_state.get("fields").unwrap_or(&ctx.entity_state);
        let skill_name = fields
            .get("SkillName")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let entity_type = fields
            .get("TargetEntityType")
            .and_then(Value::as_str)
            .unwrap_or("unknown");

        // Read the replay result (workflow-level output from gepa-replay)
        let replay_str = ctx.trigger_params
            .get("ReplayResultJson")
            .or_else(|| ctx.trigger_params.get("replay_result"))
            .or_else(|| fields.get("ReplayResultJson"));
        let replay: Value = match replay_str {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(json!({})),
            Some(v) => v.clone(),
            None => json!({}),
        };
        let replay_result = replay.get("replay_result").unwrap_or(&replay);

        // Read raw OTS trajectories for agent reasoning
        let trajectories_val = ctx.trigger_params
            .get("Trajectories")
            .or_else(|| fields.get("Trajectories"));
        let trajectories_parsed: Vec<Value>;
        let trajectories: &[Value] = match trajectories_val {
            Some(Value::Array(arr)) => arr,
            Some(Value::String(s)) => {
                trajectories_parsed = match serde_json::from_str::<Value>(s) {
                    Ok(Value::Array(arr)) => arr,
                    Ok(val) => vec![val],
                    Err(_) => vec![],
                };
                &trajectories_parsed
            }
            _ => {
                trajectories_parsed = vec![];
                &trajectories_parsed
            }
        };

        // Read spec source for context
        let spec_source = fields
            .get("SpecSource")
            .and_then(Value::as_str)
            .unwrap_or("");

        // Read previous verification errors
        let verification_feedback: Vec<String> = fields
            .get("VerificationErrors")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).map(String::from).collect())
            .unwrap_or_default();

        // Build workflow-level triplets
        let mut workflow_triplets: Vec<Value> = Vec::new();
        let replay_workflows = replay_result.get("workflows").and_then(Value::as_array);

        if let Some(workflows) = replay_workflows {
            for workflow in workflows {
                let trajectory_id = workflow.get("trajectory_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let outcome = workflow.get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let agent_goal = workflow.get("agent_goal")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let actions_total = workflow.get("actions_total")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let actions_succeeded = workflow.get("actions_succeeded")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let final_state = workflow.get("final_state")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");

                // Find matching OTS trajectory for agent reasoning
                let reasoning_chain = extract_reasoning_chain(trajectories, trajectory_id);

                // Build the input: what the agent was trying to do
                let input = if reasoning_chain.is_empty() {
                    format!("Agent session '{trajectory_id}' attempted {actions_total} actions \
                             targeting {entity_type}. Goal: {agent_goal}.")
                } else {
                    format!("Agent session '{trajectory_id}' working on {entity_type}. \
                             Goal: {agent_goal}.\n\nReasoning chain:\n{reasoning_chain}")
                };

                // Build the output: what actually happened
                let errors = workflow.get("errors").and_then(Value::as_array);
                let error_summary = match errors {
                    Some(errs) if !errs.is_empty() => {
                        let summaries: Vec<String> = errs.iter().map(|e| {
                            let action = e.get("action").and_then(Value::as_str).unwrap_or("?");
                            let from = e.get("from_state").and_then(Value::as_str).unwrap_or("?");
                            let kind = e.get("error_kind").and_then(Value::as_str).unwrap_or("?");
                            let msg = e.get("message").and_then(Value::as_str).unwrap_or("?");
                            format!("  - '{action}' from state '{from}': {kind} — {msg}")
                        }).collect();
                        error_summaries_to_string(&summaries)
                    }
                    _ => String::new(),
                };

                let output = format!(
                    "Outcome: {outcome}. {actions_succeeded}/{actions_total} actions succeeded. \
                     Final state: {final_state}.{error_summary}"
                );

                // Build feedback based on outcome
                let (feedback, preserve) = build_feedback(outcome, workflow, entity_type);

                let score = match outcome {
                    "completed" => 1.0,
                    "partial" => 0.5,
                    _ => 0.0,
                };

                workflow_triplets.push(json!({
                    "input": input,
                    "output": output,
                    "feedback": feedback,
                    "score": score,
                    "preserve": preserve,
                    "trajectory_id": trajectory_id,
                    "entity_type": entity_type,
                    "outcome": outcome,
                    "actions_total": actions_total,
                    "actions_succeeded": actions_succeeded,
                }));
            }
        }

        // Extract cross-trajectory patterns
        let patterns = extract_patterns(replay_result);

        let workflow_completion_rate = replay_result
            .get("workflow_completion_rate")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let total_trajectories = replay_workflows.map(|w| w.len()).unwrap_or(0);
        let failure_count = workflow_triplets.iter()
            .filter(|t| t.get("score").and_then(Value::as_f64).unwrap_or(0.0) < 0.5)
            .count();
        let success_count = workflow_triplets.len() - failure_count;

        ctx.log("info", &format!(
            "gepa-reflective: {success_count} successful, {failure_count} failed workflows \
             from {total_trajectories} trajectories"
        ));

        let dataset = json!({
            "skill_name": skill_name,
            "entity_type": entity_type,
            "spec_source": spec_source,
            "workflow_triplets": workflow_triplets,
            "patterns": patterns,
            "verification_feedback": verification_feedback,
            "workflow_completion_rate": workflow_completion_rate,
            "total_trajectories": total_trajectories,
            "failure_count": failure_count,
            "success_count": success_count,
        });

        Ok(json!({
            "DatasetJson": dataset.to_string()
        }))
    }
}

/// Extract the reasoning chain from OTS trajectory turns.
fn extract_reasoning_chain(trajectories: &[Value], target_id: &str) -> String {
    for trajectory in trajectories {
        let metadata = trajectory.get("metadata").unwrap_or(trajectory);
        let tid = metadata.get("trajectory_id")
            .or_else(|| metadata.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if tid != target_id { continue; }

        let turns = match trajectory.get("turns").and_then(Value::as_array) {
            Some(t) => t,
            None => continue,
        };

        let mut chain = Vec::new();
        for (i, turn) in turns.iter().enumerate() {
            // Extract reasoning from decisions
            if let Some(decisions) = turn.get("decisions").and_then(Value::as_array) {
                for decision in decisions {
                    let action = decision.get("action").and_then(Value::as_str).unwrap_or("?");
                    let reasoning = decision.get("reasoning").and_then(Value::as_str).unwrap_or("");
                    let outcome = decision.get("outcome").and_then(Value::as_str).unwrap_or("?");
                    if !reasoning.is_empty() {
                        chain.push(format!("  Turn {}: [{action} → {outcome}] {reasoning}", i + 1));
                    } else {
                        chain.push(format!("  Turn {}: [{action} → {outcome}]", i + 1));
                    }
                }
            }
            // Extract reasoning from messages
            if let Some(messages) = turn.get("messages").and_then(Value::as_array) {
                for msg in messages {
                    let role = msg.get("role").and_then(Value::as_str).unwrap_or("?");
                    let content = msg.get("content").and_then(Value::as_str).unwrap_or("");
                    if role == "assistant" && !content.is_empty() && content.len() < 500 {
                        chain.push(format!("  Turn {} (reasoning): {content}", i + 1));
                    }
                }
            }
        }
        return chain.join("\n");
    }
    String::new()
}

fn error_summaries_to_string(summaries: &[String]) -> String {
    if summaries.is_empty() { return String::new(); }
    format!("\nErrors:\n{}", summaries.join("\n"))
}

fn build_feedback(outcome: &str, workflow: &Value, entity_type: &str) -> (String, bool) {
    match outcome {
        "completed" => {
            let actions_total = workflow.get("actions_total").and_then(Value::as_u64).unwrap_or(0);
            (
                format!("PRESERVE: This workflow completed successfully ({actions_total} actions). \
                         Any spec mutation must not regress this working pattern."),
                true,
            )
        }
        "partial" => {
            let breakdown = workflow.get("breakdown");
            let suggestion = if let Some(bd) = breakdown {
                let action = bd.get("action").and_then(Value::as_str).unwrap_or("?");
                let from_state = bd.get("from_state").and_then(Value::as_str).unwrap_or("?");
                let error_kind = bd.get("error_kind").and_then(Value::as_str).unwrap_or("?");
                match error_kind {
                    "unknown_action" => format!(
                        "FIX: Action '{action}' is not defined in the {entity_type} spec. \
                         Add an [[action]] section for '{action}' with appropriate from states \
                         including '{from_state}'."
                    ),
                    "guard_rejection" => format!(
                        "FIX: Action '{action}' from state '{from_state}' was rejected by a guard. \
                         Review the guard conditions — the preconditions may be too restrictive \
                         for this workflow."
                    ),
                    _ => format!(
                        "FIX: Action '{action}' is not valid from state '{from_state}'. \
                         Add '{from_state}' to the 'from' list of the '{action}' action."
                    ),
                }
            } else {
                "Workflow partially completed but broke down. Review action availability.".to_string()
            };
            (suggestion, false)
        }
        _ => {
            let breakdown = workflow.get("breakdown");
            let suggestion = if let Some(bd) = breakdown {
                let action = bd.get("action").and_then(Value::as_str).unwrap_or("?");
                let error_kind = bd.get("error_kind").and_then(Value::as_str).unwrap_or("?");
                let msg = bd.get("message").and_then(Value::as_str).unwrap_or("?");
                format!("FIX: Workflow failed at first action. '{action}': {error_kind} — {msg}")
            } else {
                "Workflow failed completely. No actions succeeded.".to_string()
            };
            (suggestion, false)
        }
    }
}

/// Extract cross-trajectory patterns from replay results.
fn extract_patterns(replay_result: &Value) -> Value {
    let workflows = match replay_result.get("workflows").and_then(Value::as_array) {
        Some(w) => w,
        None => return json!({}),
    };

    let mut failure_actions: Vec<(String, String, u32)> = Vec::new(); // (action, state, count)
    let mut success_sequences: Vec<Vec<String>> = Vec::new();
    let mut missing_capabilities: Vec<String> = Vec::new();
    let mut guard_friction: Vec<String> = Vec::new();

    for workflow in workflows {
        let outcome = workflow.get("outcome").and_then(Value::as_str).unwrap_or("");

        if outcome == "completed" {
            // Track successful action sequences
            // (We don't have the full sequence in replay output, just stats)
            let final_state = workflow.get("final_state").and_then(Value::as_str).unwrap_or("?");
            let n = workflow.get("actions_total").and_then(Value::as_u64).unwrap_or(0);
            success_sequences.push(vec![format!("{n} actions → {final_state}")]);
        }

        if let Some(errors) = workflow.get("errors").and_then(Value::as_array) {
            for error in errors {
                let action = error.get("action").and_then(Value::as_str).unwrap_or("?").to_string();
                let from_state = error.get("from_state").and_then(Value::as_str).unwrap_or("?").to_string();
                let error_kind = error.get("error_kind").and_then(Value::as_str).unwrap_or("?");

                // Aggregate failures
                let found = failure_actions.iter_mut().find(|(a, s, _)| a == &action && s == &from_state);
                if let Some((_, _, count)) = found {
                    *count += 1;
                } else {
                    failure_actions.push((action.clone(), from_state.clone(), 1));
                }

                match error_kind {
                    "unknown_action" => {
                        if !missing_capabilities.contains(&action) {
                            missing_capabilities.push(action);
                        }
                    }
                    "guard_rejection" => {
                        let key = format!("{action} from {from_state}");
                        if !guard_friction.contains(&key) {
                            guard_friction.push(key);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Sort failure actions by frequency
    failure_actions.sort_by(|a, b| b.2.cmp(&a.2));

    json!({
        "common_failure_points": failure_actions.iter().map(|(action, state, count)| {
            json!({"action": action, "from_state": state, "occurrences": count})
        }).collect::<Vec<_>>(),
        "successful_patterns": success_sequences,
        "missing_capabilities": missing_capabilities,
        "guard_friction": guard_friction,
    })
}
