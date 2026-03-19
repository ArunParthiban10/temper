//! GEPA Replay WASM module.
//!
//! Replays full agent workflows (OTS trajectories) against a candidate IOA spec.
//! Each trajectory represents one agent session working on a task. The module
//! tracks per-workflow completion, identifies breakdown points, and aggregates
//! action-level stats across all workflows.
//!
//! Build: `cargo build -p gepa-replay-module --target wasm32-unknown-unknown --release`

use temper_wasm_sdk::prelude::*;

temper_module! {
    fn run(ctx: Context) -> Result<Value> {
        ctx.log("info", "gepa-replay: starting workflow-level replay");

        let fields = ctx.entity_state.get("fields").unwrap_or(&ctx.entity_state);
        let ioa_source = fields
            .get("SpecSource")
            .and_then(Value::as_str)
            .or_else(|| ctx.trigger_params.get("SpecSource").and_then(Value::as_str))
            .ok_or("entity state missing 'SpecSource'")?;

        let initial_state = ctx.trigger_params
            .get("initial_state")
            .and_then(Value::as_str)
            .unwrap_or("Backlog");

        // Accept full OTS trajectories (preferred) or flat action list (backward compat)
        let trajectories_val = ctx.trigger_params
            .get("Trajectories")
            .or_else(|| fields.get("Trajectories"));

        // Parse trajectories
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
                // Backward compat: flat TrajectoryActions → wrap in single workflow
                let flat_actions = ctx.trigger_params
                    .get("TrajectoryActions")
                    .or_else(|| fields.get("TrajectoryActions"));
                let flat_parsed: Vec<Value>;
                let flat: &[Value] = match flat_actions {
                    Some(Value::Array(arr)) => arr,
                    Some(Value::String(s)) => {
                        flat_parsed = serde_json::from_str(s).unwrap_or_default();
                        &flat_parsed
                    }
                    _ => return Err("no Trajectories or TrajectoryActions provided".into()),
                };
                // Wrap as a synthetic single trajectory
                trajectories_parsed = vec![json!({
                    "metadata": {"trajectory_id": "legacy-flat", "outcome": "unknown"},
                    "turns": flat.iter().map(|a| json!({
                        "decisions": [a]
                    })).collect::<Vec<_>>()
                })];
                &trajectories_parsed
            }
        };

        let mut workflows: Vec<Value> = Vec::new();
        let mut total_attempted: u32 = 0;
        let mut total_succeeded: u32 = 0;
        let mut total_guard_rejections: u32 = 0;
        let mut total_unknown_actions: u32 = 0;
        let mut workflows_completed: u32 = 0;
        let mut workflows_partial: u32 = 0;
        let mut workflows_failed: u32 = 0;
        let mut all_errors: Vec<Value> = Vec::new();

        for trajectory in trajectories {
            let metadata = trajectory.get("metadata").unwrap_or(trajectory);
            let trajectory_id = metadata.get("trajectory_id")
                .or_else(|| metadata.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let agent_goal = metadata.get("outcome")
                .or_else(|| metadata.get("goal"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");

            let turns = match trajectory.get("turns").and_then(Value::as_array) {
                Some(t) => t,
                None => continue,
            };

            let mut current_state = initial_state.to_string();
            let mut wf_attempted: u32 = 0;
            let mut wf_succeeded: u32 = 0;
            let mut breakdown: Option<Value> = None;
            let mut wf_errors: Vec<Value> = Vec::new();

            for (turn_idx, turn) in turns.iter().enumerate() {
                let decisions = match turn.get("decisions").and_then(Value::as_array) {
                    Some(d) => d,
                    None => {
                        // Flat action format: treat the turn itself as a decision
                        if turn.get("action").is_some() {
                            // Process single action directly
                            let action = turn.get("action").and_then(Value::as_str).unwrap_or("unknown");
                            let params = turn.get("params").cloned().unwrap_or(json!({}));
                            let params_str = params.to_string();
                            wf_attempted += 1;

                            let result = ctx.evaluate_spec(ioa_source, &current_state, action, &params_str)?;
                            let success = result.get("success").and_then(Value::as_bool).unwrap_or(false);

                            if success {
                                wf_succeeded += 1;
                                if let Some(new_state) = result.get("new_state").and_then(Value::as_str) {
                                    current_state = new_state.to_string();
                                }
                            } else if breakdown.is_none() {
                                let error_msg = result.get("error").and_then(Value::as_str).unwrap_or("unknown");
                                let error_kind = classify_error(error_msg);
                                breakdown = Some(json!({
                                    "turn_index": turn_idx,
                                    "action": action,
                                    "from_state": current_state,
                                    "error_kind": error_kind,
                                    "message": error_msg,
                                }));
                                wf_errors.push(breakdown.clone().unwrap_or_default());
                            }
                            continue;
                        }
                        continue;
                    }
                };

                for decision in decisions {
                    let action = decision.get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let params = decision.get("params").cloned().unwrap_or(json!({}));
                    let params_str = params.to_string();

                    wf_attempted += 1;

                    let result = ctx.evaluate_spec(ioa_source, &current_state, action, &params_str)?;
                    let success = result.get("success").and_then(Value::as_bool).unwrap_or(false);

                    if success {
                        wf_succeeded += 1;
                        if let Some(new_state) = result.get("new_state").and_then(Value::as_str) {
                            current_state = new_state.to_string();
                        }
                    } else {
                        let error_msg = result.get("error").and_then(Value::as_str).unwrap_or("unknown");
                        let error_kind = classify_error(error_msg);
                        let err = json!({
                            "turn_index": turn_idx,
                            "action": action,
                            "from_state": current_state,
                            "error_kind": error_kind,
                            "message": error_msg,
                        });
                        wf_errors.push(err.clone());
                        if breakdown.is_none() {
                            breakdown = Some(err);
                        }
                    }
                }
            }

            // Determine workflow outcome
            let outcome = if wf_attempted == 0 {
                "empty"
            } else if wf_errors.is_empty() {
                workflows_completed += 1;
                "completed"
            } else if wf_succeeded > 0 {
                workflows_partial += 1;
                "partial"
            } else {
                workflows_failed += 1;
                "failed"
            };

            total_attempted += wf_attempted;
            total_succeeded += wf_succeeded;
            for err in &wf_errors {
                let kind = err.get("error_kind").and_then(Value::as_str).unwrap_or("");
                if kind == "unknown_action" { total_unknown_actions += 1; }
                else if kind == "guard_rejection" { total_guard_rejections += 1; }
            }
            all_errors.extend(wf_errors.clone());

            workflows.push(json!({
                "trajectory_id": trajectory_id,
                "outcome": outcome,
                "agent_goal": agent_goal,
                "actions_total": wf_attempted,
                "actions_succeeded": wf_succeeded,
                "final_state": current_state,
                "breakdown": breakdown,
                "errors": wf_errors,
            }));
        }

        let workflows_attempted = workflows.len() as u32;
        let workflow_completion_rate = if workflows_attempted > 0 {
            workflows_completed as f64 / workflows_attempted as f64
        } else { 0.0 };
        let action_success_rate = if total_attempted > 0 {
            total_succeeded as f64 / total_attempted as f64
        } else { 0.0 };

        ctx.log("info", &format!(
            "gepa-replay: {workflows_completed}/{workflows_attempted} workflows completed, \
             {total_succeeded}/{total_attempted} actions succeeded"
        ));

        Ok(json!({
            "replay_result": {
                "workflows_attempted": workflows_attempted,
                "workflows_completed": workflows_completed,
                "workflows_partial": workflows_partial,
                "workflows_failed": workflows_failed,
                "workflow_completion_rate": workflow_completion_rate,
                "action_stats": {
                    "attempted": total_attempted,
                    "succeeded": total_succeeded,
                    "guard_rejections": total_guard_rejections,
                    "unknown_actions": total_unknown_actions,
                    "success_rate": action_success_rate,
                },
                "workflows": workflows,
                "errors": all_errors,
            }
        }))
    }
}

fn classify_error(error_msg: &str) -> &'static str {
    if error_msg.contains("not defined") || error_msg.contains("unknown action") || error_msg.contains("Unknown action") {
        "unknown_action"
    } else if error_msg.contains("guard") || error_msg.contains("Guard") {
        "guard_rejection"
    } else {
        "invalid_transition"
    }
}
