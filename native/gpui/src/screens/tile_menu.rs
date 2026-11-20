//! Layout controls shared by the single-page viewer and the tile renderer.
use super::pane_header::{Header, Press, button, picker};
use crate::{
    Backplane,
    hub::Command,
    i18n::{t, trs},
    theme,
    ui::with_rect,
};
use gpui::{prelude::*, *};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

type Owner = (String, String);

#[derive(Default)]
struct Measurements {
    thread: String,
    sizes: HashMap<String, [f32; 2]>,
}

#[derive(Clone)]
struct Navigation {
    focus: FocusHandle,
    take_focus: Rc<Cell<bool>>,
    row: usize,
    column: usize,
    keyboard: bool,
    scroll: ScrollHandle,
}

const SIDES: [(&str, &str, &str); 4] = [
    ("left", "◀", "viewer.tile.left"),
    ("right", "▶", "viewer.tile.right"),
    ("top", "▲", "viewer.tile.top"),
    ("bottom", "▼", "viewer.tile.bottom"),
];

#[derive(Clone, Default)]
pub struct State {
    open: Option<Owner>,
    nav: Option<Navigation>,
    measured: Rc<RefCell<Measurements>>,
    refusal: Option<(String, String, String)>,
    refusal_generation: u64,
}

impl State {
    pub fn record_bounds(&self, thread: &str, tile: &str, size: [f32; 2]) {
        let mut measured = self.measured.borrow_mut();
        if measured.thread != thread {
            measured.thread = thread.to_owned();
            measured.sizes.clear();
        }
        if size.iter().all(|n| n.is_finite() && *n > 0.) {
            measured.sizes.insert(tile.to_owned(), size);
        } else {
            measured.sizes.remove(tile);
        }
    }

    fn add_payload(&self, owner: &Owner, kind: &str, side: Option<&str>) -> Value {
        let mut payload = json!({"target":owner.1, "kind":kind});
        if let Some(side) = side {
            payload["side"] = json!(side);
        }
        {
            let measured = self.measured.borrow();
            if measured.thread == owner.0 {
                if let Some(size) = measured.sizes.get(&owner.1) {
                    payload["bounds"] = json!(size);
                }
            }
        }
        payload
    }

