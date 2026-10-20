//! The node editor on the Compose page: the graph of the container being
//! edited, as boxes and curves (design §10).
//!
//! Positions are saved in the document (`Command::SetPosition`); a node
//! without one is placed in columns by depth until it's moved.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use egui::{
    Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};
use fw_core::NodeId;
use fw_core::id::{IdGen, RandomIds};
use fw_doc::{Command, Document, Edge, Graph, Node, PortRef};
use fw_engine::Engine;
use fw_plugin::{INPUT_TYPE, NodeTypeDesc, OUTPUT_TYPE, PortType, Registry};

use crate::app::Edit;
use crate::controls::{Gestures, Number};
use crate::icons;
use crate::library::{self, Library};
use crate::style::{medium, mono, sans, tokens as t};

/// Node sizes, in editor units (points at 100%).
const NODE_WIDTH: f32 = 156.0;
const NODE_HEAD: f32 = 46.0;
const PORT_SPACING: f32 = 18.0;
/// Where the editor places nodes that have no saved position.
const COLUMN: f32 = 220.0;
const ROW: f32 = 90.0;
/// How near the pointer must be to a port to drop a connection on it, in points.
const PORT_REACH: f32 = 12.0;
const MIN_ZOOM: f32 = 0.3;
const MAX_ZOOM: f32 = 2.0;

#[derive(Default)]
pub struct NodeEditor {
    /// Pan and zoom, per container.
    views: HashMap<NodeId, View>,
    drag: Option<Drag>,
    /// The add-node list, when open.
    library: Option<Library>,
    /// Selected nodes. The inspector shows the one clicked last.
    selection: BTreeSet<NodeId>,
    ids: RandomIds,
}

#[derive(Clone, Copy)]
struct View {
    /// Where editor (0, 0) is, from the canvas's top left, in points.
    offset: Vec2,
    zoom: f32,
    /// Framed to show every node, which happens when first shown.
    framed: bool,
}

impl Default for View {
    fn default() -> View {
        View {
            offset: Vec2::ZERO,
            zoom: 1.0,
            framed: false,
        }
    }
}

impl View {
    fn to_screen(self, canvas: Rect, p: Pos2) -> Pos2 {
        canvas.min + self.offset + p.to_vec2() * self.zoom
    }

    fn to_editor(self, canvas: Rect, p: Pos2) -> Pos2 {
        ((p - canvas.min - self.offset) / self.zoom).to_pos2()
    }
}

enum Drag {
    /// Moving the selected nodes.
    Nodes {
        /// Where each started, in editor units.
        start: BTreeMap<NodeId, Pos2>,
        /// Where the pointer started, in editor units.
        from: Pos2,
        /// The last move sent, so a still pointer sends nothing.
        sent: Vec2,
    },
    /// A connection being made, or picked up from an input.
    Wire {
        /// The end that stays put.
        fixed: PortRef,
        fixed_side: Side,
        /// The input it was picked up from.
        detached: Option<PortRef>,
    },
    Pan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Input,
    Output,
}

/// A port on screen.
struct Port {
    port: PortRef,
    side: Side,
    name: String,
    ty: PortType,
    /// Its center, in editor units.
    at: Pos2,
}

/// A node laid out, in editor units.
struct Placed {
    id: NodeId,
    rect: Rect,
    name: String,
    /// The second line: what the node reads or what it is.
    detail: String,
    color: Color32,
    icon: &'static str,
}

impl NodeEditor {
    /// The selected nodes, for deleting.
    pub fn selection(&self) -> &BTreeSet<NodeId> {
        &self.selection
    }

    /// Frames every node on the next frame.
    pub fn frame_all(&mut self, container: NodeId) {
        self.views.entry(container).or_default().framed = false;
    }
}

