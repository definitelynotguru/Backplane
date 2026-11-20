//! Arrange the hub's leaves. A drag owns only its preview ratio until pointer release.
use crate::{hub::Command, theme, ui::with_rect, Backplane};
use gpui::{prelude::*, *};
use serde_json::{json, Value};
use std::{cell::Cell, collections::HashMap, rc::Rc};

#[derive(Default)]
pub struct Dock {
    drag: Option<Resize>,
}

struct Resize {
    thread: String,
    split: String,
    axis: usize,
    start: f32,
    initial: f32,
    ratio: f32,
    extent: f32,
    range: (f32, f32),
    root: [f32; 2],
}

impl Resize {
    fn at(&self, point: Point<Pixels>) -> f32 {
        let pos = [f32::from(point.x), f32::from(point.y)][self.axis];
        (self.initial + (pos - self.start) / self.extent).clamp(self.range.0, self.range.1)
    }
}

fn ratio_range(axis: usize, size: [f32; 2], limits: &Value) -> Option<(f32, f32)> {
    let cross = 1 - axis;
    let dim = |child: usize, d: usize| limits[child][d].as_f64().map(|v| v as f32);
    if size[cross] < dim(0,cross)?.max(dim(1,cross)?) || size[axis] <= 0. { return None }
    let lo = (dim(0,axis)? + 4.) / size[axis];
    let hi = 1. - (dim(1,axis)? + 4.) / size[axis];
    (lo <= hi).then_some((lo,hi))
}

fn size_probe(target: Rc<Cell<[f32; 2]>>) -> AnyElement {
    canvas(move |b, _, _| target.set([b.size.width.into(), b.size.height.into()]), |_,_,_,_| {})
        .absolute().inset_0().into_any_element()
}

pub(super) fn tree(app: &Backplane, cx: &mut Context<Backplane>, viewer: &Value,
    mut leaves: HashMap<String, AnyElement>, light: bool) -> AnyElement {
    let root = Rc::new(Cell::new([0.;2]));
    let content = branch(app, cx, viewer, &viewer["layout"]["root"], &mut leaves, light, &root);
    let release = |app: &mut Backplane, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<Backplane>| {
        if let Some(drag) = app.screens.dock.drag.take() {
            if app.data["thread"]["id"] == drag.thread {
                let ratio = drag.at(e.position);
                if ratio != drag.initial {
                    let _ = app.tx.try_send(Command::Action("layout-resize".into(),
                        json!({"split":drag.split,"ratio":ratio,"bounds":drag.root}).to_string()));
                }
            }
            app.ui_epoch += 1; cx.notify();
        }
    };
    let surface = div().id("dock-root").relative().size_full().min_w_0().min_h_0().overflow_hidden()
        .child(size_probe(root)).child(content)
        .on_mouse_move(cx.listener(|app,e:&MouseMoveEvent,_,cx| {
            if let Some(drag) = app.screens.dock.drag.as_mut() {
                let ratio = drag.at(e.position);
                if drag.ratio != ratio { drag.ratio = ratio; app.ui_epoch += 1; cx.notify(); }
            }
        }))
        .on_mouse_up(MouseButton::Left,cx.listener(release))
        .on_mouse_up_out(MouseButton::Left,cx.listener(release));
    surface.into_any_element()
}

fn branch(app: &Backplane, cx: &mut Context<Backplane>, viewer: &Value, node: &Value,
    leaves: &mut HashMap<String, AnyElement>, light: bool, root: &Rc<Cell<[f32;2]>>) -> AnyElement {
    let id = node["id"].as_str().unwrap_or_default();
    if node["type"] != "split" { return leaves.remove(id).unwrap_or_else(|| div().into_any_element()) }
    let axis = usize::from(node["axis"] == "col");
    let thread = app.data["thread"]["id"].as_str().unwrap_or_default().to_owned();
    let initial = node["ratio"].as_f64().unwrap_or(0.5) as f32;
    let ratio = app.screens.dock.drag.as_ref().filter(|d| d.thread == thread && d.split == id)
        .map_or(initial, |d| d.ratio);
    let a = branch(app,cx,viewer,&node["a"],leaves,light,root);
    let b = branch(app,cx,viewer,&node["b"],leaves,light,root);
    let size = Rc::new(Cell::new([0.;2]));
    let base = div().size_full().relative().min_w_0().min_h_0().child(size_probe(size.clone()));
    let region = || div().absolute().min_w_0().min_h_0().overflow_hidden();
    let (first, second) = if axis == 0 {
        (region().left_0().top_0().bottom_0().w(relative(ratio)).pr(px(4.)),
         region().right_0().top_0().bottom_0().w(relative(1. - ratio)).pl(px(4.)))
    } else {
        (region().top_0().left_0().right_0().h(relative(ratio)).pb(px(4.)),
         region().bottom_0().left_0().right_0().h(relative(1. - ratio)).pt(px(4.)))
    };
    let split = id.to_owned();
    let limits = viewer["splitLimits"][id].clone();
    let root = root.clone();
    let dragging = app.screens.dock.drag.as_ref().is_some_and(|d| d.split == id && d.thread == thread);
    let bar = div().id(SharedString::from(format!("dock-edge-{id}"))).absolute().flex().items_center().justify_center();
    let bar = if axis == 0 { bar.left(relative(ratio)).ml(px(-6.)).w(px(12.)).h_full().cursor_col_resize() }
        else { bar.top(relative(ratio)).mt(px(-6.)).h(px(12.)).w_full().cursor_row_resize() };
    let group = SharedString::from(format!("dock-grip-{id}"));
    let accent = theme::color(light,"accent");
    let line = if axis == 0 { div().absolute().w(px(8.)).h_full() } else { div().absolute().h(px(8.)).w_full() };
    let grip = if axis == 0 { div().w(px(3.)).h(px(36.)) } else { div().h(px(3.)).w(px(36.)) };
    let bar = bar.group(group.clone()).child(line.bg(theme::color(light,"panel")))
        .child(grip.bg(if dragging { accent } else { theme::color(light,"dim") }).group_hover(group, move |s| s.bg(accent)))
        .on_mouse_down(MouseButton::Left,cx.listener(move |app,e:&MouseDownEvent,_,cx| {
            let dims = size.get();
            if let Some(range) = ratio_range(axis,dims,&limits) {
                app.screens.dock.drag = Some(Resize { thread:thread.clone(), split:split.clone(), axis,
                    start:[e.position.x.into(),e.position.y.into()][axis], initial, ratio:initial,
                    extent:dims[axis],range,root:root.get() });
                app.ui_epoch += 1; cx.notify();
                cx.stop_propagation();
            }
        }));
    let bar = if crate::art::rects_on() {
        bar.child(crate::art::probe_rect(format!("splitter:{id}")))
    } else { bar };
    base.child(first.child(a)).child(second.child(b)).child(bar).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::ratio_range;
    use serde_json::json;

    #[test]
    fn a_nested_child_and_the_cross_axis_bound_a_drag() {
        let limits = json!([[360,180],[180,300]]);
        assert_eq!(ratio_range(0,[720.,300.],&limits),Some((364./720.,1.-184./720.)));
        assert_eq!(ratio_range(0,[720.,299.],&limits),None);
        assert_eq!(ratio_range(1,[360.,480.],&limits),None);
    }
}

