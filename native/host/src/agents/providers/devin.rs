//! Devin (`devin acp`, from the ACP Registry: acp_registry.rs): acp.rs's client with what Devin adds under `cognition.ai/*`, as T3 Code's devinAcp.ts reads it.
//!  - a subagent's own updates (`cognition.ai/subagent_context`) go to its log, not the thread's timeline; a tool call that starts or ends one
//!    (`cognition.ai/subagent_started`, `cognition.ai/subagent_completed`) is its row (`subagent.*` shapes, agents/codex.rs);
//!  - the chunks of one message (`cognition.ai/streamingMessageId`) stay one message, a new id starts the next;
//!  - a tool with no title of its own is named by `cognition.ai/inferenceToolName`;
//!  - `plan_bypass` is never offered as an answer to a permission request;
//!  - the commands it runs are Backplane's chat terminals: ACP `terminal/*` requests go to the actor as `bp.term` (term/acp.rs answers them).
use super::acp::{self, s, Acp, AcpTurn};
use super::{Driver, Made, Reaction, TurnIn};
use serde_json::{json, Value};

pub const ID: &str = "devin";
pub const GONE: &str = "devin exited before the turn ended (is it signed in? run `devin auth login`)";
pub const INSTALL: &str = "Backplane fetches Devin from the ACP registry; sign in once with `devin auth login`.";

/// The initialize request: Backplane's terminals, and the Devin fields it reads.
pub fn init() -> String {
    acp::rpc(1, "initialize", json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":true,
        "_meta":{"cognition.ai/subagentSupport":true,"cognition.ai/messageGrouping":true}}}))
}
fn meta(v: &Value) -> &Value { v.get("_meta").unwrap_or(&Value::Null) }
/// The subagent an update belongs to ("" for the agent itself).
pub fn owner(u: &Value) -> String {
    let c = &meta(u)["cognition.ai/subagent_context"];
    let id = [s(c, "agentId"), s(c, "parentAgentId")].into_iter().find(|x| !x.is_empty()).unwrap_or("");
    if id == "root" { String::new() } else { id.to_string() }
}
/// A tool's title: its own, else its inference tool's name.
pub fn title(u: &Value) -> String {
    let t = s(u, "title"); let n = meta(u)["cognition.ai/inferenceToolName"].as_str().unwrap_or("");
    if (t.is_empty() || t == "Tool") && !n.is_empty() { n.into() } else { acp::gs(u, "title", acp::gs(u, "kind", "tool")).into() }
}
/// The answers Devin's permission request may get: all but `plan_bypass`.
pub fn answerable(o: &Value) -> Value {
    let mut o = o.clone();
    if let Some(os) = o.pointer_mut("/params/options").and_then(Value::as_array_mut) { os.retain(|x| s(x, "optionId").trim() != "plan_bypass") }
    o
}
/// A subagent's start or end on a tool call: the shapes for its row and its log.
pub fn subagent(u: &Value) -> Vec<Value> {
    let m = meta(u); let (st, done) = (&m["cognition.ai/subagent_started"], &m["cognition.ai/subagent_completed"]);
    let mut v = vec![];
    if !s(st, "agentId").is_empty() { v.push(json!({"type":"subagent.started","id":s(st, "agentId"),"title":acp::or(s(st, "title"), s(st, "task")),"prompt":s(st, "task"),"model":s(st, "model")})) }
    if !s(done, "agentId").is_empty() { v.push(json!({"type":"subagent.done","id":s(done, "agentId"),"text":s(done, "summary"),"error":done.get("success") == Some(&Value::Bool(false))})) }
    v
}