#[allow(clippy::too_many_arguments)]
pub fn node_editor(
    ui: &mut Ui,
    state: &mut NodeEditor,
    gestures: &mut Gestures,
    engine: &Engine,
    container: NodeId,
    selected: &mut Option<NodeId>,
    edits: &mut Vec<Edit>,
) {
    let doc = engine.document();
    let registry = engine.registry();
    let Some(graph) = doc.graph(container) else {
        return;
    };
    // The inspector's selection wins over ours when they disagree.
    state.selection.retain(|id| graph.nodes.contains(id));
    match *selected {
        Some(id) if !state.selection.contains(&id) => state.selection = BTreeSet::from([id]),
        None => state.selection.clear(),
        _ => {}
    }

    let canvas = ui.max_rect();
    let (placed, ports) = lay_out(doc, registry, container, graph);
    let mut view = *state.views.entry(container).or_default();
    if !view.framed && canvas.width() > 0.0 {
        frame(&mut view, canvas, &placed);
    }
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let hovered = pointer.is_some_and(|p| canvas.contains(p)) && ui.rect_contains_pointer(canvas);
    if hovered {
        pan_and_zoom(ui, &mut view, canvas, pointer.expect("hovered"));
    }

    // Behind the nodes: clicking deselects, dragging pans, right-clicking adds.
    let background = ui.interact(
        canvas,
        ui.scope_id().with("node-canvas"),
        Sense::click_and_drag(),
    );
    if background.clicked() {
        state.selection.clear();
        *selected = None;
        state.library = None;
    }
    if background.drag_started() {
        state.drag = Some(Drag::Pan);
    }
    if background.dragged() && matches!(state.drag, Some(Drag::Pan)) {
        view.offset += background.drag_delta();
    }
    if background.drag_stopped() && matches!(state.drag, Some(Drag::Pan)) {
        state.drag = None;
    }
    // Tab over the canvas opens the library even though egui has already
    // moved keyboard focus for it; the library's search field takes focus.
    let tab = hovered
        && state.library.is_none()
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
    let typing = ui.ctx().egui_wants_keyboard_input();
    if (background.secondary_clicked() || tab)
        && let Some(at) = pointer
    {
        let supplied = container != doc.root();
        state.library = Some(Library::new(at, view.to_editor(canvas, at), supplied));
    }
    if hovered && !typing && ui.input(|i| i.key_pressed(egui::Key::F)) {
        frame(&mut view, canvas, &placed);
    }

    let painter = ui.painter_at(canvas);
    grid(&painter, canvas, &view);

    // Connections, under the nodes. Those touching the selection are lit.
    let port_at = |port: &PortRef, side: Side| {
        ports
            .iter()
            .find(|p| p.side == side && p.port == *port)
            .map(|p| view.to_screen(canvas, p.at))
    };
    let detached = match &state.drag {
        Some(Drag::Wire { detached, .. }) => detached.clone(),
        _ => None,
    };
    for edge in graph.edges() {
        if Some(&edge.to) == detached.as_ref() {
            continue;
        }
        let (Some(a), Some(b)) = (
            port_at(&edge.from, Side::Output),
            port_at(&edge.to, Side::Input),
        ) else {
            continue;
        };
        let lit =
            state.selection.contains(&edge.from.node) || state.selection.contains(&edge.to.node);
        let ty = ports
            .iter()
            .find(|p| p.side == Side::Input && p.port == edge.to)
            .map_or(PortType::Image, |p| p.ty);
        let color = if lit { t::ACCENT } else { wire_color(ty) };
        wire(
            &painter,
            a,
            b,
            view.zoom,
            Stroke::new(if lit { 1.8 } else { 1.5 }, color),
        );
    }

    // Nodes, in id order; later ones are on top.
    let mut clicked = None;
    let mut drag_started = None;
    for node in &placed {
        let rect = Rect::from_min_max(
            view.to_screen(canvas, node.rect.min),
            view.to_screen(canvas, node.rect.max),
        );
        let id = ui.scope_id().with(("node", node.id));
        let response = ui
            .interact(rect.intersect(canvas), id, Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::Grab);
        let is_selected = state.selection.contains(&node.id);
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::Button,
                true,
                is_selected,
                format!("Node {}", node.name),
            )
        });
        if response.clicked() {
            clicked = Some(node.id);
        }
        if response.drag_started() {
            drag_started = Some(node.id);
        }
        if response.dragged()
            && let (Some(Drag::Nodes { start, from, sent }), Some(pos)) =
                (&mut state.drag, response.interact_pointer_pos())
        {
            let delta = (view.to_editor(canvas, pos) - *from).round();
            if delta != *sent {
                *sent = delta;
                let commands = start
                    .iter()
                    .map(|(&node, &p)| Command::SetPosition {
                        node,
                        position: Some([f64::from(p.x + delta.x), f64::from(p.y + delta.y)]),
                    })
                    .collect();
                edits.push(Edit {
                    label: if start.len() == 1 {
                        "Move Node"
                    } else {
                        "Move Nodes"
                    }
                    .into(),
                    commands,
                    gesture: Some(gestures.drag(id)),
                });
            }
        }
        if response.drag_stopped() {
            gestures.end(id);
            state.drag = None;
        }
        draw_node(&painter, node, rect, view.zoom, is_selected);
    }
    let shift = ui.input(|i| i.modifiers.shift);
    if let Some(node) = clicked.or(drag_started) {
        let keep = drag_started.is_some() && state.selection.contains(&node);
        if shift && clicked.is_some() {
            if !state.selection.remove(&node) {
                state.selection.insert(node);
            }
        } else if shift {
            state.selection.insert(node);
        } else if !keep {
            state.selection = BTreeSet::from([node]);
        }
        *selected = state
            .selection
            .contains(&node)
            .then_some(node)
            .or_else(|| state.selection.last().copied());
    }
    let press = ui.input(|i| i.pointer.press_origin()).or(pointer);
    if let (Some(_), Some(pos)) = (drag_started, press) {
        let start = placed
            .iter()
            .filter(|p| state.selection.contains(&p.id))
            .map(|p| (p.id, p.rect.min))
            .collect();
        state.drag = Some(Drag::Nodes {
            start,
            from: view.to_editor(canvas, pos),
            sent: Vec2::ZERO,
        });
    }

    // Ports, over the nodes so they win the pointer.
    for port in &ports {
        let center = view.to_screen(canvas, port.at);
        let hit = Rect::from_center_size(center, Vec2::splat(14.0));
        let id = ui
            .scope_id()
            .with(("port", &port.port, port.side == Side::Input));
        let response = ui
            .interact(hit.intersect(canvas), id, Sense::drag())
            .on_hover_cursor(egui::CursorIcon::Crosshair)
            .on_hover_text(&port.name);
        let owner = placed
            .iter()
            .find(|p| p.id == port.port.node)
            .map_or("", |p| p.name.as_str());
        let side = match port.side {
            Side::Input => "Input",
            Side::Output => "Output",
        };
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                true,
                format!("{side} {} of {owner}", port.name),
            )
        });
        if response.drag_started() {
            let source = (port.side == Side::Input)
                .then(|| graph.inputs.get(&port.port).cloned())
                .flatten();
            state.drag = Some(match source {
                // Picking up a connection by its input end.
                Some(from) => Drag::Wire {
                    fixed: from,
                    fixed_side: Side::Output,
                    detached: Some(port.port.clone()),
                },
                None => Drag::Wire {
                    fixed: port.port.clone(),
                    fixed_side: port.side,
                    detached: None,
                },
            });
        }
        let radius = 4.0 * view.zoom.max(0.6);
        painter.circle(
            center,
            radius,
            t::WELL,
            Stroke::new(1.5, port_color(port.ty)),
        );
    }

    // The connection being dragged, and whether it can land where it is.
    if let Some(Drag::Wire {
        fixed,
        fixed_side,
        detached,
    }) = &state.drag
        && let (Some(start), Some(pos)) = (port_at(fixed, *fixed_side), pointer)
    {
        let target = ports
            .iter()
            .filter(|p| p.side != *fixed_side)
            .map(|p| (p, view.to_screen(canvas, p.at)))
            .filter(|(_, at)| at.distance(pos) <= PORT_REACH)
            .min_by(|a, b| a.1.distance(pos).total_cmp(&b.1.distance(pos)));
        let end = target.map_or(pos, |(_, at)| at);
        let plan = wire_edit(
            fixed,
            *fixed_side,
            detached.as_ref(),
            target.map(|(p, _)| &p.port),
        );
        let refused = plan
            .as_ref()
            .and_then(|edit| dry_run(doc, registry, &edit.commands).err());
        let color = match (&refused, target) {
            (Some(_), _) => t::REFUSED,
            (None, Some(_)) => t::ACCENT,
            (None, None) => t::TEXT2,
        };
        let (a, b) = match fixed_side {
            Side::Output => (start, end),
            Side::Input => (end, start),
        };
        wire(&painter, a, b, view.zoom, Stroke::new(1.8, color));
        if let Some(reason) = &refused {
            let galley = painter.layout(reason.clone(), sans(11.0), t::TEXT_BRIGHT, 260.0);
            let at = pos + vec2(14.0, 10.0);
            let back = Rect::from_min_size(at, galley.size()).expand2(vec2(7.0, 4.0));
            painter.rect_filled(back, CornerRadius::same(3), t::MENU_BG);
            painter.rect_stroke(
                back,
                CornerRadius::same(3),
                Stroke::new(1.0, t::REFUSED),
                StrokeKind::Inside,
            );
            painter.galley(at, galley, t::TEXT_BRIGHT);
        }
        if ui.input(|i| i.pointer.any_released()) {
            if let (Some(edit), None) = (plan, refused) {
                edits.push(edit);
            }
            state.drag = None;
        }
    }

    if let Some(open) = &mut state.library {
        match library::show(ui, open, registry) {
            library::Outcome::Open => {}
            library::Outcome::Close => state.library = None,
            library::Outcome::Add(type_id) => {
                let node = state.ids.next_id();
                let at = open.at;
                let name = registry
                    .desc(&type_id)
                    .map_or(type_id.as_str(), |d| d.name.as_str());
                edits.push(Edit {
                    label: format!("Add {name}"),
                    commands: vec![
                        Command::AddNode {
                            parent: container,
                            id: node,
                            type_id: type_id.clone(),
                            name: None,
                            placement: None,
                        },
                        Command::SetPosition {
                            node,
                            position: Some([f64::from(at.x.round()), f64::from(at.y.round())]),
                        },
                    ],
                    gesture: None,
                });
                state.selection = BTreeSet::from([node]);
                *selected = Some(node);
                state.library = None;
            }
        }
    }

    if placed.is_empty() {
        painter.text(
            canvas.center(),
            Align2::CENTER_CENTER,
            "Right-click or press Tab to add a node",
            sans(12.0),
            t::TEXT3,
        );
    }
    painter.text(
        pos2(canvas.left() + 12.0, canvas.bottom() - 12.0),
        Align2::LEFT_BOTTOM,
        "Tab  add node   ·   F  frame all   ·   Delete  remove",
        mono(10.0),
        t::TEXT3,
    );
    painter.text(
        pos2(canvas.right() - 12.0, canvas.bottom() - 12.0),
        Align2::RIGHT_BOTTOM,
        format!("{:.0}%", view.zoom * 100.0),
        mono(10.0),
        t::TEXT3,
    );
    state.views.insert(container, view);
}

