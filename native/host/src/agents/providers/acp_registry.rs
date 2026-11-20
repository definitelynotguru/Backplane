//! The ACP Registry (https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json): agents published as one archive per platform, each run
//! over stdio with the ACP client (acp.rs). As T3 Code's provider-acp-registry: the registry read (kept for FRESH seconds, the last good copy when it
//! cannot be reached), the agent's archive fetched once per version into `<home>/acp-registry/<agent>/<version>/`, launched as its entry says.
//! curl and tar run through proc.rs (update/svc.rs); the archive's member names are checked before anything is unpacked; a version is in place
//! only once it is whole (one rename), so a fetch cut short is fetched again and two turns starting at once both end on the same folder.
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const URL: &str = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";
const MAX_REGISTRY: u64 = 4 << 20;
const MAX_ARCHIVE: u64 = 1 << 30;
const FRESH: u64 = 6 * 3600;
const READY: &str = ".ready";

/// One agent's distribution for this platform, as the registry names it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry { pub id: String, pub version: String, pub archive: String, pub cmd: String, pub args: Vec<String> }
/// What a turn starts: the program in its version's folder and the arguments the registry gives it.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch { pub program: PathBuf, pub args: Vec<String>, pub version: String }

/// The registry's platform key for an OS and architecture (Rust's `std::env::consts` names).
pub fn platform(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("linux", "x86_64") => "linux-x86_64", ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "darwin-x86_64", ("macos", "aarch64") => "darwin-aarch64",
        ("windows", "x86_64") => "windows-x86_64", ("windows", "aarch64") => "windows-aarch64",
        _ => return None,
    })
}
pub fn here() -> Option<&'static str> { platform(std::env::consts::OS, std::env::consts::ARCH) }

fn word_ok(s: &str, extra: &[u8]) -> bool { !s.is_empty() && s.len() <= 64 && !s.starts_with('.') && !s.contains("..") && s.bytes().all(|b| b.is_ascii_alphanumeric() || extra.contains(&b)) }
pub fn id_ok(s: &str) -> bool { word_ok(s, b"-_") }
pub fn version_ok(s: &str) -> bool { word_ok(s, b".-+_") }
/// An archive is an https link to a .tar.gz, .tgz or .zip, with no credentials or spaces in it.
pub fn archive_ok(s: &str) -> bool {
    s.len() <= 2048 && s.strip_prefix("https://").is_some_and(|r| !r.is_empty() && !r.split('/').next().unwrap_or("").contains('@'))
        && !s.chars().any(|c| c.is_whitespace() || c.is_control()) && [".tar.gz", ".tgz", ".zip"].iter().any(|e| s.ends_with(e))
}
/// The command's path inside the version's folder: relative, `/` or `\` between parts, none of them `..`.
pub fn cmd_parts(cmd: &str) -> Option<Vec<&str>> {
    let ps: Vec<&str> = cmd.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
    let abs = cmd.starts_with('/') || cmd.starts_with('\\') || cmd.contains(':');
    if abs || ps.is_empty() || ps.iter().any(|p| *p == ".." || p.len() > 255) { None } else { Some(ps) }
}

/// The entry for `id` on `plat` in a registry document. Err says what is wrong with it.
pub fn entry(doc: &Value, id: &str, plat: &str) -> Result<Entry, String> {
    let a = doc.get("agents").and_then(Value::as_array).and_then(|xs| xs.iter().find(|a| a.get("id").and_then(Value::as_str) == Some(id))).ok_or_else(|| format!("{id} is not in the ACP registry"))?;
    let version = a.get("version").and_then(Value::as_str).filter(|v| version_ok(v)).ok_or_else(|| format!("the ACP registry gives {id} no usable version"))?;
    let b = a.pointer("/distribution/binary").and_then(|b| b.get(plat)).ok_or_else(|| format!("the ACP registry has no {id} build for {plat}"))?;
    let archive = b.get("archive").and_then(Value::as_str).filter(|u| archive_ok(u)).ok_or_else(|| format!("the ACP registry's {id} archive is not an https .tar.gz or .zip"))?;
    let cmd = b.get("cmd").and_then(Value::as_str).filter(|c| cmd_parts(c).is_some()).ok_or_else(|| format!("the ACP registry's {id} command is not inside its archive"))?;
    let args = match b.get("args") { None | Some(Value::Null) => vec![], Some(Value::Array(xs)) if xs.len() <= 32 => xs.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<Vec<_>>>().ok_or("the ACP registry's arguments are not text")?, _ => return Err("the ACP registry's arguments are not a short list".into()) };
    Ok(Entry { id: id.into(), version: version.into(), archive: archive.into(), cmd: cmd.into(), args })
}