pub struct DevinDriver { pub turn: AcpTurn, pub a: Acp, pub model: String, msg: String, said: std::collections::HashMap<String, String>, calls: std::collections::HashMap<String, Value> }
impl DevinDriver { pub fn new() -> DevinDriver { DevinDriver { turn: AcpTurn::default(), a: Acp::new(), model: String::new(), msg: String::new(), said: Default::default(), calls: Default::default() } } }
impl DevinDriver {
    /// One session update, Devin's fields read first; the rest is acp.rs's.
    fn update(&mut self, u: &Value) -> Vec<Value> {
        let kind = s(u, "sessionUpdate"); let sub = owner(u);
        if !sub.is_empty() {
            return match kind {
                "agent_message_chunk" => { self.said.entry(sub).or_default().push_str(u.get("content").map(|c| s(c, "text")).unwrap_or("")); vec![] }
                "tool_call" => { let mut v = self.flush_sub(&sub); v.push(json!({"type":"subagent.item","id":sub,"item":s(u, "toolCallId"),"kind":title(u),"text":title(u)})); v.extend(subagent(u)); v }
                "tool_call_update" => subagent(u),
                _ => vec![],
            }
        }
        let mut v = vec![];
        match kind {
            "agent_message_chunk" => {
                let id = meta(u)["cognition.ai/streamingMessageId"].as_str().unwrap_or("");
                if !id.is_empty() && id != self.msg { if !self.msg.is_empty() && !self.a.buf.is_empty() { v.push(acp::event("item.completed", json!({"item":{"type":"agent_message","text":std::mem::take(&mut self.a.buf)}}))) } self.msg = id.into() }
                v.extend(acp::update(&mut self.a, u));
            }
            "tool_call" => { let mut t = u.clone(); t["title"] = json!(title(u)); self.seen(&t); v.extend(acp::update(&mut self.a, &t)); v.extend(subagent(u)) }
            "tool_call_update" => { self.seen(u); v.extend(subagent(u)); v.extend(acp::update(&mut self.a, u)) }
            _ => v.extend(acp::update(&mut self.a, u)),
        }
        v
    }
    /// What a tool call has said of itself so far (title, kind, rawInput), by its id.
    fn seen(&mut self, u: &Value) {
        let c = self.calls.entry(s(u, "toolCallId").into()).or_insert_with(|| json!({}));
        for k in ["title", "kind", "rawInput"] { if let Some(x) = u.get(k).filter(|x| !x.is_null()) { c[k] = x.clone() } }
    }
    /// A permission request names its tool call by id alone: the card gets what that call said of itself.
    fn known(&self, mut o: Value) -> Value {
        if let Some(tc) = o.pointer_mut("/params/toolCall").and_then(Value::as_object_mut) {
            if let Some(Value::Object(c)) = self.calls.get(tc.get("toolCallId").and_then(Value::as_str).unwrap_or("")) {
                for (k, x) in c { if tc.get(k).map_or(true, Value::is_null) { tc.insert(k.clone(), x.clone()); } }
            }
            let cmd = tc.get("_meta").and_then(|m| m.get("cognition.ai/editableCommand")).and_then(Value::as_str).map(String::from);
            if let Some(cmd) = cmd.filter(|c| !c.is_empty()) { tc.insert("rawInput".into(), json!({"command":cmd})); }
        }
        o
    }
    fn flush_sub(&mut self, sub: &str) -> Vec<Value> { match self.said.remove(sub).filter(|t| !t.trim().is_empty()) { Some(t) => vec![json!({"type":"subagent.item","id":sub,"item":"","kind":"say","text":t})], None => vec![] } }
}
impl Driver for DevinDriver {
    /// After the registry's own arguments (`acp`): the thread's model, when it names one.
    fn argv(&self, a: &TurnIn) -> Vec<String> { let mut v = vec![]; if !a.model.is_empty() && a.model != "default" { v.push("--model".into()); v.push(a.model.clone()) } v }
    fn start(&mut self, a: &TurnIn) -> Vec<String> {
        self.turn = AcpTurn { agent: ID.into(), cwd: a.cwd.clone(), mcp: a.mcp_url.clone(), prompt: a.prompt.clone(), text: a.text.clone(), session: a.session.clone(), ask: super::grok::asks(&a.runtime), servers: a.mcp.acp.clone(), model: String::new() };
        vec![init()]
    }
    fn on_line(&mut self, o: &Value) -> Reaction {
        let m = s(o, "method");
        if m.starts_with("terminal/") { return Reaction { send: vec![], shapes: vec![json!({"type":"bp.term","request":o})] } }
        if m == "session/update" && self.a.open_now() {
            let u = o.pointer("/params/update").unwrap_or(&Value::Null).clone();
            return Reaction { send: vec![], shapes: self.update(&u) };
        }
        let o = if m == "session/request_permission" { self.known(answerable(o)) } else { o.clone() };
        let r = acp::line(&mut self.a, &self.turn, &o);
        let mut shapes = vec![];
        if self.a.done { let subs: Vec<String> = self.said.keys().cloned().collect(); for k in subs { shapes.extend(self.flush_sub(&k)) } }
        shapes.extend(r.shapes);
        Reaction { send: r.send, shapes }
    }
    fn ask_info(&self, r: &Value) -> Option<super::AskInfo> { acp::ask_of(r) }
    fn answer(&mut self, r: &Value, answer: &str) -> Vec<String> { acp::answer_of(r, answer) }
    fn ends_on_turn_end(&self) -> bool { true }
    fn exit_why(&self, _code: i32) -> Option<String> { Some(GONE.into()) }
    fn install_hint(&self) -> Option<String> { Some(INSTALL.into()) }
    fn registry(&self) -> Option<&'static str> { Some(ID) }
}
pub fn driver(provider: &str) -> Option<Made> { if provider == ID { Some(Made { cli: ID, driver: Box::new(DevinDriver::new()) }) } else { None } }