/// The edit a connection dropped on `target` (or on nothing) makes.
fn wire_edit(
    fixed: &PortRef,
    fixed_side: Side,
    detached: Option<&PortRef>,
    target: Option<&PortRef>,
) -> Option<Edit> {
    let edge = |from: &PortRef, to: &PortRef| Command::Connect {
        edge: Edge {
            from: from.clone(),
            to: to.clone(),
        },
    };
    let commands = match (fixed_side, detached, target) {
        // Dropped back where it was.
        (_, Some(input), Some(to)) if input == to => return None,
        (Side::Output, Some(input), Some(to)) => {
            vec![
                Command::Disconnect {
                    input: input.clone(),
                },
                edge(fixed, to),
            ]
        }
        (Side::Output, Some(input), None) => {
            return Some(Edit {
                label: "Disconnect".into(),
                commands: vec![Command::Disconnect {
                    input: input.clone(),
                }],
                gesture: None,
            });
        }
        (Side::Output, None, Some(to)) => vec![edge(fixed, to)],
        (Side::Input, _, Some(from)) => vec![edge(from, fixed)],
        (_, _, None) => return None,
    };
    Some(Edit {
        label: "Connect".into(),
        commands,
        gesture: None,
    })
}

/// Whether the document would take `commands`, and why not.
fn dry_run(doc: &Document, registry: &Registry, commands: &[Command]) -> Result<(), String> {
    let mut trial = doc.clone();
    for command in commands {
        command
            .apply(&mut trial, registry)
            .map_err(|e| sentence(&e.to_string()))?;
    }
    Ok(())
}