pub fn root(home: &Path) -> PathBuf { home.join("acp-registry") }
/// Where a version is kept and its program in it.
pub fn version_dir(home: &Path, id: &str, version: &str) -> PathBuf { root(home).join(id).join(version) }
pub fn program_in(dir: &Path, cmd: &str) -> PathBuf {
    let mut p = dir.to_path_buf(); for c in cmd_parts(cmd).unwrap_or_default() { p.push(c) }
    if cfg!(windows) && p.extension().is_none() { p.set_extension("exe"); } p
}
fn launch_of(home: &Path, e: &Entry) -> Launch { Launch { program: program_in(&version_dir(home, &e.id, &e.version), &e.cmd), args: e.args.clone(), version: e.version.clone() } }
/// The version is whole: its mark and its program are there.
pub fn ready(home: &Path, e: &Entry) -> bool { let d = version_dir(home, &e.id, &e.version); d.join(READY).is_file() && launch_of(home, e).program.is_file() }

fn now_s() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }
fn age(p: &Path) -> Option<u64> { let m = std::fs::metadata(p).ok()?.modified().ok()?; Some(now_s().saturating_sub(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs())) }
fn stage(home: &Path, what: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    root(home).join(format!(".{what}-{}-{}", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)))
}

/// The agents Backplane runs from the registry.
pub const AGENTS: &[&str] = &[super::devin::ID];
/// The program of the version of `id` kept here, by the kept registry (no fetch): for what runs it outside a turn (its sign-in, its model list).
pub fn kept(home: &Path, id: &str) -> Option<PathBuf> {
    let doc = serde_json::from_slice::<Value>(&std::fs::read(root(home).join("registry.json")).ok()?).ok()?;
    let e = entry(&doc, id, here()?).ok()?; ready(home, &e).then(|| launch_of(home, &e).program)
}
/// Where `id`'s program is: a registry agent's kept version (outside test mode), else the CLI on PATH.
pub fn locate(home: &Path, cli: &crate::agents::proc::Cli, id: &str) -> Option<PathBuf> {
    if AGENTS.contains(&id) && cli.dir().is_none() { if let Some(p) = kept(home, id) { return Some(p) } }
    cli.resolve(id).ok()
}
/// As locate, a registry agent fetched first when no version is kept.
pub async fn locate_or_fetch(home: &Path, cli: &crate::agents::proc::Cli, id: &'static str) -> Result<PathBuf, String> {
    if let Some(p) = locate(home, cli, id) { return Ok(p) }
    if !AGENTS.contains(&id) || cli.dir().is_some() { return Err(format!("{id} is not installed")) }
    let h = home.to_path_buf(); tokio::task::spawn_blocking(move || ensure(&h, id)).await.map_err(|e| e.to_string())?.map(|l| l.program)
}

/// The registry document: the kept copy while it is fresh, else read again (the kept copy when that fails).
pub fn document(home: &Path) -> Result<Value, String> {
    let kept = root(home).join("registry.json");
    let parse = |b: &[u8]| serde_json::from_slice::<Value>(b).ok().filter(|v| v.get("agents").is_some_and(Value::is_array));
    if age(&kept).is_some_and(|a| a < FRESH) { if let Some(v) = std::fs::read(&kept).ok().and_then(|b| parse(&b)) { return Ok(v) } }
    std::fs::create_dir_all(root(home)).map_err(|e| e.to_string())?;
    let tmp = stage(home, "registry");
    let got = crate::update::svc::curl(URL, &tmp, MAX_REGISTRY).map_err(|_| "could not reach the ACP registry".to_string()).and_then(|_| std::fs::read(&tmp).map_err(|e| e.to_string()));
    match got.as_deref().ok().and_then(parse) {
        Some(v) => { let _ = std::fs::rename(&tmp, &kept); Ok(v) }
        None => { let _ = std::fs::remove_file(&tmp); std::fs::read(&kept).ok().and_then(|b| parse(&b)).ok_or_else(|| got.err().unwrap_or_else(|| "the ACP registry sent something that is not a registry".into())) }
    }
}

