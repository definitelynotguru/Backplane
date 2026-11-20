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

#[derive(Clone, Default)]
pub struct State {
    open: Option<(String, String)>,
    refusal: Option<(String, String, String)>,
    refusal_generation: u64,
}

#[derive(Clone)]
pub enum Action {
    Open(Option<(String, String)>),
    Send(&'static str, Value),
    Fit(String),
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
            Self::Open(target) => this.screens.tile_menu.open = target.clone(),
            Self::Send(action, payload) => {
                this.screens.tile_menu.open = None;
                this.screens.tile_menu.refusal = None;
                let _ = this
                    .tx
                    .try_send(Command::Action((*action).into(), payload.to_string()));
            }
        }
        this.ui_epoch += 1;
        cx.notify();
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
        let state = &mut this.screens.tile_menu;
        state.refusal = Some((thread.into(), tile.into(), text));
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
        }).detach();
    } else {
        this.note = text;
    }
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
) -> AnyElement {
    let paint_id = id.clone();
    let drawn = label.clone();
    let page = page.to_owned();
    let registered = press.clone();
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
    .child(label)
    .child(
        canvas(
            move |_, _, _| {
                crate::script::header_action(&page, &paint_id, &drawn, false, Some(registered));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0(),
    )
    .on_click(move |_, w, cx| {
        cx.stop_propagation();
        let _ = entity.update(cx, |this, cx| press.run(this, w, cx));
    })
    .into_any_element()
}

fn popup(cx: &Context<Backplane>, light: bool, viewer: &Value, tile: &str) -> AnyElement {
    let c = |token| theme::color(light, token);
    let mut rows = div()
        .id("add-beside-menu")
        .occlude()
        .p_1()
        .bg(c("panel"))
        .border_1()
        .border_color(c("line"))
        .text_size(crate::scale::tpx(12.))
        .flex()
        .flex_col()
        .max_h(px(360.))
        .overflow_y_scroll();
    for row in viewer["rail"].as_array().into_iter().flatten() {
        let Some(kind) = row["id"].as_str() else {
            continue;
        };
        if matches!(kind, "split" | "terminal" | "commands") {
            continue;
        }
        let label = trs(row["label"].as_str().unwrap_or(kind));
        let mut line = div().flex().items_center().gap_1();
        if row["state"] == "disabled" {
            line = line
                .opacity(0.6)
                .px_1()
                .child(label)
                .child(trs(row["why"].as_str().unwrap_or("")));
        } else {
            let add = |side| action("layout-add", json!({"target":tile,"kind":kind,"side":side}));
            line = line.child(div().flex_1().child(entry(
                cx,
                light,
                tile,
                format!("add-beside:{kind}"),
                label,
                add("right"),
            )));
            for (side, key) in [
                ("left", "viewer.c.view.left"),
                ("right", "viewer.c.view.right"),
                ("top", "viewer.c.view.top"),
                ("bottom", "viewer.c.view.bottom"),
            ] {
                line = line.child(entry(
                    cx,
                    light,
                    tile,
                    format!("add-beside:{kind}:{side}"),
                    t(key),
                    add(side),
                ));
            }
        }
        rows = rows.child(line);
    }
    let entity = cx.entity().downgrade();
    let rows = rows.on_mouse_down_out(move |_, _, cx| {
        let _ = entity.update(cx, |this, cx| Action::Open(None).run(this, cx));
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
                    .child(rows),
            )
            .with_priority(3),
        )
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
    let add_id = format!("tile:{id}:add-beside");
    let add = picker(
        cx,
        &add_id,
        t("viewer.tile.add"),
        open,
        open.then(|| popup(cx, light, viewer, id)),
        Press::Tile(Action::Open((!open).then_some(owner))),
    );
    if viewer["layout"]["root"]["type"] == "split"
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
    if viewer["layout"]["root"]["type"] == "split" {
        let grip = entry(
            cx,
            light,
            id,
            format!("tile:{id}:focus"),
            "⠿".into(),
            action("layout-focus", json!({"tile":id})),
        );
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
            .child(entry(
                cx,
                light,
                id,
                format!("tile:{id}:close"),
                "×".into(),
                action("layout-close", json!({"tile":id})),
            ))
            .into_any_element();
    }
    h
}

/// Corner toolbars (such as Mech and Browser) keep their existing resting size.
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
    let group = "tile-corner-tools";
    let add = div()
        .id("corner-add")
        .absolute()
        .right(relative(1.))
        .top_0()
        .bg(theme::color(light, "panel"))
        .whitespace_nowrap()
        .tooltip(crate::keys::tip(&t("viewer.tile.add"), "", light))
        .when(!open, |d| {
            d.opacity(0.).group_hover(group, |s| s.opacity(1.))
        })
        .child(entry(
            cx,
            light,
            id,
            format!("tile:{id}:add-beside"),
            "⋯".into(),
            Press::Tile(Action::Open((!open).then_some(owner))),
        ))
        .children(open.then(|| popup(cx, light, viewer, id)));
    let message = state
        .refusal
        .as_ref()
        .filter(|(thread, tile, _)| thread == &thread_id && tile == id);
    div()
        .group(group)
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