pub(crate) fn page(app: &mut Backplane, cx: &mut Context<Backplane>, light: bool, mut review_panel: Option<AnyElement>) -> AnyElement {
    use super::pane_header::{pane_header, Controls, Header, Identity};
    use super::panestate::{self, PaneState};
    use crate::i18n::trs;
    let thread = &app.data["thread"];
    let viewer = &thread["viewer"];
    let layout = &viewer["layout"];
    let mut leaves = HashMap::new();
    for (id, row) in viewer["tiles"].as_object().into_iter().flatten() {
        if layout["maximized"].as_str().is_some_and(|shown| shown != id) { continue; }
        let kind = row["kind"].as_str().unwrap_or_default();
        let focused = layout["focus"] == id.as_str();
        let header = Header { identity: Identity::new(kind, &row["identity"]).tile(focused),
            controls: Controls { tools: vec![], close: Empty.into_any_element() } };
        let header = super::tile_menu::decorate(cx,light,thread,row,false,&app.screens.tile_menu,header);
        let body = if let Some(reason) = row["error"].as_str() {
            panestate::view(light, &PaneState::error(trs(reason),false),None)
        } else if let Some(reason) = row["empty"].as_str() {
            panestate::view(light,&PaneState::empty(trs(reason),trs(row["emptyNext"].as_str().unwrap_or_default())),None)
        } else if matches!(kind, "board" | "schematic" | "gerbers") {
            let slot = app.screens.plots.tile_slot(id);
            let frame = app.screens.tile_frame(id).cloned();
            if let Some(slot) = slot {
                let mut picture = row.clone();
                if let Some(f) = &frame { picture["key"] = json!(f.key); }
                let plot = &app.screens.plot;
                let pv = &mut app.screens.plots[slot];
                let canvas = super::viewer::plot_pane_inner(cx,light,slot,if kind == "gerbers" { "board" } else { kind },
                    &picture,frame.as_ref(),pv,focused,&plot.sheets,&plot.sheet_refs,None,false,None,Some(&plot.retrying),pv.last_retry);
                if focused && matches!(kind, "board" | "schematic") {
                    // The history RPC and its controls belong to the focused document.
                    let key = picture["key"].as_str().unwrap_or_default();
                    let history = super::histbar::bar_el(cx, light, &app.screens.plots[0], key);
                    div().size_full().flex().flex_col()
                        .child(div().flex_1().min_h_0().child(canvas)).child(history).into_any_element()
                } else { canvas }
            } else { panestate::view(light,&PaneState::loading(panestate::loading_for(kind,kind == "gerbers")),None) }
        } else if let Some(page) = super::tile_pages::body_with_panel(&app.screens, thread, row, light, cx,
            if focused && kind == "review" { review_panel.take() } else { None }) {
            page
        } else if let Some(solid) = app.screens.tile_solid(id) {
            solid.clone().into_any_element()
        } else {
            panestate::view(light,&PaneState::loading(panestate::loading_for(kind,false)),None)
        };
        let key = id.clone();
        let body = with_rect(div().flex_1().min_h_0().min_w_0().overflow_hidden().child(body),||format!("tile-body:{id}"));
        let tile = with_rect(div().id(SharedString::from(format!("dock-{id}"))),||format!("tile:{id}"))
            .size_full().min_w_0().min_h_0().flex().flex_col()
            .border_1().border_color(theme::color(light,if focused {"accent"} else {"line"}))
            .on_mouse_down(MouseButton::Left,cx.listener(move |this,_,_,_| {
                if !focused { let _ = this.tx.try_send(Command::Action("layout-focus".into(),json!({"tile":key}).to_string())); }
            }))
            .child(app.screens.tile_menu.bounds_probe(thread["id"].as_str().unwrap_or_default(),id))
            .child(pane_header(light,header.identity,super::tile_pages::tools(cx,&app.screens,thread,row),header.controls)).child(body);
        leaves.insert(id.clone(),tile.into_any_element());
    }
    if let Some(id) = layout["maximized"].as_str() {
        if let Some(tile) = leaves.remove(id) { return tile; }
    }
    tree(app,cx,viewer,leaves,light)
}
