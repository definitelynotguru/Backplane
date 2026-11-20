//! ACP's terminal requests (`terminal/create`, `output`, `wait_for_exit`, `kill`, `release`) from an agent that runs its commands in the client's
//! terminals (Devin: agents/providers/devin.rs) as this thread's chat terminals: each command is an entry in the chat the person watches and types in.
use super::{emb, get, tools, Terms};
use crate::hub::Hub;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

fn sv<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(Value::as_str).unwrap_or("") }
/// Most of a screen an answer carries (its end).
const OUTPUT_MAX: usize = 1 << 20;

/// One word for a POSIX shell: as it is when it is plain, else in single quotes.
pub fn quote(a: &str) -> String {
    if !a.is_empty() && a.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b)) { a.into() } else { format!("'{}'", a.replace('\'', "'\\''")) }
}
/// The shell line for a create request: its environment, its command and its arguments.
pub fn command_line(p: &Value) -> String {
    let env: Vec<String> = p.get("env").and_then(Value::as_array).into_iter().flatten().filter(|e| { let n = sv(e, "name"); !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') && !n.as_bytes()[0].is_ascii_digit() })
        .map(|e| format!("{}={}", sv(e, "name"), quote(sv(e, "value")))).collect();
    let args: Vec<String> = p.get("args").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(quote).collect();
    let cmd = sv(p, "command");
    let cmd = if args.is_empty() { cmd.to_string() } else { format!("{} {}", quote(cmd), args.join(" ")) };
    if env.is_empty() { cmd } else { format!("export {}; {cmd}", env.join(" ")) }
}
/// ACP's exit status for a terminal's state (emb.rs): `done N` exited N, `stopped` killed; null while it runs.
/// terminal/kill and terminal/release stop only a command still running: a finished one keeps its status, and the shell after it stays the person's.
pub fn releases_kill(state: &str) -> bool { state == "running" }
pub fn exit_status(state: &str) -> Value {
    if let Some(n) = state.strip_prefix("done ") { return json!({"exitCode": n.trim().parse::<i64>().ok(), "signal": null}) }
    match state { "running" => Value::Null, "stopped" => json!({"exitCode": null, "signal": "SIGTERM"}), _ => json!({"exitCode": 127, "signal": null}) }
}
/// The end of a screen's text, at most `max` bytes, on a character boundary; and whether some was cut.
pub fn tail(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max { return (text.into(), false) }
    let mut i = text.len() - max; while !text.is_char_boundary(i) { i += 1 } (text[i..].into(), true)
}

/// The response line for one terminal request.
pub async fn answer(hub: &Arc<Hub>, thread: &str, cwd: &str, req: &Value, input: &mpsc::Sender<String>) -> String {
    let id = req.get("id").cloned().unwrap_or(Value::Null); let p = req.get("params").unwrap_or(&Value::Null);
    let r = match get(hub) { Some(t) => run(&t, hub, thread, cwd, sv(req, "method"), p, input).await, None => Err("terminals are not available".into()) };
    match r { Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}), Err(m) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":m}}) }.to_string()
}
async fn run(t: &Arc<Terms>, hub: &Arc<Hub>, thread: &str, cwd: &str, method: &str, p: &Value, input: &mpsc::Sender<String>) -> Result<Value, String> {
    if method == "terminal/create" {
        let cmd = command_line(p); if cmd.trim().is_empty() { return Err("command is empty".into()) }
        if t.running_of(thread).await.len() >= super::MAX_PER_THREAD { return Err("too many terminals are open in this chat".into()) }
        let dir = { let d = sv(p, "cwd"); if d.is_empty() { cwd } else { d } };
        return Ok(json!({"terminalId": tools::start(t, hub, thread, &emb::title_of("", &cmd), &cmd, dir, emb::cols(0), emb::rows(0)).await?}))
    }
    let key = emb::key(thread, sv(p, "terminalId"));
    let screen = || async { t.screen(&key).await.ok_or_else(|| format!("no terminal {} in this thread", sv(p, "terminalId"))) };
    match method {
        "terminal/output" => { let (text, state, _, _) = screen().await?; let (output, truncated) = tail(&text, OUTPUT_MAX); let mut r = json!({"output":output,"truncated":truncated}); let st = exit_status(&state); if !st.is_null() { r["exitStatus"] = st } Ok(r) }
        "terminal/wait_for_exit" => loop {
            let (_, state, _, _) = screen().await?; let st = exit_status(&state);
            if !st.is_null() { return Ok(st) }
            if input.is_closed() { return Err("the agent is gone".into()) }
            tokio::time::sleep(Duration::from_millis(250)).await;
        },
        "terminal/kill" | "terminal/release" => { let (_, state, _, _) = screen().await?; if releases_kill(&state) { t.kill(&key).await } Ok(json!({})) }
        _ => Err(format!("{method} is not supported")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn a_create_request_is_one_shell_line_with_its_environment_and_quoted_arguments() {
        assert_eq!(command_line(&json!({"command":"ls -la"})), "ls -la");
        assert_eq!(command_line(&json!({"command":"git","args":["commit","-m","it's done"]})), "git commit -m 'it'\\''s done'");
        assert_eq!(command_line(&json!({"command":"make","env":[{"name":"CI","value":"1"},{"name":"bad name","value":"x"},{"name":"A","value":"b c"}]})), "export CI=1 A='b c'; make");
    }
    #[test] fn releasing_a_finished_command_keeps_its_status_and_its_shell() {
        assert!(releases_kill("running")); assert!(!releases_kill("done 0")); assert!(!releases_kill("stopped"));
    }
    #[test] fn a_terminals_state_is_its_acp_exit_status() {
        assert_eq!(exit_status("running"), Value::Null);
        assert_eq!(exit_status("done 0"), json!({"exitCode":0,"signal":null})); assert_eq!(exit_status("done 2"), json!({"exitCode":2,"signal":null}));
        assert_eq!(exit_status("stopped"), json!({"exitCode":null,"signal":"SIGTERM"})); assert_eq!(exit_status("failed to start")["exitCode"], 127);
        assert_eq!(tail("abcdef", 3), ("def".to_string(), true)); assert_eq!(tail("é1", 2), ("1".to_string(), true)); assert_eq!(tail("ab", 3), ("ab".to_string(), false));
    }
}