/// `"connecting a to b would create a loop"` → `"Connecting a to b would create a loop"`.
fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Lays out the graph's nodes and ports in editor units.
fn lay_out(
    doc: &Document,
    registry: &Registry,
    container: NodeId,
    graph: &Graph,
) -> (Vec<Placed>, Vec<Port>) {
    let auto = auto_positions(graph);
    let mut placed = vec![];
    let mut ports = vec![];
    for &id in &graph.nodes {
        let Some(node) = doc.node(id) else {
            continue;
        };
        let desc = node.as_known().and_then(|k| registry.desc(&k.type_id));
        let inputs: Vec<_> = desc.map_or(vec![], |d| d.inputs.iter().collect());
        let outputs: Vec<_> = desc.map_or(vec![], |d| d.outputs.iter().collect());
        let height = NODE_HEAD.max(14.0 + PORT_SPACING * inputs.len().max(outputs.len()) as f32);
        let min = match graph.positions.get(&id) {
            Some([x, y]) => pos2(*x as f32, *y as f32),
            None => auto.get(&id).copied().unwrap_or(Pos2::ZERO),
        };
        let rect = Rect::from_min_size(min, vec2(NODE_WIDTH, height));
        for (side, list, x) in [
            (Side::Input, &inputs, rect.left()),
            (Side::Output, &outputs, rect.right()),
        ] {
            for (i, p) in list.iter().enumerate() {
                ports.push(Port {
                    port: PortRef::new(id, &p.id),
                    side,
                    name: p.name.clone(),
                    ty: p.ty,
                    at: pos2(x, rect.top() + port_y(i, list.len(), height)),
                });
            }
        }
        let (color, icon) = category_look(desc);
        placed.push(Placed {
            id,
            rect,
            name: node.name().to_owned(),
            detail: detail(doc, container, node, desc),
            color,
            icon,
        });
    }
    (placed, ports)
}

