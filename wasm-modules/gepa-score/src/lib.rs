//! GEPA Score WASM module.
//!
//! Computes multi-objective scores from workflow-level replay results.
//! Primary objective: workflow_completion_rate (do full agent sessions succeed?).
//! Secondary: action-level success_rate, guard_pass_rate, coverage.
//!
//! Build: `cargo build -p gepa-score-module --target wasm32-unknown-unknown --release`

use temper_wasm_sdk::prelude::*;

temper_module! {
    fn run(ctx: Context) -> Result<Value> {
        ctx.log("info", "gepa-score: computing workflow-level objective scores");

        let fields = ctx.entity_state.get("fields").unwrap_or(&ctx.entity_state);

        // Read replay result — may be nested under "replay_result" key
        let replay_raw = ctx.trigger_params
            .get("ReplayResultJson")
            .or_else(|| ctx.trigger_params.get("replay_result"))
            .or_else(|| fields.get("ReplayResultJson"));
        let replay: Value = match replay_raw {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(json!({})),
            Some(v) => v.clone(),
            None => json!({}),
        };
        let replay_result = replay.get("replay_result").unwrap_or(&replay);

        let mut scores = json!({});

        // --- Primary objective: workflow completion rate ---
        let workflow_completion_rate = replay_result
            .get("workflow_completion_rate")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        scores["workflow_completion_rate"] = json!(workflow_completion_rate);

        let workflows_attempted = replay_result
            .get("workflows_attempted")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let workflows_completed = replay_result
            .get("workflows_completed")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let workflows_partial = replay_result
            .get("workflows_partial")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        // Partial credit: partial workflows count as 0.5
        let partial_adjusted_rate = if workflows_attempted > 0 {
            (workflows_completed as f64 + workflows_partial as f64 * 0.5) / workflows_attempted as f64
        } else {
            0.0
        };
        scores["partial_adjusted_rate"] = json!(partial_adjusted_rate);

        // --- Secondary objectives: action-level metrics ---
        let empty_obj = json!({});
        let action_stats = replay_result.get("action_stats").unwrap_or(&empty_obj);
        let actions_attempted = action_stats.get("attempted")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let succeeded = action_stats.get("succeeded")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let guard_rejections = action_stats.get("guard_rejections")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let unknown_actions = action_stats.get("unknown_actions")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        if actions_attempted > 0 {
            let success_rate = succeeded as f64 / actions_attempted as f64;
            scores["action_success_rate"] = json!(success_rate);

            let guard_pass_rate = 1.0 - (guard_rejections as f64 / actions_attempted as f64);
            scores["guard_pass_rate"] = json!(guard_pass_rate);
        }

        // Coverage: fraction of unique actions that are known
        let total_unique = succeeded + guard_rejections + unknown_actions;
        if total_unique > 0 {
            let coverage = 1.0 - (unknown_actions as f64 / total_unique as f64);
            scores["coverage"] = json!(coverage);
        }

        // --- Weighted sum ---
        // workflow_completion_rate has highest weight (1.5) — it's what matters most.
        // Partial-adjusted rate also weighted high (1.2) to reward partial progress.
        // Action-level metrics are supporting signals.
        let weights = ctx.entity_state.get("scoring_weights").cloned().unwrap_or(json!({
            "workflow_completion_rate": 1.5,
            "partial_adjusted_rate": 1.2,
            "action_success_rate": 1.0,
            "coverage": 0.8,
            "guard_pass_rate": 0.6,
        }));

        let mut total = 0.0_f64;
        let mut weight_sum = 0.0_f64;

        if let Some(weights_obj) = weights.as_object() {
            for (objective, weight_val) in weights_obj {
                let weight = weight_val.as_f64().unwrap_or(0.0);
                if let Some(score) = scores.get(objective).and_then(Value::as_f64) {
                    total += score * weight;
                    weight_sum += weight;
                }
            }
        }

        let weighted_sum = if weight_sum > 0.0 { total / weight_sum } else { 0.0 };
        scores["weighted_sum"] = json!(weighted_sum);

        ctx.log("info", &format!(
            "gepa-score: workflow_completion={workflow_completion_rate:.2}, \
             partial_adjusted={partial_adjusted_rate:.2}, weighted_sum={weighted_sum:.3}"
        ));

        Ok(json!({
            "ScoresJson": scores.to_string(),
        }))
    }
}