/// The agent `id` ready to start: fetched and unpacked first when this version is not kept yet.
pub fn ensure(home: &Path, id: &str) -> Result<Launch, String> {
    let plat = here().ok_or("the ACP registry has no builds for this platform")?;
    let e = entry(&document(home)?, id, plat)?;
    if !ready(home, &e) { install(home, &e)? }
    Ok(launch_of(home, &e))
}
fn install(home: &Path, e: &Entry) -> Result<(), String> {
    let st = stage(home, &e.id); let _ = std::fs::remove_dir_all(&st);
    let r = (|| {
        let unpack = st.join("unpack"); std::fs::create_dir_all(&unpack).map_err(|x| x.to_string())?;
        let a = st.join(if e.archive.ends_with(".zip") { "archive.zip" } else { "archive.tar.gz" });
        crate::update::svc::curl(&e.archive, &a, MAX_ARCHIVE).map_err(|_| format!("could not download {} {}", e.id, e.version))?;
        crate::update::svc::tar(&a, &unpack)?;
        let prog = program_in(&unpack, &e.cmd);
        if !prog.is_file() { return Err(format!("the {} archive has no {}", e.id, e.cmd)) }
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).map_err(|x| x.to_string())?; }
        std::fs::write(unpack.join(READY), &e.archive).map_err(|x| x.to_string())?;
        let to = version_dir(home, &e.id, &e.version);
        if ready(home, e) { return Ok(()) } // another turn put it in place first
        let _ = std::fs::remove_dir_all(&to); std::fs::create_dir_all(to.parent().unwrap()).map_err(|x| x.to_string())?;
        std::fs::rename(&unpack, &to).or_else(|x| if ready(home, e) { Ok(()) } else { Err(x.to_string()) })
    })();
    let _ = std::fs::remove_dir_all(&st); r
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn doc() -> Value { json!({"version":"1.0.0","agents":[{"id":"devin","version":"3000.14.30821","distribution":{"binary":{
        "linux-x86_64":{"archive":"https://static.example.com/cli/3000.14.30821/devin-x86_64-unknown-linux.tar.gz","cmd":"./bin/devin","args":["acp"]},
        "windows-x86_64":{"archive":"https://static.example.com/cli/3000.14.30821/devin-x86_64-pc-windows.zip","cmd":"./bin\\devin.exe","args":["acp"]}}}},
        {"id":"npx-only","version":"1.0.0","distribution":{"npx":{"package":"x"}}}]}) }
    #[test] fn the_registry_entry_for_this_platform_names_its_archive_command_and_arguments() {
        let e = entry(&doc(), "devin", "linux-x86_64").unwrap();
        assert_eq!((e.version.as_str(), e.cmd.as_str(), e.args.clone()), ("3000.14.30821", "./bin/devin", vec!["acp".to_string()]));
        assert!(entry(&doc(), "devin", "darwin-aarch64").unwrap_err().contains("no devin build for darwin-aarch64"));
        assert!(entry(&doc(), "npx-only", "linux-x86_64").is_err() && entry(&doc(), "nobody", "linux-x86_64").unwrap_err().contains("not in the ACP registry"));
        assert_eq!(platform("linux", "x86_64"), Some("linux-x86_64")); assert_eq!(platform("macos", "aarch64"), Some("darwin-aarch64")); assert_eq!(platform("freebsd", "x86_64"), None);
    }
    #[test] fn an_entry_that_would_leave_its_folder_or_fetch_over_http_is_refused() {
        let with = |f: &dyn Fn(&mut Value)| { let mut d = doc(); f(&mut d["agents"][0]); entry(&d, "devin", "linux-x86_64") };
        assert!(with(&|a| a["distribution"]["binary"]["linux-x86_64"]["archive"] = json!("http://static.example.com/devin.tar.gz")).is_err());
        assert!(with(&|a| a["distribution"]["binary"]["linux-x86_64"]["archive"] = json!("https://user:pw@static.example.com/devin.tar.gz")).is_err());
        assert!(with(&|a| a["distribution"]["binary"]["linux-x86_64"]["cmd"] = json!("../../bin/sh")).is_err());
        assert!(with(&|a| a["distribution"]["binary"]["linux-x86_64"]["cmd"] = json!("/bin/sh")).is_err());
        assert!(with(&|a| a["distribution"]["binary"]["linux-x86_64"]["cmd"] = json!("C:\\x.exe")).is_err());
        assert!(with(&|a| a["version"] = json!("../1")).is_err());
        assert!(with(&|a| a["distribution"]["binary"]["linux-x86_64"]["args"] = json!([1])).is_err());
    }
    #[test] fn each_version_has_a_folder_of_its_own_and_counts_only_once_whole() {
        let home = crate::plat::tmp::canonical("acp-registry-cache");
        let e = entry(&doc(), "devin", "linux-x86_64").unwrap();
        let d = version_dir(&home, "devin", &e.version); assert!(d.ends_with("acp-registry/devin/3000.14.30821"));
        assert!(!ready(&home, &e));
        std::fs::create_dir_all(d.join("bin")).unwrap(); std::fs::write(d.join("bin/devin"), "x").unwrap(); assert!(!ready(&home, &e), "no mark: a fetch cut short");
        std::fs::write(d.join(READY), "").unwrap(); assert!(ready(&home, &e));
        assert_eq!(launch_of(&home, &e).program, d.join("bin").join(if cfg!(windows) { "devin.exe" } else { "devin" }));
        assert_eq!(program_in(Path::new("/v"), "./bin\\devin.exe"), Path::new("/v").join("bin").join("devin.exe"));
    }
}