/// A port's distance from the node's top: the middle of the head for one,
/// evenly spaced around the middle for several.
fn port_y(index: usize, count: usize, height: f32) -> f32 {
    if count <= 1 {
        return NODE_HEAD / 2.0;
    }
    let span = PORT_SPACING * (count - 1) as f32;
    height / 2.0 - span / 2.0 + PORT_SPACING * index as f32
}

/// Places nodes in columns by their depth from the sources, in id order
/// within a column.
fn auto_positions(graph: &Graph) -> BTreeMap<NodeId, Pos2> {
    fn depth(graph: &Graph, id: NodeId, memo: &mut BTreeMap<NodeId, usize>) -> usize {
        if let Some(d) = memo.get(&id) {
            return *d;
        }
        // Graphs have no loops (the document refuses them), so this ends.
        let sources: Vec<NodeId> = graph
            .inputs
            .iter()
            .filter(|(to, _)| to.node == id)
            .map(|(_, from)| from.node)
            .collect();
        let d = sources
            .into_iter()
            .map(|s| depth(graph, s, memo) + 1)
            .max()
            .unwrap_or(0);
        memo.insert(id, d);
        d
    }
    let mut memo = BTreeMap::new();
    let mut rows: BTreeMap<usize, usize> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for &id in &graph.nodes {
        let column = depth(graph, id, &mut memo);
        let row = rows.entry(column).or_default();
        out.insert(id, pos2(column as f32 * COLUMN, *row as f32 * ROW));
        *row += 1;
    }
    out
}

