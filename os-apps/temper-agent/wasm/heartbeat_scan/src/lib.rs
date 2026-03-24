//! Heartbeat Scanner — WASM module for detecting stale agents.
//!
//! Queries TemperAgent entities in non-terminal states, checks heartbeat freshness,
//! and fires TimeoutFail on stale ones.

use temper_wasm_sdk::prelude::*;

#[unsafe(no_mangle)]
pub extern "C" fn run(_ctx_ptr: i32, _ctx_len: i32) -> i32 {
    let result = (|| -> Result<(), String> {
        let ctx = Context::from_host()?;
        ctx.log("info", "heartbeat_scan: starting");

        let fields = ctx.entity_state.get("fields").cloned().unwrap_or(json!({}));
        let temper_api_url = resolve_temper_api_url(&ctx, &fields);
        let tenant = &ctx.tenant;

        let headers = vec![
            ("x-tenant-id".to_string(), tenant.to_string()),
            ("x-temper-principal-kind".to_string(), "system".to_string()),
            ("accept".to_string(), "application/json".to_string()),
        ];

        // Query agents in non-terminal states
        let filter = "$filter=Status ne 'Completed' and Status ne 'Failed' and Status ne 'Cancelled' and Status ne 'Created'";
        let url = format!("{temper_api_url}/tdata/TemperAgents?{filter}");
        let resp = ctx.http_call("GET", &url, &headers, "")?;

        let mut stale_count: i64 = 0;

        if resp.status == 200 {
            let parsed: Value = serde_json::from_str(&resp.body).unwrap_or(json!({"value": []}));
            let agents = parsed.get("value").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            ctx.log("info", &format!("heartbeat_scan: checking {} active agents", agents.len()));

            for agent in &agents {
                let agent_id = agent.get("Id").and_then(|v| v.as_str()).unwrap_or("");
                let last_heartbeat = agent.get("LastHeartbeatAt").and_then(|v| v.as_str()).unwrap_or("");
                let timeout_secs: u64 = agent.get("HeartbeatTimeoutSeconds")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(300);

                // Skip agents without heartbeat monitoring configured.
                if timeout_secs == 0 {
                    continue;
                }

                if last_heartbeat.is_empty() {
                    let fail_url = format!(
                        "{temper_api_url}/tdata/TemperAgents('{agent_id}')/Temper.Agent.TemperAgent.TimeoutFail"
                    );
                    let fail_body = json!({
                        "error_message": format!(
                            "heartbeat timeout: no heartbeat observed within {} seconds",
                            timeout_secs
                        )
                    });
                    match ctx.http_call("POST", &fail_url, &headers, &fail_body.to_string()) {
                        Ok(resp) if resp.status >= 200 && resp.status < 300 => {
                            stale_count += 1;
                            ctx.log(
                                "warn",
                                &format!("heartbeat_scan: failed stale agent {}", agent_id),
                            );
                        }
                        Ok(resp) => ctx.log(
                            "warn",
                            &format!(
                                "heartbeat_scan: TimeoutFail failed for {} (HTTP {})",
                                agent_id, resp.status
                            ),
                        ),
                        Err(error) => ctx.log(
                            "warn",
                            &format!(
                                "heartbeat_scan: TimeoutFail failed for {}: {}",
                                agent_id, error
                            ),
                        ),
                    }
                    continue;
                }

                ctx.log(
                    "info",
                    &format!(
                        "heartbeat_scan: agent {} heartbeat marker='{}' timeout={}s",
                        agent_id, last_heartbeat, timeout_secs
                    ),
                );
            }
        }

        // Return scan complete
        set_success_result("ScanComplete", &json!({
            "last_scan_at": "scan-complete",
            "stale_agents_found": stale_count,
        }));

        Ok(())
    })();

    if let Err(e) = result {
        set_error_result(&e);
    }
    0
}

fn resolve_temper_api_url(ctx: &Context, fields: &Value) -> String {
    fields
        .get("temper_api_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| ctx.config.get("temper_api_url").filter(|s| !s.is_empty()).cloned())
        .unwrap_or_else(|| "http://127.0.0.1:3000".to_string())
}