#[cfg(test)]
mod tests {
    use super::*;
    fn open() -> DevinDriver {
        let mut d = DevinDriver::new(); d.start(&TurnIn { thread: "t".into(), cwd: "/tmp/p".into(), runtime: "approval-required".into(), text: "hi".into(), ..Default::default() });
        d.on_line(&json!({"jsonrpc":"2.0","id":1,"result":{}})); d.on_line(&json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"S"}})); d
    }
    fn up(d: &mut DevinDriver, u: Value) -> Vec<Value> { d.on_line(&json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"S","update":u}})).shapes }
    #[test] fn initialize_offers_terminals_and_reads_devins_fields() {
        let i: Value = serde_json::from_str(&init()).unwrap(); let c = &i["params"]["clientCapabilities"];
        assert_eq!(c["terminal"], true); assert_eq!(c["_meta"]["cognition.ai/subagentSupport"], true); assert_eq!(c["_meta"]["cognition.ai/messageGrouping"], true);
    }
    #[test] fn chunks_of_one_message_stay_one_and_a_new_id_starts_the_next() {
        let mut d = open(); let m = |id: &str, t: &str| json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":t},"_meta":{"cognition.ai/streamingMessageId":id}});
        assert_eq!(up(&mut d, m("a", "Hel"))[0]["type"], "item.delta"); up(&mut d, m("a", "lo"));
        let v = up(&mut d, m("b", "Next")); assert_eq!(v[0], json!({"type":"item.completed","item":{"type":"agent_message","text":"Hello"}})); assert_eq!(v[1]["text"], "Next");
        let end = d.on_line(&json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}})).shapes; assert_eq!(end[0]["item"]["text"], "Next"); assert_eq!(end[1]["type"], "turn.completed");
    }
    #[test] fn a_tool_with_no_title_is_named_by_its_inference_tool() {
        let mut d = open();
        let v = up(&mut d, json!({"sessionUpdate":"tool_call","toolCallId":"c1","title":"Tool","_meta":{"cognition.ai/inferenceToolName":"grep"}})); assert_eq!(v[0]["item"]["command"], "grep");
        let v = up(&mut d, json!({"sessionUpdate":"tool_call","toolCallId":"c2","title":"Read a.rs","_meta":{"cognition.ai/inferenceToolName":"read"}})); assert_eq!(v[0]["item"]["command"], "Read a.rs");
    }
    #[test] fn a_subagent_has_its_row_its_steps_in_its_log_and_its_answer() {
        let mut d = open();
        let v = up(&mut d, json!({"sessionUpdate":"tool_call","toolCallId":"c1","title":"Explore","_meta":{"cognition.ai/subagent_started":{"agentId":"sa1","task":"Look around","title":"Explore","model":"m"}}}));
        assert!(v.contains(&json!({"type":"subagent.started","id":"sa1","title":"Explore","prompt":"Look around","model":"m"})));
        let ctx = json!({"cognition.ai/subagent_context":{"agentId":"sa1","parentAgentId":"root"}});
        assert!(up(&mut d, json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Found it"},"_meta":ctx})).is_empty(), "a subagent's words are not the thread's");
        let v = up(&mut d, json!({"sessionUpdate":"tool_call","toolCallId":"k1","title":"ls","_meta":ctx}));
        assert_eq!(v[0], json!({"type":"subagent.item","id":"sa1","item":"","kind":"say","text":"Found it"})); assert_eq!(v[1]["kind"], "ls");
        let v = up(&mut d, json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed","_meta":{"cognition.ai/subagent_completed":{"agentId":"sa1","success":false,"summary":"No luck"}}}));
        assert_eq!(v[0], json!({"type":"subagent.done","id":"sa1","text":"No luck","error":true})); assert_eq!(v[1]["type"], "tool.done");
    }
    #[test] fn plan_bypass_is_never_offered_and_terminal_requests_go_to_the_actor() {
        let mut d = open();
        let r = d.on_line(&json!({"jsonrpc":"2.0","id":9,"method":"session/request_permission","params":{"toolCall":{"title":"Exit plan"},"options":[{"optionId":"plan_bypass","kind":"allow_always"},{"optionId":"ok","kind":"allow_once"},{"optionId":"no","kind":"reject_once"}]}}));
        let req = r.shapes[0]["request"].as_str().unwrap(); assert!(!req.contains("plan_bypass") && req.contains("\"ok\""));
        assert_eq!(acp::answer_of(&r.shapes[0]["request"], "session"), vec![r#"{"jsonrpc":"2.0","id":9,"result":{"outcome":{"outcome":"selected","optionId":"ok"}}}"#.to_string()]);
        let t = d.on_line(&json!({"jsonrpc":"2.0","id":10,"method":"terminal/create","params":{"sessionId":"S","command":"ls"}})); assert_eq!(t.shapes[0]["type"], "bp.term"); assert!(t.send.is_empty());
        assert_eq!(d.argv(&TurnIn { model: "opus".into(), ..Default::default() }), ["--model", "opus"]); assert!(d.argv(&TurnIn { model: "default".into(), ..Default::default() }).is_empty());
    }
    #[test] fn a_permission_card_names_the_tool_call_it_is_for() {
        let mut d = open();
        up(&mut d, json!({"sessionUpdate":"tool_call","toolCallId":"c7","title":"Ran rm","kind":"execute"}));
        let ask = |d: &mut DevinDriver, tc: Value| { let r = d.on_line(&json!({"jsonrpc":"2.0","id":4,"method":"session/request_permission","params":{"sessionId":"s","toolCall":tc,"options":[{"optionId":"allow_once","kind":"allow_once"}]}})); let a = d.ask_info(&r.shapes[0]["request"]).unwrap(); (a.tool, a.detail) };
        assert_eq!(ask(&mut d, json!({"toolCallId":"c7"})), ("execute".to_string(), "Ran rm".to_string()));
        assert_eq!(ask(&mut d, json!({"toolCallId":"c7","_meta":{"cognition.ai/editableCommand":"rm -f notes.txt"}})), ("execute".to_string(), "rm -f notes.txt".to_string()));
    }
}