/// The second line on a node: the file it reads, what it is when its name
/// doesn't say, or its first number.
fn detail(doc: &Document, container: NodeId, node: &Node, desc: Option<&NodeTypeDesc>) -> String {
    let Some(known) = node.as_known() else {
        return format!("{} isn't installed", node.type_id());
    };
    let Some(desc) = desc else {
        return known.type_id.clone();
    };
    let nested = container != doc.root();
    // Inside a clip, Input is the clip's media and Output goes back to the timeline.
    let from = if nested && known.type_id == INPUT_TYPE {
        doc.node(container).and_then(Node::as_known)
    } else {
        Some(known)
    };
    if let Some(media) = from.and_then(media_name) {
        return media;
    }
    if nested && known.type_id == OUTPUT_TYPE {
        return format!("→ {}", doc.node(doc.root()).map_or("", Node::name));
    }
    if desc.name != known.name {
        return desc.name.clone();
    }
    let number = desc.params.iter().find_map(|p| {
        let fw_plugin::ParamKind::Float { unit } = p.kind else {
            return None;
        };
        let value = match known.params.get(&p.id) {
            Some(fw_doc::Param::Constant(v)) => v.as_f64()?,
            Some(fw_doc::Param::Keyframed(_)) => return Some(format!("{} animated", p.label)),
            None => p.default.as_f64()?,
        };
        let number = Number {
            unit,
            slider: None,
            limits: None,
        };
        Some(number.show(value))
    });
    number.unwrap_or_else(|| desc.category.clone())
}

