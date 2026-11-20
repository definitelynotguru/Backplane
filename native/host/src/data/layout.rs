//! data/1 docking layout (feature `layout`): the hub's tile layout (`present::layout`) for phones, tablets and browsers.
//!
//! - `viewer.layout {thread, bounds:[w,h], page?, diff?}` → the device's layout for `thread`, fitted to the viewer area in CSS px.
//! - `viewer.layout.act {thread, op, bounds, …}` → the same reply after the op, or a refusal `{code, key, message}`.
//!
//! One shared layout per (device, thread): the TREE is stored, never the fit; every connection sends its own bounds and gets its own fit
//! (`present::layout_fit`). A change is pushed as `{t:"layout", thread, rev}` to the device's connections watching the thread; a client that sees a rev it
//! does not hold asks again. Multi-tile layouts survive a hub restart (a small file under the hub's home, dropped when the device is revoked or gone).
//!
//! The device's layout is NEVER a desktop session's: the hub's pure layout operations run over a synthetic session built here from this device's own
//! drafts, and only their `@view*:<thread>` drafts come back into this device's store. Nothing reads or writes a presenter session.
use super::*;
use crate::present::layout::{self, Layout, Refusal};
use crate::present::layout_fit::{self, Fitted};
use crate::present::viewer::{self, LayoutOut};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Most tiles one layout holds (the hub's own limit lands with PR 1461's `layout::MAX_TILES`; the same number).
pub const MAX_TILES: usize = 8;
/// A device keeps layouts for this many threads (least recently used first out) and the hub for this many devices.
const THREADS_PER_DEVICE: usize = 32;
const DEVICES: usize = 64;
/// The page kinds a data/1 layout holds: viewers only. A KiCad frame, the Simulator, the terminal and the hub's browser pane are never tiles.
const TILE_KINDS: &[&str] = &["board", "schematic", "3d", "gerbers", "mech", "files", "diff", "review"];
/// The ops of `viewer.layout.act`.
const OPS: &[&str] = &["add", "close", "focus", "resize", "swap", "move", "overlay", "unsplit", "maximize", "restore", "show"];

/// One device's layout of one thread: the synthetic session drafts the hub's layout code reads (the layout, the page, the picked board), the
/// focus recency (most recent first) and the revision clients compare.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub drafts: serde_json::Map<String, Value>,
    pub recent: Vec<String>,
    pub rev: u64,
    stamp: u64,
}

pub struct Store {
    entries: Mutex<HashMap<(String, String), Entry>>,
    /// connection → the device it belongs to
    clients: Mutex<HashMap<u64, String>>,
    /// (device, thread) → the connections that read this layout and are told when it changes
    watchers: Mutex<HashMap<(String, String), BTreeSet<u64>>>,
    file: Option<PathBuf>,
    tick: std::sync::atomic::AtomicU64,
    pruned: std::sync::atomic::AtomicU64,
}

static GLOBAL: OnceLock<Store> = OnceLock::new();

/// A device's layouts are gone (revoked): from the hub's store and its file.
pub fn forget_device_file(home: &Path, device: &str) {
    let st = global(home);
    st.forget_device(device);
    st.forget_device(&format!("browser:{device}"));
}

/// The hub's store, loaded from its home on first use.
pub fn global(home: &Path) -> &'static Store { GLOBAL.get_or_init(|| Store::open(Some(home.join("viewer-layouts.json")))) }

fn s<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(Value::as_str).unwrap_or("") }