    pub fn bounds_probe(&self, thread: &str, tile: &str) -> AnyElement {
        let state = self.clone();
        let (thread, tile) = (thread.to_owned(), tile.to_owned());
        canvas(
            move |bounds, _, _| {
                state.record_bounds(
                    &thread,
                    &tile,
                    [bounds.size.width.into(), bounds.size.height.into()],
                )
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }
}

#[derive(Clone)]
pub enum Action {
    Open(Option<(String, String)>),
    Send(&'static str, Value),
    Fit(String),
    Add {
        owner: Owner,
        kind: String,
        side: Option<&'static str>,
    },
}

impl Action {
    pub(super) fn run(&self, this: &mut Backplane, cx: &mut Context<Backplane>) {
        match self {
            Self::Fit(id) => {
                if let Some(slot) = this.screens.plots.tile_slot(id) {
                    let plot = this.screens.pv(slot);
                    plot.touched = false;
                    plot.fit_wh = (0., 0.);
                }
            }
            Self::Open(target) => {
                let state = &mut this.screens.tile_menu;
                state.open = target.clone();
                state.nav = target.as_ref().map(|_| Navigation {
                    focus: cx.focus_handle(),
                    take_focus: Rc::new(Cell::new(true)),
                    row: 0,
                    column: 0,
                    keyboard: false,
                    scroll: ScrollHandle::new(),
                });
            }
            Self::Add { owner, kind, side } => {
                let state = &this.screens.tile_menu;
                if state.open.as_ref() != Some(owner) || this.data["thread"]["id"] != owner.0 {
                    return;
                }
                send_add(this, owner, kind, *side, cx);
                return;
            }
            Self::Send(action, payload) => {
                this.screens.tile_menu.open = None;
                this.screens.tile_menu.nav = None;
                let _ = this
                    .tx
                    .try_send(Command::Action((*action).into(), payload.to_string()));
            }
        }
        this.ui_epoch += 1;
        cx.notify();
    }
}

fn send_add(
    this: &mut Backplane,
    owner: &Owner,
    kind: &str,
    side: Option<&str>,
    cx: &mut Context<Backplane>,
) {
    let payload = this.screens.tile_menu.add_payload(owner, kind, side);
    Action::Send("layout-add", payload).run(this, cx);
}

pub(crate) fn rail_add(this: &mut Backplane, kind: &str, cx: &mut Context<Backplane>) {
    let thread = &this.data["thread"];
    let layout = &thread["viewer"]["layout"];
    let target = layout["focus"]
        .as_str()
        .filter(|id| !id.is_empty())
        .or_else(|| {
            layout["root"]["id"]
                .as_str()
                .filter(|_| layout["root"]["type"] == "tile")
        });
    if let (Some(thread), Some(tile)) = (thread["id"].as_str(), target) {
        let owner = (thread.to_owned(), tile.to_owned());
        send_add(this, &owner, kind, None, cx);
    }
}

pub(crate) fn refused(this: &mut Backplane, reply: &Value, cx: &mut Context<Backplane>) {
    let key = reply["key"].as_str().unwrap_or("");
    let mut text = t(key);
    if text == key {
        text = crate::string(reply, "text");
    }
    this.m("refusal", json!({"code":reply["code"], "text":text}));
    if let (Some(thread), Some(tile)) = (reply["thread"].as_str(), reply["tile"].as_str()) {
        show_refusal(this, thread, tile, text, cx);
        return;
    } else {
        this.note = text;
    }
    this.ui_epoch += 1;
    cx.notify();
}

pub(super) fn show_refusal(
    this: &mut Backplane,
    thread: &str,
    tile: &str,
    text: String,
    cx: &mut Context<Backplane>,
) {
    let state = &mut this.screens.tile_menu;
    state.refusal = Some((thread.to_owned(), tile.to_owned(), text));
    state.refusal_generation += 1;
    let generation = state.refusal_generation;
    cx.spawn(async move |this, cx| {
        Timer::after(std::time::Duration::from_secs(5)).await;
        let _ = this.update(cx, |app, cx| {
            let state = &mut app.screens.tile_menu;
            if state.refusal_generation == generation && state.refusal.take().is_some() {
                app.ui_epoch += 1;
                cx.notify();
            }
        });
    })
    .detach();
    this.ui_epoch += 1;
    cx.notify();
}

fn action(name: &'static str, payload: Value) -> Press {
    Press::Tile(Action::Send(name, payload))
}

// Register entries during prepaint, after the script registry starts the frame.
fn entry(
    cx: &Context<Backplane>,
    light: bool,
    page: &str,
    id: String,
    label: String,
    press: Press,
) -> Stateful<Div> {
    let entity = cx.entity().downgrade();
    with_rect(
        with_rect(div().id(SharedString::from(id.clone())), || id.clone()),
        || format!("pane-tool:{page}:{id}"),
    )
    .relative()
    .cursor_pointer()
    .px_1()
    .py_1()
    .hover(|s| s.bg(theme::color(light, "raise")))
    .when(crate::script::header_tracking(), |d| {
        let page = page.to_owned();
        let registered = press.clone();
        d.child(
            canvas(
                move |_, _, _| {
                    crate::script::header_action(&page, &id, &label, false, Some(registered));
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
    })
    .on_click(move |_, w, cx| {
        cx.stop_propagation();
        let _ = entity.update(cx, |this, cx| press.run(this, w, cx));
    })
}

fn add(owner: &Owner, kind: &str, column: usize) -> Press {
    Press::Tile(Action::Add {
        owner: owner.clone(),
        kind: kind.into(),
        side: column
            .checked_sub(1)
            .and_then(|i| SIDES.get(i))
            .map(|s| s.0),
    })
}

fn popup(
    cx: &Context<Backplane>,
    light: bool,
    viewer: &Value,
    owner: &Owner,
    nav: &Navigation,
) -> AnyElement {
    let c = |token| theme::color(light, token);
    let tile = owner.1.as_str();
    let mut options = Vec::new();
    let mut list = div()
        .id("add-beside-options")
        .flex()
        .flex_col()
        .max_h(crate::scale::tpx(340.))
        .overflow_y_scroll()
        .track_scroll(&nav.scroll);
    for (index, row) in viewer["rail"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| {
            r["id"]
                .as_str()
                .is_some_and(|id| !matches!(id, "split" | "terminal" | "commands"))
        })
        .enumerate()
    {
        let kind = row["id"].as_str().unwrap();
        let label = trs(row["label"].as_str().unwrap_or(kind));
        let group = SharedString::from(format!("add-row-{kind}"));
        let icon = row["icon"]
            .as_str()
            .and_then(crate::art::icon_name)
            .map(|icon| crate::art::icon(icon, 14., c("dim")));
        let caption = div()
            .flex()
            .items_center()
            .gap_2()
            .children(icon)
            .child(label.clone());
        let mut line = div()
            .group(group.clone())
            .flex()
            .items_center()
            .px_1()
            .h(crate::scale::tpx(27.));
        if row["state"] == "disabled" {
            let why = trs(row["why"].as_str().unwrap_or(""));
            line = line.child(
                div()
                    .id(SharedString::from(format!("disabled-{kind}")))
                    .size_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .opacity(0.55)
                    .tooltip(crate::keys::tip(&why, "", light))
                    .child(caption)
                    .child(
                        div()
                            .text_size(crate::scale::tpx(10.))
                            .text_color(c("dim"))
                            .child(why),
                    ),
            );
        } else {
            let focused = nav.keyboard && nav.row == options.len();
            options.push((index, kind.to_owned()));
            line = line
                .when(focused, |d| d.bg(c("raise")))
                .hover(|s| s.bg(c("raise")))
                .child(
                    entry(
                        cx,
                        light,
                        tile,
                        format!("add-beside:{kind}"),
                        label,
                        add(owner, kind, 0),
                    )
                    .flex_1()
                    .child(caption),
                );
            let mut arrows = div().flex().items_center().when(!focused, |d| {
                d.invisible().group_hover(group, |s| s.visible())
            });
            for (i, (side, glyph, key)) in SIDES.iter().enumerate() {
                let hint = t(key);
                arrows = arrows.child(
                    entry(
                        cx,
                        light,
                        tile,
                        format!("add-beside:{kind}:{side}"),
                        hint.clone(),
                        add(owner, kind, i + 1),
                    )
                    .tooltip(crate::keys::tip(&hint, "", light))
                    .when(focused && nav.column == i + 1, |d| {
                        d.bg(c("panel")).text_color(c("accent"))
                    })
                    .child(*glyph),
                );
            }
            line = line.child(arrows);
        }
        list = list.child(line);
    }
    let owner = owner.clone();
    let entity = cx.entity().downgrade();
    let outside = entity.clone();
    let focus = nav.focus.clone();
    let pending = nav.take_focus.clone();
    let menu = div()
        .id("add-beside-menu")
        .occlude()
        .min_w(crate::scale::tpx(210.))
        .p_1()
        .bg(c("panel"))
        .border_1()
        .border_color(c("line"))
        .text_size(crate::scale::tpx(12.))
        .track_focus(&nav.focus)
        .child(
            canvas(
                move |_, w, _| {
                    if pending.replace(false) {
                        w.focus(&focus);
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .text_color(c("dim"))
                .child(t("viewer.tile.add")),
        )
        .child(list)
        .on_key_down(move |ev, w, cx| {
            let key = ev.keystroke.key.as_str();
            if !matches!(
                key,
                "escape" | "up" | "down" | "left" | "right" | "enter" | "space"
            ) {
                return;
            }
            cx.stop_propagation();
            w.prevent_default();
            let _ = entity.update(cx, |app, cx| {
                if key == "escape" {
                    Action::Open(None).run(app, cx);
                    return;
                }
                if options.is_empty() {
                    return;
                }
                let Some(nav) = app.screens.tile_menu.nav.as_mut() else {
                    return;
                };
                nav.row = nav.row.min(options.len() - 1);
                let first = !nav.keyboard;
                nav.keyboard = true;
                match key {
                    "up" if first => nav.row = options.len() - 1,
                    "down" if first => nav.row = 0,
                    "up" => nav.row = (nav.row + options.len() - 1) % options.len(),
                    "down" => nav.row = (nav.row + 1) % options.len(),
                    "left" => nav.column = (nav.column + SIDES.len()) % (SIDES.len() + 1),
                    "right" => nav.column = (nav.column + 1) % (SIDES.len() + 1),
                    _ => {
                        let press = add(&owner, &options[nav.row].1, nav.column);
                        press.run(app, w, cx);
                        return;
                    }
                }
                nav.scroll.scroll_to_item(options[nav.row].0);
                app.ui_epoch += 1;
                cx.notify();
            });
        })
        .on_mouse_down_out(move |_, _, cx| {
            let _ = outside.update(cx, |app, cx| Action::Open(None).run(app, cx));
        });
    div()
        .absolute()
        .top(crate::scale::tpx(super::head::BTN_H))
        .right_0()
        .child(
            deferred(
                anchored()
                    .anchor(Corner::TopRight)
                    .snap_to_window_with_margin(px(4.))
                    .child(menu),
            )
            .with_priority(3),
        )
        .into_any_element()
}

fn trigger(cx: &Context<Backplane>, light: bool, owner: &Owner, open: bool) -> AnyElement {
    let hint = t("viewer.tile.add.tip");
    let activate = Press::Tile(Action::Open((!open).then(|| owner.clone())));
    let keyboard = activate.clone();
    let entity = cx.entity().downgrade();
    entry(
        cx,
        light,
        &owner.1,
        format!("tile:{}:add-beside", owner.1),
        hint.clone(),
        activate,
    )
    .focusable()
    .tab_stop(true)
    .focus(|s| s.border_color(theme::color(light, "accent")))
    .on_key_down(move |ev, w, cx| {
        if matches!(ev.keystroke.key.as_str(), "enter" | "space" | "down") {
            cx.stop_propagation();
            w.prevent_default();
            let _ = entity.update(cx, |app, cx| keyboard.run(app, w, cx));
        }
    })
    .size(crate::scale::tpx(super::head::BTN_H))
    .flex()
    .items_center()
    .justify_center()
    .border_1()
    .border_color(theme::color(light, if open { "accent" } else { "line" }))
    .tooltip(crate::keys::tip(&hint, "", light))
    .child(crate::art::icon(
        "layout-panel-left",
        14.,
        theme::color(light, "text"),
    ))
    .into_any_element()
}

/// Pass the original thread picture and the painted layout leaf, including for a maximized tile.
pub fn decorate(
    cx: &Context<Backplane>,
    light: bool,
    thread: &Value,
    leaf: &Value,
    embedded: bool,
    state: &State,
    mut h: Header,
) -> Header {
    let Some(id) = leaf["id"].as_str() else {
        return h;
    };
    let thread_id = crate::string(thread, "id");
    let owner = (thread_id.clone(), id.to_owned());
    let viewer = &thread["viewer"];
    if embedded || !viewer["rail"].is_array() {
        return h;
    }
    let open = state.open.as_ref() == Some(&owner);
    let tiled = super::viewer::is_tiled(viewer);
    let add_id = if tiled {
        format!("tile:{id}:add-beside")
    } else {
        format!("tile:{id}:more:add-beside")
    };
    let mut menu = state
        .nav
        .as_ref()
        .filter(|_| open)
        .map(|nav| popup(cx, light, viewer, &owner, nav));
    let add = picker(
        cx,
        &add_id,
        t("viewer.tile.add"),
        open,
        if tiled { menu.take() } else { None },
        Press::Tile(Action::Open((!open).then(|| owner.clone()))),
    );
    if tiled
        && matches!(
            leaf["kind"].as_str(),
            Some("board" | "gerbers" | "schematic")
        )
        && !leaf["error"].is_string()
        && !leaf["empty"].is_string()
    {
        h.controls.tools.push(button(
            cx,
            format!("tile:{id}:fit"),
            trs("Fit"),
            Press::Tile(Action::Fit(id.into())),
        ));
    }
    h.controls.tools.push(add);
    if let Some((thread, tile, text)) = &state.refusal {
        if *thread == thread_id && tile == id {
            h.identity.refusal(text);
        }
    }
    if tiled {
        let grip = entry(
            cx,
            light,
            id,
            format!("tile:{id}:focus"),
            "⠿".into(),
            action("layout-focus", json!({"tile":id})),
        )
        .child("⠿");
        h.identity.grip = Some(
            div()
                .id(SharedString::from(format!("tile-grip:{id}")))
                .tooltip(crate::keys::tip(&t("viewer.tile.focus"), "", light))
                .child(grip)
                .into_any_element(),
        );
        h.controls.close = div()
            .id(SharedString::from(format!("tile-close:{id}")))
            .tooltip(crate::keys::tip(&t("common.close"), "", light))
            .child(
                entry(
                    cx,
                    light,
                    id,
                    format!("tile:{id}:close"),
                    "×".into(),
                    action("layout-close", json!({"tile":id})),
                )
                .child("×"),
            )
            .into_any_element();
    } else {
        h.controls.close = div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .relative()
                    .child(trigger(cx, light, &owner, open))
                    .children(menu),
            )
            .child(h.controls.close)
            .into_any_element();
    }
    h
}

/// Use the same trigger alongside the controls on pages with a corner toolbar.
pub(super) fn corner(
    cx: &Context<Backplane>,
    light: bool,
    thread: &Value,
    state: &State,
    controls: AnyElement,
) -> AnyElement {
    let viewer = &thread["viewer"];
    let leaf = &viewer["layout"]["root"];
    let Some(id) = leaf["id"]
        .as_str()
        .filter(|_| leaf["type"] == "tile" && viewer["rail"].is_array())
    else {
        return controls;
    };
    let thread_id = crate::string(thread, "id");
    let owner = (thread_id.clone(), id.to_owned());
    let open = state.open.as_ref() == Some(&owner);
    let add = div()
        .relative()
        .child(trigger(cx, light, &owner, open))
        .children(
            state
                .nav
                .as_ref()
                .filter(|_| open)
                .map(|nav| popup(cx, light, viewer, &owner, nav)),
        );
    let message = state
        .refusal
        .as_ref()
        .filter(|(thread, tile, _)| thread == &thread_id && tile == id);
    div()
        .relative()
        .flex()
        .items_center()
        .children(
            message.map(|(_, _, text)| div().text_size(crate::scale::tpx(12.)).child(text.clone())),
        )
        .child(controls)
        .child(add)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{SIDES, State};
    use serde_json::json;

    #[test]
    fn default_and_directional_add_use_the_acted_tiles_current_size() {
        let state = State::default();
        let owner = ("thread".to_owned(), "t2".to_owned());
        state.record_bounds("thread", "t1", [600., 400.]);
        state.record_bounds("thread", "t2", [280., 520.]);
        assert_eq!(
            state.add_payload(&owner, "board", None),
            json!({"target":"t2", "kind":"board", "bounds":[280.,520.]})
        );
        state.record_bounds("thread", "t2", [390., 520.]);
        for (side, _, _) in SIDES {
            assert_eq!(
                state.add_payload(&owner, "board", Some(side)),
                json!({"target":"t2", "kind":"board", "bounds":[390.,520.], "side":side})
            );
        }
        state.record_bounds("another-thread", "t2", [400., 500.]);
        assert!(
            state
                .add_payload(&owner, "board", None)
                .get("bounds")
                .is_none()
        );
    }
}