/// The file name of the first media the node reads.
fn media_name(node: &fw_doc::KnownNode) -> Option<String> {
    node.params.values().find_map(|p| match p {
        fw_doc::Param::Constant(fw_plugin::ParamValue::Media(m)) if !m.path.is_empty() => {
            std::path::Path::new(&m.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        }
        _ => None,
    })
}

/// The color strip and icon for a node's category.
fn category_look(desc: Option<&NodeTypeDesc>) -> (Color32, &'static str) {
    let Some(category) = desc.map(|d| d.category.as_str()) else {
        return (t::TEXT_DISABLED, icons::FILTER);
    };
    let icon = match category {
        "Sources" => icons::IMAGE,
        "Color" => icons::COLOR,
        "Outputs" => icons::OUTPUT,
        "Containers" => icons::NEST,
        _ => icons::FILTER,
    };
    (library::category_color(category), icon)
}

fn port_color(ty: PortType) -> Color32 {
    match ty {
        PortType::Image => t::PORT_IMAGE,
        PortType::Mask => t::PORT_MASK,
        PortType::Audio => t::CLIP_AUDIO.1,
        PortType::Scene => t::NODE_CONTAINER,
        PortType::Value => t::NODE_COLOR,
    }
}

fn wire_color(ty: PortType) -> Color32 {
    match ty {
        PortType::Image => t::WIRE,
        other => port_color(other),
    }
}

/// Zooms to show every node, never past 100%.
fn frame(view: &mut View, canvas: Rect, placed: &[Placed]) {
    view.framed = true;
    let Some(bounds) = placed.iter().map(|p| p.rect).reduce(|a, b| a.union(b)) else {
        view.zoom = 1.0;
        view.offset = canvas.size() / 2.0;
        return;
    };
    let room = canvas.shrink(48.0);
    view.zoom = (room.width() / bounds.width())
        .min(room.height() / bounds.height())
        .clamp(MIN_ZOOM, 1.0);
    view.offset = canvas.size() / 2.0 - bounds.center().to_vec2() * view.zoom;
}

/// Ctrl+wheel and pinches zoom around the pointer; the wheel pans.
fn pan_and_zoom(ui: &Ui, view: &mut View, canvas: Rect, pointer: Pos2) {
    let (zoom, scroll, middle) = ui.input(|i| {
        let middle = if i.pointer.middle_down() {
            i.pointer.delta()
        } else {
            Vec2::ZERO
        };
        (i.zoom_delta(), i.smooth_scroll_delta, middle)
    });
    if zoom != 1.0 {
        let at = view.to_editor(canvas, pointer);
        view.zoom = (view.zoom * zoom).clamp(MIN_ZOOM, MAX_ZOOM);
        view.offset = pointer - canvas.min - at.to_vec2() * view.zoom;
    }
    view.offset += scroll + middle;
}

/// The well behind the nodes: a fine grid with a coarser one over it.
fn grid(painter: &egui::Painter, canvas: Rect, view: &View) {
    painter.rect_filled(canvas, CornerRadius::ZERO, t::WELL);
    for (step, color) in [(16.0, t::GRID_FINE), (80.0, t::GRID)] {
        let spacing = step * view.zoom;
        if spacing < 6.0 {
            continue;
        }
        let origin = canvas.min + view.offset;
        let mut x = origin.x + ((canvas.left() - origin.x) / spacing).ceil() * spacing;
        while x < canvas.right() {
            painter.vline(x, canvas.y_range(), Stroke::new(1.0, color));
            x += spacing;
        }
        let mut y = origin.y + ((canvas.top() - origin.y) / spacing).ceil() * spacing;
        while y < canvas.bottom() {
            painter.hline(canvas.x_range(), y, Stroke::new(1.0, color));
            y += spacing;
        }
    }
}

/// A connection from an output at `a` to an input at `b`.
fn wire(painter: &egui::Painter, a: Pos2, b: Pos2, zoom: f32, stroke: Stroke) {
    let reach = ((b.x - a.x) * 0.5).max(30.0 * zoom);
    let curve = egui::epaint::CubicBezierShape::from_points_stroke(
        [a, a + vec2(reach, 0.0), b - vec2(reach, 0.0), b],
        false,
        Color32::TRANSPARENT,
        stroke,
    );
    painter.add(curve);
}

fn draw_node(painter: &egui::Painter, node: &Placed, rect: Rect, zoom: f32, selected: bool) {
    let radius = CornerRadius::same((4.0 * zoom).round().clamp(1.0, 8.0) as u8);
    let shadow = egui::epaint::Shadow {
        offset: [0, 2],
        blur: 6,
        spread: 0,
        color: Color32::from_black_alpha(115),
    };
    painter.add(shadow.as_shape(rect, radius));
    painter.rect_filled(rect, radius, t::NODE);
    let border = if selected { t::ACCENT } else { t::MENU_BORDER };
    painter.rect_stroke(rect, radius, Stroke::new(1.0, border), StrokeKind::Inside);
    if selected {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, t::ACCENT),
            StrokeKind::Outside,
        );
    }
    let strip = Rect::from_min_max(
        rect.min,
        pos2(rect.right(), rect.top() + 2.0 * zoom.max(0.5)),
    );
    painter.rect_filled(
        strip.shrink2(vec2(1.0, 0.0)),
        CornerRadius {
            nw: radius.nw,
            ne: radius.ne,
            sw: 0,
            se: 0,
        },
        node.color,
    );

    let head = Rect::from_min_size(rect.min, vec2(rect.width(), NODE_HEAD * zoom));
    let well = Rect::from_min_size(head.min + vec2(8.0, 9.0) * zoom, vec2(34.0, 28.0) * zoom);
    painter.rect_filled(well, CornerRadius::same((2.0 * zoom) as u8), t::WELL);
    icons::paint(
        painter,
        Rect::from_center_size(well.center(), Vec2::splat(14.0 * zoom)),
        node.icon,
        node.color,
        1.6 * zoom.min(1.0),
        false,
    );
    let text = painter.with_clip_rect(rect.shrink(1.0).intersect(painter.clip_rect()));
    let left = well.right() + 8.0 * zoom;
    if zoom >= 0.6 {
        text.text(
            pos2(left, head.top() + 17.0 * zoom),
            Align2::LEFT_CENTER,
            &node.name,
            medium(11.5 * zoom),
            t::TEXT,
        );
        text.text(
            pos2(left, head.top() + 31.0 * zoom),
            Align2::LEFT_CENTER,
            &node.detail,
            mono(9.5 * zoom),
            t::TEXT3,
        );
    } else {
        text.text(
            pos2(left, head.center().y),
            Align2::LEFT_CENTER,
            &node.name,
            medium(11.5 * zoom),
            t::TEXT,
        );
    }
}