impl Store {
    pub fn new() -> Store { Store::open(None) }
    pub fn open(file: Option<PathBuf>) -> Store {
        let st = Store { entries: Mutex::new(HashMap::new()), clients: Mutex::new(HashMap::new()), watchers: Mutex::new(HashMap::new()), file, tick: 0.into(), pruned: 0.into() };
        if let Some(f) = &st.file { st.load(f) }
        st
    }
    fn load(&self, file: &Path) {
        let Ok(text) = std::fs::read_to_string(file) else { return };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
        let mut g = self.entries.lock().unwrap();
        for (device, threads) in v["devices"].as_object().into_iter().flatten() {
            for (thread, e) in threads.as_object().into_iter().flatten() {
                let drafts = e["drafts"].as_object().cloned().unwrap_or_default();
                // only what this module writes is read back: `@view*:<thread>` drafts holding a multi-tile layout
                let drafts: serde_json::Map<String, Value> = drafts.into_iter().filter(|(k, _)| persisted_key(k, thread)).collect();
                if !has_layout(&drafts, thread) { continue }
                let recent = e["recent"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
                g.insert((device.clone(), thread.clone()), Entry { drafts, recent, rev: e["rev"].as_u64().unwrap_or(1).max(1), stamp: self.tick.fetch_add(1, std::sync::atomic::Ordering::Relaxed) });
            }
        }
    }
    /// Write the multi-tile layouts (atomic: a temporary file and a rename). Single-tile layouts are the page the device is on; they are not kept.
    fn save(&self) {
        let Some(file) = &self.file else { return };
        let mut devices = serde_json::Map::new();
        for ((device, thread), e) in self.entries.lock().unwrap().iter() {
            if !has_layout(&e.drafts, thread) { continue }
            let slot = devices.entry(device.clone()).or_insert_with(|| json!({}));
            // only what is root-relative stays on disk: the layout and the picked board (`@view.file` & co. hold the absolute path the desktop derives)
            let kept: serde_json::Map<String, Value> = e.drafts.iter().filter(|(k, _)| persisted_key(k, thread)).map(|(k, v)| (k.clone(), v.clone())).collect();
            slot[thread] = json!({"drafts": kept, "recent": e.recent, "rev": e.rev});
        }
        let body = json!({"version": 1, "devices": devices}).to_string();
        let tmp = file.with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, body).is_ok() && crate::plat::fs::rename(&tmp, file).is_err() { let _ = std::fs::remove_file(&tmp); }
    }

    pub fn entry(&self, device: &str, thread: &str) -> Option<Entry> { self.entries.lock().unwrap().get(&(device.to_string(), thread.to_string())).cloned() }
    pub fn len(&self) -> usize { self.entries.lock().unwrap().len() }
    /// Store `e` for (device, thread), keeping the store bounded; a layout that is a single tile is the page, so nothing of it stays on disk.
    pub fn put(&self, device: &str, thread: &str, mut e: Entry) {
        e.stamp = self.tick.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        { let mut g = self.entries.lock().unwrap();
          g.insert((device.to_string(), thread.to_string()), e);
          // least recently used out: per device, then across devices
          let mine: Vec<_> = g.iter().filter(|((d, _), _)| d == device).map(|(k, v)| (k.clone(), v.stamp)).collect();
          if mine.len() > THREADS_PER_DEVICE { if let Some((k, _)) = mine.into_iter().min_by_key(|(_, st)| *st) { g.remove(&k); } }
          let devices: BTreeSet<String> = g.keys().map(|(d, _)| d.clone()).collect();
          if devices.len() > DEVICES {
              if let Some(old) = devices.into_iter().min_by_key(|d| g.iter().filter(|((x, _), _)| x == d).map(|(_, v)| v.stamp).max().unwrap_or(0)) { g.retain(|(d, _), _| *d != old); }
          } }
        self.save();
    }
    /// A device's layouts are gone: revoked, logged out, expired.
    pub fn forget_device(&self, device: &str) {
        self.entries.lock().unwrap().retain(|(d, _), _| d != device);
        self.watchers.lock().unwrap().retain(|(d, _), _| d != device);
        self.save();
    }
    /// Drop every device not in `live` (the hub token's `hub` always stays).
    pub fn prune(&self, live: &BTreeSet<String>) -> usize {
        let gone: BTreeSet<String> = self.entries.lock().unwrap().keys().map(|(d, _)| d.clone()).filter(|d| d != "hub" && !live.contains(d)).collect();
        for d in &gone { self.forget_device(d) }
        gone.len()
    }

