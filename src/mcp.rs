use anyhow::Result;
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

use crate::backend::Backend;
use crate::git;
use crate::status;
use crate::task;

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the subcontext MCP server over stdio.
///
/// Speaks JSON-RPC 2.0 (one message per line) and exposes a single tool,
/// `subcontext_status`, which returns the output of `subcontext status` for
/// the server's current working directory.
pub fn run(backend: &dyn Backend) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let id = req.get("id").cloned();
        let is_notification = id.is_none();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

        let result = handle_method(backend, method, &req);

        if is_notification {
            continue;
        }

        let reply = match result {
            Ok(Some(value)) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
            Ok(None) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("method not found: {method}")}
            }),
            Err(msg) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32603, "message": msg}
            }),
        };

        writeln!(out, "{}", serde_json::to_string(&reply)?)?;
        out.flush()?;
    }

    Ok(())
}

fn handle_method(
    backend: &dyn Backend,
    method: &str,
    req: &Value,
) -> Result<Option<Value>, String> {
    match method {
        "initialize" => Ok(Some(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {
                "name": "subcontext",
                "version": env!("CARGO_PKG_VERSION")
            }
        }))),
        "tools/list" => Ok(Some(json!({
            "tools": [{
                "name": "subcontext_status",
                "description": "Show current repo, worktree, and subcontext status for the project the server is running in.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }, {
                "name": "subcontext_deadlines",
                "description": "List deadlines for tasks not marked done or failed. Returns task names, statuses, deadlines, and importance values sorted by deadline.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "important_only": {
                            "type": "boolean",
                            "description": "If true, only show tasks with importance > 0. Default: false.",
                            "default": false
                        },
                        "horizon": {
                            "type": "string",
                            "description": "How far into the future to look for deadlines, as a human-readable duration (e.g. '1d', '2w', '3mo', '1y'). Suffixes: s, m (minutes), h, d, w, mo (months), y. Use '0' for only overdue deadlines. If omitted, shows all deadlines."
                        }
                    },
                    "additionalProperties": false
                }
            }]
        }))),
        "tools/call" => {
            let name = req
                .pointer("/params/name")
                .and_then(|n| n.as_str())
                .unwrap_or("");
            match name {
                "subcontext_status" => {
                    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
                    let text = match status::status_text(backend, &cwd) {
                        Ok(s) => s,
                        Err(e) => format!("Error: {e:#}"),
                    };
                    Ok(Some(json!({
                        "content": [{"type": "text", "text": text}],
                        "isError": false
                    })))
                }
                "subcontext_deadlines" => {
                    let params = req.get("params").and_then(|p| p.get("arguments"));
                    let important_only = params
                        .and_then(|p| p.get("important_only"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let horizon: Option<String> = params
                        .and_then(|p| p.get("horizon"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
                    let text =
                        match deadlines_text(backend, &cwd, important_only, horizon.as_deref()) {
                            Ok(s) => s,
                            Err(e) => format!("Error: {e:#}"),
                        };
                    Ok(Some(json!({
                        "content": [{"type": "text", "text": text}],
                        "isError": false
                    })))
                }
                other => Ok(Some(json!({
                    "content": [{"type": "text", "text": format!("unknown tool: {other}")}],
                    "isError": true
                }))),
            }
        }
        _ => Ok(None),
    }
}

fn deadlines_text(
    backend: &dyn Backend,
    cwd: &std::path::Path,
    important_only: bool,
    horizon: Option<&str>,
) -> anyhow::Result<String> {
    let root = git::find_main_git_root(backend, cwd)?;
    let scope = task::TaskScope::for_local(backend, &root)?;
    let entries = task::list_deadlines(&scope, important_only, horizon, None)?;
    Ok(task::format_deadlines(&entries))
}
