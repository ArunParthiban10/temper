//! Claude Code local CLI adapter.

use std::time::Instant;

use async_trait::async_trait;
use tokio::process::Command;

use super::{AdapterContext, AdapterError, AdapterResult, AgentAdapter};

/// Adapter implementation for local `claude` CLI execution.
#[derive(Debug, Default)]
pub struct ClaudeCodeAdapter;

#[async_trait]
impl AgentAdapter for ClaudeCodeAdapter {
    fn adapter_type(&self) -> &str {
        "claude_code"
    }

    async fn execute(&self, ctx: AdapterContext) -> Result<AdapterResult, AdapterError> {
        let checkpoint = checkpoint_from_state(&ctx);
        let run = run_claude(&ctx, checkpoint.as_deref()).await;

        match run {
            Ok(result) => Ok(result),
            Err(e) => {
                // Retry once without resume when the checkpoint/session is stale.
                if checkpoint.is_some()
                    && e.to_string()
                        .to_ascii_lowercase()
                        .contains("unknown session")
                {
                    run_claude(&ctx, None).await
                } else {
                    Err(e)
                }
            }
        }
    }
}

fn checkpoint_from_state(ctx: &AdapterContext) -> Option<String> {
    ctx.entity_state
        .get("fields")
        .and_then(|v| v.get("checkpoint"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

async fn run_claude(
    ctx: &AdapterContext,
    resume: Option<&str>,
) -> Result<AdapterResult, AdapterError> {
    let started = Instant::now(); // determinism-ok: wall-clock timing for external process

    let command_name = ctx
        .integration_config
        .get("command")
        .map(String::as_str)
        .unwrap_or("claude");

    let mut command = Command::new(command_name);
    // determinism-ok: process spawn for agent execution
    command
        .arg("--print")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose");

    if let Some(session) = resume {
        command.arg("--resume").arg(session);
    }

    if let Some(skills_path) = ctx.integration_config.get("skills_path")
        && !skills_path.trim().is_empty()
    {
        command.arg("--add-dir").arg(skills_path);
    }

    if let Some(extra_args) = ctx.integration_config.get("args") {
        for arg in extra_args.split_whitespace() {
            command.arg(arg);
        }
    }

    if let Some(workdir) = ctx.integration_config.get("workdir")
        && !workdir.trim().is_empty()
    {
        command.current_dir(workdir);
    }

    // Pass platform-minted credential for identity resolution (ADR-0033).
    // The spawned agent uses this token to authenticate back to Temper,
    // and the platform resolves it to a verified identity.
    if let Some(ref api_key) = ctx.agent_ctx.agent_api_key {
        command.env("TEMPER_API_KEY", api_key);
    }
    command
        .env("TEMPER_RUN_ID", ctx.entity_id.clone())
        .env("TEMPER_TASK_ID", ctx.entity_id.clone())
        .env("TEMPER_WAKE_REASON", ctx.trigger_action.clone());

    // Generic context passing: expose entity state and trigger params as env vars.
    // Any adapter integration can reference these — not specific to any use case.
    pass_entity_context_as_env(&mut command, ctx);

    // Build prompt with template interpolation.
    if let Some(template) = ctx.integration_config.get("prompt")
        && !template.trim().is_empty()
    {
        let entity_fields = ctx.entity_state.get("fields").unwrap_or(&ctx.entity_state);
        let prompt = interpolate_prompt(template, &ctx.trigger_params, entity_fields);
        command.arg(prompt);
    }

    let output = command
        .output()
        .await
        .map_err(|e| AdapterError::Invocation(format!("failed to spawn '{command_name}': {e}")))?;

    let duration_ms = started.elapsed().as_millis() as u64;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        let callback_params = parse_stream_json_output(&stdout);
        Ok(AdapterResult::success(callback_params, duration_ms))
    } else {
        let detail = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        Ok(AdapterResult::failure(detail, duration_ms))
    }
}

/// Interpolate `{key}` placeholders in a prompt template from trigger params and entity fields.
///
/// Lookup order: trigger_params → entity_state.fields → integration_config.
/// String values are inserted directly. Objects/arrays are pretty-printed as JSON.
/// Unresolved placeholders are left as-is so the LLM sees what was expected.
fn interpolate_prompt(
    template: &str,
    trigger_params: &serde_json::Value,
    entity_fields: &serde_json::Value,
) -> String {
    let mut result = String::with_capacity(template.len() * 2);
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '{' {
            // Collect the key name until '}'
            let mut key = String::new();
            let mut found_close = false;
            for inner in chars.by_ref() {
                if inner == '}' {
                    found_close = true;
                    break;
                }
                key.push(inner);
            }
            if !found_close || key.is_empty() {
                // Malformed placeholder — emit as-is
                result.push('{');
                result.push_str(&key);
                continue;
            }

            // Look up value: trigger_params first, then entity fields
            let value = trigger_params.get(&key).or_else(|| entity_fields.get(&key));

            match value {
                Some(serde_json::Value::String(s)) => result.push_str(s),
                Some(v) => {
                    // Pretty-print objects/arrays for readability
                    if let Ok(pretty) = serde_json::to_string_pretty(v) {
                        result.push_str(&pretty);
                    } else {
                        result.push_str(&v.to_string());
                    }
                }
                None => {
                    // Unresolved — leave placeholder visible
                    result.push('{');
                    result.push_str(&key);
                    result.push('}');
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Pass entity state fields and trigger params as environment variables.
///
/// Scalar string values become `TEMPER_FIELD_<KEY>` and `TEMPER_PARAM_<KEY>`.
/// This is a generic capability — any spawned agent process can read these.
fn pass_entity_context_as_env(command: &mut Command, ctx: &AdapterContext) {
    if let Some(fields) = ctx.entity_state.get("fields").and_then(|v| v.as_object()) {
        for (key, value) in fields {
            if let Some(s) = value.as_str() {
                // Cap env var values at 4KB to avoid OS limits
                if s.len() <= 4096 {
                    command.env(format!("TEMPER_FIELD_{}", key.to_uppercase()), s);
                }
            }
        }
    }
    if let Some(params) = ctx.trigger_params.as_object() {
        for (key, value) in params {
            if let Some(s) = value.as_str() {
                if s.len() <= 4096 {
                    command.env(format!("TEMPER_PARAM_{}", key.to_uppercase()), s);
                }
            }
        }
    }
}

fn parse_stream_json_output(stdout: &str) -> serde_json::Value {
    let mut merged = serde_json::Map::new();
    let mut last_json: Option<serde_json::Value> = None;

    for line in stdout.lines() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(obj) = value.as_object() {
                for (k, v) in obj {
                    merged.insert(k.clone(), v.clone());
                }
            }
            last_json = Some(value);
        }
    }

    let mut out = serde_json::json!({
        "raw_output": stdout.trim(),
    });

    if let Some(obj) = out.as_object_mut() {
        if !merged.is_empty() {
            obj.insert("stream".to_string(), serde_json::Value::Object(merged));
        }
        if let Some(last) = last_json {
            obj.insert("result".to_string(), last);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpolate_simple_string() {
        let template = "Hello {name}, you are working on {task}";
        let params = serde_json::json!({"name": "Claude"});
        let fields = serde_json::json!({"task": "evolution"});
        let result = interpolate_prompt(template, &params, &fields);
        assert_eq!(result, "Hello Claude, you are working on evolution");
    }

    #[test]
    fn test_interpolate_missing_key() {
        let template = "Spec: {spec_source}, Missing: {unknown}";
        let params = serde_json::json!({"spec_source": "[automaton]\nname = \"Issue\""});
        let fields = serde_json::json!({});
        let result = interpolate_prompt(template, &params, &fields);
        assert!(result.contains("[automaton]"));
        assert!(result.contains("{unknown}"));
    }

    #[test]
    fn test_interpolate_json_object() {
        let template = "Patterns: {patterns}";
        let params = serde_json::json!({"patterns": {"failures": 3, "successes": 7}});
        let fields = serde_json::json!({});
        let result = interpolate_prompt(template, &params, &fields);
        assert!(result.contains("failures"));
        assert!(result.contains("successes"));
    }

    #[test]
    fn test_interpolate_trigger_params_priority() {
        let template = "Value: {key}";
        let params = serde_json::json!({"key": "from_trigger"});
        let fields = serde_json::json!({"key": "from_entity"});
        let result = interpolate_prompt(template, &params, &fields);
        assert_eq!(result, "Value: from_trigger");
    }
}