    pub fn register_client(&self, client: u64, device: &str) { self.clients.lock().unwrap().insert(client, device.to_string()); }
    /// Forget the layouts of every device the registry no longer holds (expired, revoked while the hub was down), at most once a minute.
    pub fn prune_to(&self, ids: &[String], now_ms: u64) {
        let last = self.pruned.swap(now_ms, std::sync::atomic::Ordering::Relaxed);
        if now_ms.saturating_sub(last) < 60_000 && last != 0 { self.pruned.store(last, std::sync::atomic::Ordering::Relaxed); return }
        let live: BTreeSet<String> = ids.iter().flat_map(|i| [i.clone(), format!("browser:{i}")]).collect();
        self.prune(&live);
    }
    pub fn device_of(&self, client: u64) -> Option<String> { self.clients.lock().unwrap().get(&client).cloned() }
    pub fn forget_client(&self, client: u64) {
        self.clients.lock().unwrap().remove(&client);
        for w in self.watchers.lock().unwrap().values_mut() { w.remove(&client); }
    }
    fn watch(&self, device: &str, thread: &str, client: u64) { self.watchers.lock().unwrap().entry((device.to_string(), thread.to_string())).or_default().insert(client); }
    /// The connections of `device` watching `thread`.
    pub fn watchers_of(&self, device: &str, thread: &str) -> Vec<u64> { self.watchers.lock().unwrap().get(&(device.to_string(), thread.to_string())).map(|w| w.iter().copied().collect()).unwrap_or_default() }
}

/// Whether a draft key is one this module owns for `thread`: the page, the stored layout and what they derive from.
fn owned_key(key: &str, thread: &str) -> bool {
    ["@view", "@view.layout", "@view.board", "@view.file", "@view.sheet", "@view.gerbers", "@mech.part", "@files.dir"].iter().any(|k| key == format!("{k}:{thread}"))
}
/// The drafts kept on disk: the stored layout and the picked board, both root-relative.
fn persisted_key(key: &str, thread: &str) -> bool { key == format!("@view.layout:{thread}") || key == format!("@view.board:{thread}") }
fn has_layout(drafts: &serde_json::Map<String, Value>, thread: &str) -> bool {
    drafts.get(&format!("@view.layout:{thread}")).and_then(Value::as_str).is_some_and(|t| serde_json::from_str::<Layout>(t).map_or(false, |l| l.tiles().len() >= 2))
}

/// The synthetic session the hub's layout code reads for this device: its own drafts and nothing else (no `info`, no other thread's drafts).
pub fn session_for(thread: &str, entry: Option<&Entry>, page: &str, diff: bool) -> Value {
    let mut drafts = serde_json::Map::new();
    match entry { Some(e) => for (k, v) in &e.drafts { if owned_key(k, thread) { drafts.insert(k.clone(), v.clone()); } },
        None => {
            // no layout yet: the page the device is on is the one tile (`gerbers` is a Board page with the Gerber drafts set)
            let (open, gerbers) = if page == "gerbers" { ("board", "1") } else { (page, "") };
            if !open.is_empty() { drafts.insert(format!("@view:{thread}"), json!(open)); }
            if !gerbers.is_empty() { drafts.insert(format!("@view.gerbers:{thread}"), json!(gerbers)); }
        } }
    let _ = diff;
    json!({"sel": thread, "rid": 1, "drafts": drafts, "panel": false, "info": {}})
}

/// `bounds` as a client sends it: `[w, h]` of numbers (strings, a missing one or a wrong count are an invalid request); each side is then clamped.
pub fn parse_bounds(v: &Value) -> Result<(f32, f32), Refused> {
    let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| Refused::of(Refusal::InvalidInput))?;
    let n = |x: &Value| x.as_f64();
    match (n(&a[0]), n(&a[1])) { (Some(w), Some(h)) => Ok(layout_fit::clamp_bounds(w, h)), _ => Err(Refused::of(Refusal::InvalidInput)) }
}

/// A refusal as data/1 answers it: `{code, key, message}`, the message from the catalog in the hub's language.
#[derive(Clone, Debug, PartialEq)]
pub struct Refused { pub code: &'static str, pub key: &'static str, pub text: &'static str }
impl Refused {
    pub fn of(r: Refusal) -> Refused { Refused::of_for(r, "en") }
    pub fn of_for(r: Refusal, locale: &str) -> Refused { Refused { code: r.code(), key: r.i18n_key(), text: r.text_for(locale) } }
    pub fn to_value(&self) -> Value { json!({"code": self.code, "key": self.key, "message": self.text}) }
}

const LINKED: &str = "That layout is shown on its own machine only for now.";
/// Refusals data/1 adds to the hub's own (catalog keys follow PR 1461's; English until they are in the catalogs).
fn extra(code: &'static str, key: &'static str, text: &'static str) -> Refused { Refused { code, key, text } }

/// Check a `source` a client names: it must be a path inside the project's folder (never absolute, never leaving it, no symlink out) that exists.
pub fn safe_source(root: &str, source: &str) -> Result<String, Refused> {
    if source.is_empty() { return Ok(String::new()) }
    let bad = || Refused::of(Refusal::InvalidSource);
    if !layout::is_relative_source(source) || source.contains('\0') || source.contains('\\') || source.split('/').any(|x| x == ".." ) || crate::plat::fs::is_abs(source) { return Err(bad()) }
    let rel = source.trim_start_matches("./").to_string();
    if root.is_empty() { return Err(bad()) }
    let (Ok(base), Ok(full)) = (std::fs::canonicalize(root), std::fs::canonicalize(Path::new(root).join(&rel))) else { return Err(bad()) };
    if !full.starts_with(&base) { return Err(bad()) }
    Ok(rel)
}

/// The tile JSON of one tile: no field of it is an absolute path.
fn tile_json(t: &layout::Tile, focused: bool, rect: Option<layout::Rect>) -> Value {
    let rel = |p: &str| if layout::is_relative_source(p) { p.to_string() } else { String::new() };
    let mut v = json!({"id": t.id, "kind": t.kind.as_str(), "source": rel(&t.source), "explicit": t.explicit, "focused": focused, "visible": rect.is_some(),
        "rect": rect.map(|r| json!({"x": r.x, "y": r.y, "w": r.w, "h": r.h}))});
    if let Some(ver) = &t.version { v["version"] = json!({"sourceId": ver.source_id, "id": ver.id, "label": ver.label}) }
    if let Some(ov) = &t.overlay {
        v["overlay"] = json!({"kind": ov.kind.as_str(), "source": rel(&ov.source), "mode": match ov.mode { layout::OverlayMode::Old => "old", layout::OverlayMode::Diff => "diff", layout::OverlayMode::New => "new" }});
        if let Some(ver) = &ov.version { v["overlay"]["version"] = json!({"sourceId": ver.source_id, "id": ver.id, "label": ver.label}) }
    }
    v
}

/// The `viewer.layout` reply: the stored tree fitted to `bounds`.
pub fn reply(thread: &str, rev: u64, layout: &Layout, fitted: &Fitted) -> Value {
    let rect_of = |id: &str| fitted.visible.iter().find(|(i, _)| i == id).map(|(_, r)| *r);
    let tiles: Vec<Value> = layout.tiles().into_iter().map(|t| tile_json(t, layout.focus.as_deref() == Some(t.id.as_str()), rect_of(&t.id))).collect();
    let axis = fitted.axis.map(|a| match a { layout::Axis::Row => "row", layout::Axis::Col => "col" });
    let splits: Vec<Value> = fitted.splits.iter().map(|sp| json!({"id": sp.id, "axis": match sp.axis { layout::Axis::Row => "row", layout::Axis::Col => "col" }, "ratio": sp.ratio,
        "rect": {"x": sp.rect.x, "y": sp.rect.y, "w": sp.rect.w, "h": sp.rect.h}})).collect();
    json!({"thread": thread, "rev": rev, "mode": fitted.mode.as_str(), "axis": axis, "bounds": [fitted.bounds.0, fitted.bounds.1],
        "focus": layout.focus, "maximized": layout.maximized, "order": fitted.order, "tiles": tiles, "splits": splits,
        "limits": {"maxTiles": MAX_TILES, "minTilePx": layout::MIN_TILE_PX}})
}

/// The layout the hub derives for a session (the desktop's own derivation: the stored tree, else the one open page).
fn current(st: &crate::state::State, session: &Value) -> Layout {
    viewer::viewer_json(st, &json!({}), session).and_then(|v| serde_json::from_value::<Layout>(v["layout"].clone()).ok()).unwrap_or_else(Layout::empty)
}

/// What `viewer.layout` answers: the device's tree fitted to `bounds`. The page the device is on (`page`, `diff`) is what a device with no layout of several
/// tiles is looking at: it is that one tile, kept so a following op starts from what was read; a change of page moves the tree (a new `rev`).
pub fn read(st: &crate::state::State, store: &Store, device: &str, thread: &str, bounds: (f32, f32), page: &str, diff: bool) -> Result<Value, Refused> {
    check_thread(st, thread)?;
    let entry = store.entry(device, thread);
    let multi = entry.as_ref().is_some_and(|e| has_layout(&e.drafts, thread));
    if !multi && !page.is_empty() {
        // a single page: the tree is derived from it, and kept (in memory) so the next op starts here
        let session = session_for(thread, None, page, diff);
        let l = current(st, &session);
        let mut e = entry.clone().unwrap_or(Entry { rev: 1, ..Default::default() });
        let before = if entry.is_some() { current(st, &session_for(thread, entry.as_ref(), "", false)) } else { Layout::empty() };
        let moved = entry.is_some() && serde_json::to_value(&before).ok() != serde_json::to_value(&l).ok();
        e.drafts = session["drafts"].as_object().cloned().unwrap_or_default();
        if moved { e.rev += 1; }
        e.recent.retain(|x| l.contains_tile(x));
        if let Some(f) = l.focus.clone() { e.recent.retain(|x| *x != f); e.recent.insert(0, f); }
        let rev = e.rev;
        let fitted = layout_fit::fit(&l, &e.recent, bounds);
        let out = reply(thread, rev, &l, &fitted);
        if entry.as_ref() != Some(&e) { store.put(device, thread, e); }
        return Ok(out);
    }
    let session = session_for(thread, entry.as_ref(), page, diff);
    let l = current(st, &session);
    let (rev, recent) = entry.map_or((1, vec![]), |e| (e.rev, e.recent));
    Ok(reply(thread, rev, &l, &layout_fit::fit(&l, &recent, bounds)))
}

fn check_thread(st: &crate::state::State, thread: &str) -> Result<(), Refused> {
    if thread.contains('~') { return Err(extra("linked", "viewer.layout.refusal.linked", LINKED)) }
    if !st.threads.contains_key(thread) { return Err(Refused::of(Refusal::NotFound)) }
    Ok(())
}

/// Map an op and its fields to the hub's layout action and its JSON payload. `bounds` is passed to `resize` (the hub clamps the ratio by tile minimums there).
fn action_of(op: &str, p: &Value, root: &str, bounds: (f32, f32)) -> Result<(&'static str, Value), Refused> {
    let bad = || Refused::of(Refusal::InvalidInput);
    let str_of = |k: &str| -> Result<Option<String>, Refused> { match p.get(k) { None | Some(Value::Null) => Ok(None), Some(v) => v.as_str().map(|x| Some(x.to_string())).ok_or_else(bad) } };
    let kind_ok = |k: &str| TILE_KINDS.contains(&k);
    Ok(match op {
        "add" => {
            let kind = str_of("kind")?.ok_or_else(bad)?;
            if !kind_ok(&kind) { return Err(extra("notOnDevice", "viewer.layout.refusal.notondevice", "That cannot be a tile here.")) }
            let mut v = json!({"kind": kind});
            if let Some(side) = str_of("side")? { v["side"] = json!(side) }
            if let Some(t) = str_of("target")? { v["target"] = json!(t) }
            if let Some(src) = str_of("source")? { v["source"] = json!(safe_source(root, &src)?) }
            if let Some(ver) = p.get("version").filter(|x| !x.is_null()) { v["version"] = ver.clone() }
            ("layout-add", v)
        }
        "close" => ("layout-close", json!({"tile": str_of("tile")?.ok_or_else(bad)?})),
        "focus" => ("layout-focus", json!({"tile": str_of("tile")?.ok_or_else(bad)?})),
        "maximize" => ("layout-maximize", json!({"tile": str_of("tile")?.ok_or_else(bad)?})),
        "restore" => ("layout-restore", json!({})),
        // split an overlay back out of `tile` (to `side`, right by default)
        "unsplit" => { let mut v = json!({"tile": str_of("tile")?.ok_or_else(bad)?}); if let Some(side) = str_of("side")? { v["side"] = json!(side) } ("layout-unsplit", v) }
        "resize" => {
            let ratio = p.get("ratio").and_then(Value::as_f64).filter(|r| r.is_finite()).ok_or_else(|| Refused::of(Refusal::InvalidRatio))?;
            ("layout-resize", json!({"split": str_of("split")?.ok_or_else(bad)?, "ratio": ratio, "bounds": [bounds.0, bounds.1]}))
        }
        "swap" => ("layout-swap", json!({"a": str_of("a")?.ok_or_else(bad)?, "b": str_of("b")?.ok_or_else(bad)?})),
        "move" => {
            let mut v = json!({"tile": str_of("tile")?.ok_or_else(bad)?, "target": str_of("target")?.ok_or_else(bad)?});
            if let Some(side) = str_of("side")? { v["side"] = json!(side) }
            ("layout-move", v)
        }
        // `overlay` is null (clear it), the id of another tile (merge it over `tile`) or `{kind, source?, mode?, version?}` (a source over it)
        "overlay" => {
            let mut v = json!({"base": str_of("tile")?.ok_or_else(bad)?});
            if let Some(mode) = str_of("mode")? { v["mode"] = json!(mode) }
            match p.get("overlay") {
                None | Some(Value::Null) => { v["overlay"] = Value::Null }
                Some(Value::String(tile)) => { v["overlay"] = json!(tile) }
                Some(ov) if ov.is_object() => {
                    let mut o = ov.clone();
                    let kind = ov["kind"].as_str().ok_or_else(bad)?;
                    if !kind_ok(kind) { return Err(extra("notOnDevice", "viewer.layout.refusal.notondevice", "That cannot be a tile here.")) }
                    if let Some(src) = ov.get("source").and_then(Value::as_str) { o["source"] = json!(safe_source(root, src)?) }
                    v["overlay"] = o;
                }
                _ => return Err(bad()),
            }
            ("layout-overlay", v)
        }
        _ => return Err(bad()),
    })
}

/// Apply `op` for `device` on `thread`: the hub's pure layout code over this device's own synthetic session, the result stored, the watchers told.
/// `root` is the project's design folder (sources are checked against it); the hub's locale decides the refusal text.
pub fn act(st: &crate::state::State, store: &Store, device: &str, thread: &str, op: &str, p: &Value, root: &str, locale: &str, bounds: (f32, f32)) -> Result<Value, Refused> {
    check_thread(st, thread)?;
    if !OPS.contains(&op) { return Err(Refused::of_for(Refusal::InvalidInput, locale)) }
    let entry = store.entry(device, thread);
    let page = s(p, "page");
    let session = session_for(thread, entry.as_ref(), page, p["diff"] == true);
    let before = current(st, &session);
    // a layout never holds more tiles than the limit, whatever an op adds
    let out = if op == "show" {
        let kind = s(p, "kind");
        if kind != "split" && !TILE_KINDS.contains(&kind) { return Err(extra("notOnDevice", "viewer.layout.refusal.notondevice", "That cannot be a tile here.")) }
        if let Some(src) = p.get("source").and_then(Value::as_str) { safe_source(root, src)?; }
        viewer::view_action_for(st, "", &session, None, locale, kind, 0)
    } else {
        let (action, mut payload) = action_of(op, p, root, bounds).map_err(|r| Refused { text: Refusal::InvalidInput.text_for(locale), ..r })?;
        // the guard fields are the desktop's own: a data/1 client holds no KiCad, so nothing it sends can name a lock or a KiCad tile
        if let Some(o) = payload.as_object_mut() { o.remove("locked"); o.remove("kicadTile"); }
        viewer::layout_action(st, "", &session, None, locale, action, &payload.to_string())
    };
    let vo = match out {
        Some(LayoutOut::Ok(vo)) => vo,
        Some(LayoutOut::Refused { code, key, text }) => return Err(Refused { code, key, text }),
        None => return Err(Refused::of_for(Refusal::InvalidInput, locale)),
    };
    let mut after_session = session.clone();
    crate::session::patch::merge(&mut after_session, &vo.patch);
    let after = current(st, &after_session);
    if after.tiles().len() > MAX_TILES { return Err(extra("tooManyTiles", "viewer.layout.refusal.toomany", "Too many tiles open")) }
    // only this device's own `@view*:<thread>` drafts come back; whatever else a patch carries is not stored
    let mut drafts = serde_json::Map::new();
    for (k, v) in after_session["drafts"].as_object().into_iter().flatten() { if owned_key(k, thread) && v.as_str().map_or(true, |t| !t.is_empty()) { drafts.insert(k.clone(), v.clone()); } }
    let changed = serde_json::to_value(&before).ok() != serde_json::to_value(&after).ok();
    let mut e = entry.unwrap_or(Entry { rev: 1, ..Default::default() });
    e.drafts = drafts;
    if changed { e.rev += 1; }
    if let Some(f) = after.focus.clone() { e.recent.retain(|x| *x != f); e.recent.insert(0, f); }
    e.recent.retain(|x| after.contains_tile(x));
    e.recent.truncate(MAX_TILES);
    let rev = e.rev;
    let fitted = layout_fit::fit(&after, &e.recent, bounds);
    store.put(device, thread, e);
    let mut r = reply(thread, rev, &after, &fitted);
    r["changed"] = json!(changed);
    Ok(r)
}

/// The connections of `device` to tell that `thread`'s layout moved to `rev`.
pub fn pushes(store: &Store, device: &str, thread: &str, rev: u64) -> Vec<(u64, Value)> {
    store.watchers_of(device, thread).into_iter().map(|c| (c, json!({"t": "layout", "thread": thread, "rev": rev}))).collect()
}

/// Who a connection is for the store: a paired device's id, a browser device (`browser:<id>`), or the hub token (`hub`).
pub fn device_key(native_device: Option<String>, browser_device: Option<&str>) -> String {
    match (browser_device, native_device) { (Some(b), _) => format!("browser:{b}"), (None, Some(d)) => d, (None, None) => "hub".to_string() }
}

/// The `viewer.layout` and `viewer.layout.act` requests, for the connection `c` of the hub `hub`.
pub async fn call(hub: &Arc<Hub>, c: &mut crate::hub::Conn, m: &str, p: &Value) -> Result<Value, Value> {
    let store = global(&hub.cfg.home);
    let device = store.device_of(c.client).unwrap_or_else(|| "hub".to_string());
    let locale = hub.setting("ui.lang").await.unwrap_or_default();
    let th = s(p, "thread").to_string();
    let bounds = parse_bounds(p.get("bounds").unwrap_or(&Value::Null)).map_err(|r| r.to_value())?;
    // the project's design folder, as `viewer.files` answers it, for the sources a client names
    let root = if th.contains('~') { String::new() } else { crate::files::kicad::canonical::design_root(&super::ext::root_of(hub, &th).await.unwrap_or_default()) };
    let r = crate::statehost::with_state_of(hub, |st| match m {
        "viewer.layout" => read(st, store, &device, &th, bounds, s(p, "page"), p["diff"] == true),
        _ => act(st, store, &device, &th, s(p, "op"), p, &root, &locale, bounds),
    });
    let r = match r { Some(r) => r, None => return Err(json!({"code": "starting", "key": "", "message": "The hub is starting."})) };
    match r {
        Ok(v) => {
            store.watch(&device, &th, c.client);
            if m == "viewer.layout.act" && v["changed"] == true {
                for (client, frame) in pushes(store, &device, &th, v["rev"].as_u64().unwrap_or(0)) { hub.push(client, frame); }
            }
            Ok(v)
        }
        Err(r) => Err(r.to_value()),
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
