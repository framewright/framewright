//! The app's state and its one page so far: chrome around docked panels.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Stroke, Ui, vec2};
use egui_tiles::{TileId, Tiles, Tree};
use fw_core::id::{IdGen, RandomIds};
use fw_core::{NodeId, Time};
use fw_doc::{Command, Edge, PortRef};
use fw_engine::{Engine, MediaSummary, Request};

use crate::chrome::{self, AppCommand, TitleBar, WindowAction, Workspace};
use crate::controls::Gestures;
use crate::icons;
use crate::inspector::{self, Inspector};
use crate::lists;
use crate::node_editor::{self, NodeEditor};
use crate::style::{sans, tokens as t};
use crate::timeline::{self, TimelineView};
use crate::viewer::{self, Shown, ViewerState};

/// An edit a panel asks for, sent to the engine as [`Request::Edit`].
pub struct Edit {
    pub label: String,
    pub commands: Vec<Command>,
    pub gesture: Option<u64>,
}

/// A panel that can be docked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Media,
    Nodes,
    Viewer,
    Inspector,
    Timeline,
}

impl Pane {
    fn title(self) -> &'static str {
        match self {
            Pane::Media => "Media",
            Pane::Nodes => "Nodes",
            Pane::Viewer => "Viewer",
            Pane::Inspector => "Inspector",
            Pane::Timeline => "Timeline",
        }
    }
}

/// What kind of document is open, which decides the workspaces (design §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Video,
    /// A graph: shown on the Compose page.
    Graph,
}

const VIDEO_WORKSPACES: [Workspace; 7] = [
    Workspace {
        name: "Media",
        icon: icons::FOLDER,
        built: false,
    },
    Workspace {
        name: "Edit",
        icon: icons::FILM,
        built: true,
    },
    Workspace {
        name: "Compose",
        icon: icons::GRAPH,
        built: true,
    },
    Workspace {
        name: "Grade",
        icon: icons::GRADE,
        built: false,
    },
    Workspace {
        name: "3D",
        icon: icons::CUBE,
        built: false,
    },
    Workspace {
        name: "Audio",
        icon: icons::WAVE,
        built: false,
    },
    Workspace {
        name: "Deliver",
        icon: icons::DELIVER,
        built: false,
    },
];

/// A graph opens on Compose, the node editor's page.
const GRAPH_WORKSPACES: [Workspace; 2] = [
    Workspace {
        name: "Compose",
        icon: icons::GRAPH,
        built: true,
    },
    Workspace {
        name: "Deliver",
        icon: icons::DELIVER,
        built: false,
    },
];

/// The page on screen. Which one is UI state, never saved or undone
/// (design §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Edit,
    /// The node editor on this container's graph: a graph document's root,
    /// or a clip opened in Compose.
    Compose(NodeId),
}

pub struct App {
    kind: Kind,
    page: Page,
    /// The Edit page's panels, for timelines.
    edit: Tree<Pane>,
    /// The Compose page's panels.
    compose: Tree<Pane>,
    /// The frame on screen, set before each frame of the UI.
    pub shown: Option<Shown>,
    /// What the UI asked of the OS window this frame.
    pub actions: Vec<WindowAction>,
    pub maximised: bool,
    /// The last thing that went wrong, shown in the workspace bar.
    message: Option<String>,
    gpu_name: String,
    /// The node the inspector shows. UI state: selecting isn't an edit.
    selected: Option<NodeId>,
    gestures: Gestures,
    inspector: Inspector,
    timeline: TimelineView,
    node_editor: NodeEditor,
    /// The media the document uses, as of this frame.
    media: Vec<MediaSummary>,
    ids: RandomIds,
}

impl App {
    pub fn new(engine: &Engine) -> App {
        let doc = engine.document();
        let kind = if doc.timeline(doc.root()).is_some() {
            Kind::Video
        } else {
            Kind::Graph
        };
        let page = match kind {
            Kind::Video => Page::Edit,
            Kind::Graph => Page::Compose(doc.root()),
        };
        App {
            kind,
            page,
            edit: edit_layout(),
            compose: compose_layout(),
            shown: None,
            actions: vec![],
            maximised: false,
            message: None,
            gpu_name: engine.gpu().info.name.clone(),
            selected: None,
            gestures: Gestures::default(),
            inspector: Inspector::default(),
            timeline: TimelineView::default(),
            node_editor: NodeEditor::default(),
            media: vec![],
            ids: RandomIds::default(),
        }
    }

    /// Shows `message` in the workspace bar, as the last thing to report.
    pub fn report(&mut self, message: String) {
        self.message = Some(message);
    }

    pub fn select(&mut self, node: Option<NodeId>) {
        self.selected = node;
    }

    pub fn ui(&mut self, ui: &mut Ui, engine: &mut Engine) {
        let mut commands = vec![];
        let mut edits = vec![];
        self.media = engine.media().to_vec();
        if self
            .selected
            .is_some_and(|id| !engine.document().contains(id))
        {
            self.selected = None;
        }
        // Undo can take away the graph being shown.
        if let Page::Compose(container) = self.page
            && engine.document().graph(container).is_none()
        {
            self.back_to_timeline(engine);
        }
        if !self.maximised
            && let Some(direction) = chrome::resize_edges(ui.ctx())
        {
            self.actions.push(WindowAction::Resize(direction));
        }
        chrome::shortcuts(ui.ctx(), &mut commands);

        let project = engine
            .path()
            .file_stem()
            .map_or("Untitled".into(), |s| s.to_string_lossy().into_owned());
        let bar = TitleBar {
            project: &project,
            modified: engine.is_modified(),
            undo: engine.undo_label(),
            redo: engine.redo_label(),
            maximised: self.maximised,
            selection: self.selected.is_some(),
            clip_selected: (self.kind == Kind::Video)
                .then(|| self.selected.is_some_and(|id| is_clip(engine, id))),
        };
        egui::Panel::top("title-bar")
            .exact_size(chrome::TITLE_BAR_HEIGHT)
            .frame(egui::Frame::new().fill(t::CHROME))
            .show_separator_line(false)
            .show(ui, |ui| {
                chrome::title_bar(ui, &bar, &mut self.actions, &mut commands)
            });

        let (workspaces, active): (&[Workspace], usize) = match (self.kind, self.page) {
            (Kind::Video, Page::Edit) => (&VIDEO_WORKSPACES, 1),
            (Kind::Video, Page::Compose(_)) => (&VIDEO_WORKSPACES, 2),
            (Kind::Graph, _) => (&GRAPH_WORKSPACES, 0),
        };
        let left = match &self.message {
            Some(message) => message.clone(),
            None => self.describe(engine),
        };
        let status = engine.playback();
        let right = match status.dropped {
            0 => format!("Draft · {}", self.gpu_name),
            1 => format!("1 frame dropped · Draft · {}", self.gpu_name),
            n => format!("{n} frames dropped · Draft · {}", self.gpu_name),
        };
        egui::Panel::bottom("workspace-bar")
            .exact_size(chrome::WORKSPACE_BAR_HEIGHT)
            .frame(egui::Frame::new().fill(t::CHROME))
            .show_separator_line(false)
            .show(ui, |ui| {
                chrome::workspace_bar(ui, workspaces, active, &left, &right, &mut commands)
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(t::BG)
                    .inner_margin(egui::Margin::same(4)),
            )
            .show(ui, |ui| {
                let page = match self.page {
                    Page::Edit => &mut self.edit,
                    Page::Compose(_) => &mut self.compose,
                };
                let mut tree = std::mem::replace(page, Tree::empty(egui::Id::unique("swap")));
                let mut behaviour = Behaviour {
                    app: self,
                    engine,
                    commands: &mut commands,
                    edits: &mut edits,
                    root: tree.root,
                };
                tree.ui(&mut behaviour, ui);
                match self.page {
                    Page::Edit => self.edit = tree,
                    Page::Compose(_) => self.compose = tree,
                }
            });

        // A resize from the edge wins over a drag of the title bar under it.
        if self
            .actions
            .iter()
            .any(|a| matches!(a, WindowAction::Resize(_)))
        {
            self.actions.retain(|a| *a != WindowAction::Drag);
        }
        for edit in edits {
            let request = Request::Edit {
                label: edit.label,
                commands: edit.commands,
                gesture: edit.gesture,
            };
            self.message = engine.request(request).err().map(|e| e.to_string());
        }
        for command in commands {
            self.run(command, engine);
        }
    }

    pub fn run(&mut self, command: AppCommand, engine: &mut Engine) {
        let rate = engine.frame_rate();
        let index = engine.playhead().frame_index(rate).unwrap_or(0);
        let frame = |i: i64| Time::of_frame(i.max(0), rate).unwrap_or(Time::ZERO);
        let request = match command {
            AppCommand::Save => Request::Save,
            AppCommand::Undo => Request::Undo,
            AppCommand::Redo => Request::Redo,
            AppCommand::Seek(time) => Request::Seek { time },
            AppCommand::StepBack => Request::Seek {
                time: frame(index - 1),
            },
            AppCommand::StepForward => Request::Seek {
                time: frame(index + 1),
            },
            AppCommand::GoToStart => Request::Seek { time: Time::ZERO },
            AppCommand::GoToEnd => {
                // The last frame, not the moment after it.
                let end = engine.duration().frame_index(rate).unwrap_or(0);
                Request::Seek {
                    time: frame(end - 1),
                }
            }
            AppCommand::Delete if matches!(self.page, Page::Compose(_)) => {
                let nodes: Vec<NodeId> = self.node_editor.selection().iter().copied().collect();
                self.selected = None;
                if nodes.is_empty() {
                    return;
                }
                Request::Edit {
                    label: if nodes.len() == 1 {
                        "Delete Node"
                    } else {
                        "Delete Nodes"
                    }
                    .into(),
                    commands: nodes
                        .into_iter()
                        .map(|id| Command::RemoveNode { id })
                        .collect(),
                    gesture: None,
                }
            }
            AppCommand::Delete => {
                let Some(node) = self.selected.take() else {
                    return;
                };
                let doc = engine.document();
                let is_clip = doc
                    .timeline(doc.root())
                    .is_some_and(|t| t.clips.contains_key(&node));
                Request::Edit {
                    label: if is_clip {
                        "Delete Clip"
                    } else {
                        "Delete Node"
                    }
                    .into(),
                    commands: vec![Command::RemoveNode { id: node }],
                    gesture: None,
                }
            }
            AppCommand::PlayPause if engine.is_playing() => Request::Pause,
            AppCommand::PlayPause | AppCommand::Play => Request::Play,
            AppCommand::Pause => Request::Pause,
            AppCommand::Quit => {
                self.actions.push(WindowAction::Close);
                return;
            }
            AppCommand::ResetLayout => {
                match self.page {
                    Page::Edit => self.edit = edit_layout(),
                    Page::Compose(_) => self.compose = compose_layout(),
                }
                return;
            }
            AppCommand::OpenInCompose => {
                if let Some(clip) = self.selected.filter(|id| is_clip(engine, *id))
                    && matches!(self.page, Page::Edit)
                {
                    self.open_in_compose(clip, engine);
                }
                return;
            }
            AppCommand::Workspace("Edit") => {
                self.back_to_timeline(engine);
                return;
            }
            AppCommand::Workspace("Compose") => {
                match self.selected.filter(|id| is_clip(engine, *id)) {
                    Some(clip) => self.open_in_compose(clip, engine),
                    None => {
                        self.message = Some("Select a clip to open it in Compose".into());
                    }
                }
                return;
            }
            AppCommand::Workspace(_) => return,
        };
        self.message = engine.request(request).err().map(|e| e.to_string());
    }
}

impl App {
    /// The container whose graph the node editor shows.
    fn container(&self, engine: &Engine) -> NodeId {
        match self.page {
            Page::Compose(container) => container,
            Page::Edit => engine.document().root(),
        }
    }

    /// Steps into `clip`'s graph, first giving it one that passes its media
    /// straight through (Input → Output) when it has none.
    fn open_in_compose(&mut self, clip: NodeId, engine: &mut Engine) {
        if engine.document().graph(clip).is_none() {
            let (input, output) = (self.ids.next_id(), self.ids.next_id());
            let add = |id, type_id: &str, x: f64| {
                [
                    Command::AddNode {
                        parent: clip,
                        id,
                        type_id: type_id.into(),
                        name: None,
                        placement: None,
                    },
                    Command::SetPosition {
                        node: id,
                        position: Some([x, 0.0]),
                    },
                ]
            };
            let mut commands = vec![Command::MakeGraph { node: clip }];
            commands.extend(add(input, fw_plugin::INPUT_TYPE, 0.0));
            commands.extend(add(output, fw_plugin::OUTPUT_TYPE, 260.0));
            commands.push(Command::Connect {
                edge: Edge {
                    from: PortRef::new(input, "image"),
                    to: PortRef::new(output, "image"),
                },
            });
            let request = Request::Edit {
                label: "Create Clip Graph".into(),
                commands,
                gesture: None,
            };
            if let Err(e) = engine.request(request) {
                self.message = Some(e.to_string());
                return;
            }
        }
        self.page = Page::Compose(clip);
        self.selected = None;
        self.node_editor.frame_all(clip);
    }

    /// Back to the Edit page, with the clip that was open selected.
    fn back_to_timeline(&mut self, engine: &Engine) {
        if self.kind != Kind::Video {
            return;
        }
        if let Page::Compose(clip) = self.page {
            self.selected = engine.document().contains(clip).then_some(clip);
        }
        self.page = Page::Edit;
    }

    /// What's open, for the workspace bar's left end.
    fn describe(&self, engine: &Engine) -> String {
        let doc = engine.document();
        if let (Kind::Video, Page::Compose(clip)) = (self.kind, self.page) {
            let count = doc.graph(clip).map_or(0, |g| g.nodes.len());
            let name = doc.node(clip).map_or("Clip", |n| n.name());
            return format!("{name} › Graph · {}", nodes(count));
        }
        describe(engine)
    }
}

/// Whether `id` is a clip on the root timeline.
fn is_clip(engine: &Engine, id: NodeId) -> bool {
    let doc = engine.document();
    doc.timeline(doc.root())
        .is_some_and(|t| t.clips.contains_key(&id))
}

/// `1 node`, `3 nodes`.
fn nodes(count: usize) -> String {
    match count {
        1 => "1 node".into(),
        n => format!("{n} nodes"),
    }
}

/// `Timeline · 1920×1080 · 24 fps`, or what's open when it isn't a timeline.
fn describe(engine: &Engine) -> String {
    let doc = engine.document();
    match doc.timeline(doc.root()) {
        Some(timeline) => {
            let s = &timeline.settings;
            let rate = s.rate.as_f64();
            let rate = if rate.fract() == 0.0 {
                format!("{rate}")
            } else {
                format!("{rate:.3}")
            };
            format!("Timeline · {}×{} · {rate} fps", s.width, s.height)
        }
        None => format!(
            "Graph · {}",
            nodes(doc.graph(doc.root()).map_or(0, |g| g.nodes.len()))
        ),
    }
}

/// The Edit page's panels as first laid out.
fn edit_layout() -> Tree<Pane> {
    let mut tiles = Tiles::default();
    let left = tiles.insert_pane(Pane::Media);
    let left = tiles.insert_tab_tile(vec![left]);
    let viewer = tiles.insert_pane(Pane::Viewer);
    let centre = tiles.insert_tab_tile(vec![viewer]);
    let inspector = tiles.insert_pane(Pane::Inspector);
    let right = tiles.insert_tab_tile(vec![inspector]);
    let top = tiles.insert_horizontal_tile(vec![left, centre, right]);
    set_shares(
        &mut tiles,
        top,
        &[(left, 300.0), (centre, 972.0), (right, 320.0)],
    );
    let timeline = tiles.insert_pane(Pane::Timeline);
    let bottom = tiles.insert_tab_tile(vec![timeline]);
    let root = tiles.insert_vertical_tile(vec![top, bottom]);
    set_shares(&mut tiles, root, &[(top, 562.0), (bottom, 344.0)]);
    Tree::new(egui::Id::unique("studio-page"), root, tiles)
}

/// The Compose page: the viewer over the node editor, with the inspector
/// down the right.
fn compose_layout() -> Tree<Pane> {
    let mut tiles = Tiles::default();
    let viewer = tiles.insert_pane(Pane::Viewer);
    let viewer = tiles.insert_tab_tile(vec![viewer]);
    let nodes = tiles.insert_pane(Pane::Nodes);
    let nodes = tiles.insert_tab_tile(vec![nodes]);
    let left = tiles.insert_vertical_tile(vec![viewer, nodes]);
    set_shares(&mut tiles, left, &[(viewer, 400.0), (nodes, 506.0)]);
    let inspector = tiles.insert_pane(Pane::Inspector);
    let inspector = tiles.insert_tab_tile(vec![inspector]);
    let root = tiles.insert_horizontal_tile(vec![left, inspector]);
    set_shares(&mut tiles, root, &[(left, 1262.0), (inspector, 330.0)]);
    Tree::new(egui::Id::unique("compose-page"), root, tiles)
}

fn set_shares(tiles: &mut Tiles<Pane>, container: TileId, shares: &[(TileId, f32)]) {
    if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(l))) =
        tiles.get_mut(container)
    {
        for (tile, share) in shares {
            l.shares.set_share(*tile, *share);
        }
    }
}

struct Behaviour<'a> {
    app: &'a mut App,
    engine: &'a Engine,
    commands: &'a mut Vec<AppCommand>,
    edits: &'a mut Vec<Edit>,
    root: Option<TileId>,
}

impl egui_tiles::Behavior<Pane> for Behaviour<'_> {
    fn pane_ui(&mut self, ui: &mut Ui, _tile: TileId, pane: &mut Pane) -> egui_tiles::UiResponse {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, CornerRadius::ZERO, t::PANEL);
        match pane {
            Pane::Viewer => {
                let doc = self.engine.document();
                let aspect = doc.timeline(doc.root()).map_or(16.0 / 9.0, |t| {
                    t.settings.width as f32 / t.settings.height as f32
                });
                let state = ViewerState {
                    shown: self.app.shown.as_ref(),
                    failure: self.engine.failure(),
                    aspect,
                    playhead: self.engine.playhead(),
                    duration: self.engine.duration(),
                    rate: self.engine.frame_rate(),
                    playing: self.engine.is_playing(),
                };
                viewer::viewer(ui, &state, self.commands);
            }
            Pane::Media => lists::media(ui, &self.app.media),
            Pane::Nodes => {
                let container = self.app.container(self.engine);
                node_editor::node_editor(
                    ui,
                    &mut self.app.node_editor,
                    &mut self.app.gestures,
                    self.engine,
                    container,
                    &mut self.app.selected,
                    self.edits,
                );
            }
            Pane::Inspector => inspector::inspector(
                ui,
                &mut self.app.inspector,
                &mut self.app.gestures,
                self.engine,
                self.app.selected,
                match self.app.page {
                    Page::Edit => "Select a clip to see its settings",
                    Page::Compose(_) => "Select a node to see its settings",
                },
                self.edits,
            ),
            Pane::Timeline => timeline::timeline(
                ui,
                &mut self.app.timeline,
                &mut self.app.gestures,
                self.engine,
                &mut self.app.selected,
                self.edits,
                self.commands,
            ),
        }
        egui_tiles::UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        pane.title().into()
    }

    fn tab_bar_height(&self, _style: &egui::Style) -> f32 {
        30.0
    }

    fn gap_width(&self, _style: &egui::Style) -> f32 {
        4.0
    }

    fn tab_bar_color(&self, _visuals: &egui::Visuals) -> Color32 {
        t::PANEL_HEADER
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions {
            all_panes_must_have_tabs: true,
            ..Default::default()
        }
    }

    fn resize_stroke(&self, _style: &egui::Style, state: egui_tiles::ResizeState) -> Stroke {
        match state {
            egui_tiles::ResizeState::Idle => Stroke::NONE,
            _ => Stroke::new(2.0, t::ACCENT),
        }
    }

    fn tab_ui(
        &mut self,
        tiles: &mut Tiles<Pane>,
        ui: &mut Ui,
        id: egui::Id,
        tile: TileId,
        state: &egui_tiles::TabState,
    ) -> egui::Response {
        let title = self.tab_title_for_tile(tiles, tile).text().to_owned();
        let text = ui
            .painter()
            .layout_no_wrap(title.clone(), sans(12.0), t::TEXT2);
        let width = text.size().x + 18.0;
        let (_, outer) = ui.allocate_space(vec2(width + 2.0, ui.available_height()));
        let rect = Rect::from_center_size(outer.center(), vec2(width, 24.0));
        let response = ui.interact(rect, id, egui::Sense::click_and_drag());
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, true, state.active, &title)
        });
        if !state.is_being_dragged {
            if state.active {
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(3), t::RAISED2);
            }
            let colour = if state.active || response.hovered() {
                t::TEXT_BRIGHT
            } else {
                t::TEXT2
            };
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                &title,
                sans(12.0),
                colour,
            );
        }
        response
    }

    fn tab_bar_left_ui(
        &mut self,
        tiles: &Tiles<Pane>,
        ui: &mut Ui,
        _tile: TileId,
        tabs: &egui_tiles::Tabs,
    ) {
        ui.add_space(4.0);
        let active = tabs.active.and_then(|a| tiles.get_pane(&a)).copied();
        let doc = self.engine.document();
        if active == Some(Pane::Nodes)
            && let Page::Compose(container) = self.app.page
            && container != doc.root()
        {
            // The way back names where it goes (design §3.2).
            let name = doc.node(doc.root()).map_or("Timeline", |n| n.name());
            let back = ui.add(egui::Button::new(
                egui::RichText::new(format!("← {name}")).font(sans(11.0)),
            ));
            if back.clicked() {
                self.commands.push(AppCommand::Workspace("Edit"));
            }
            ui.add_space(4.0);
        }
    }

    fn top_bar_right_ui(
        &mut self,
        tiles: &Tiles<Pane>,
        ui: &mut Ui,
        _tile: TileId,
        tabs: &egui_tiles::Tabs,
        _scroll: &mut f32,
    ) {
        let active = tabs.active.and_then(|a| tiles.get_pane(&a)).copied();
        if active == Some(Pane::Viewer) {
            ui.add_space(10.0);
            let name = self
                .engine
                .path()
                .file_stem()
                .map_or("Untitled".into(), |s| s.to_string_lossy().into_owned());
            ui.label(
                egui::RichText::new(name)
                    .font(crate::style::medium(12.0))
                    .color(t::TEXT_BRIGHT),
            );
        }
        if active == Some(Pane::Nodes) {
            let container = self.app.container(self.engine);
            ui.add_space(8.0);
            let frame = ui.add(egui::Button::new(
                egui::RichText::new("Frame All").font(sans(11.0)),
            ));
            if frame.on_hover_text("Show every node (F)").clicked() {
                self.app.node_editor.frame_all(container);
            }
            let doc = self.engine.document();
            let count = doc.graph(container).map_or(0, |g| g.nodes.len());
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(nodes(count))
                    .font(sans(11.0))
                    .color(t::TEXT3),
            );
            if container != doc.root() {
                // Where this graph is: "Main Timeline › Clip › Graph".
                ui.add_space(12.0);
                let path = format!(
                    "{} › {} › Graph",
                    doc.node(doc.root()).map_or("", |n| n.name()),
                    doc.node(container).map_or("", |n| n.name())
                );
                ui.label(egui::RichText::new(path).font(sans(11.0)).color(t::TEXT2));
            }
        }
        if active == Some(Pane::Timeline) {
            ui.add_space(8.0);
            let fit = ui.add(egui::Button::new(
                egui::RichText::new("Zoom to Fit").font(sans(11.0)),
            ));
            if fit
                .on_hover_text("Fit the whole edit in the panel")
                .clicked()
            {
                self.app.timeline.fit();
            }
        }
    }

    fn paint_on_top_of_tile(
        &self,
        painter: &egui::Painter,
        _style: &egui::Style,
        tile: TileId,
        rect: Rect,
    ) {
        if self.root != Some(tile) {
            round_corners(painter, rect, 4.0, t::BG);
        }
    }
}

/// Masks a panel's square corners with the background, so it looks rounded.
fn round_corners(painter: &egui::Painter, r: Rect, radius: f32, bg: Color32) {
    let corners = [
        (r.left_top(), vec2(1.0, 1.0)),
        (r.right_top(), vec2(-1.0, 1.0)),
        (r.left_bottom(), vec2(1.0, -1.0)),
        (r.right_bottom(), vec2(-1.0, -1.0)),
    ];
    for (corner, dir) in corners {
        let centre = corner + dir * radius;
        let mut points: Vec<Pos2> = vec![corner];
        for k in 0..=6 {
            let a = k as f32 / 6.0 * std::f32::consts::FRAC_PI_2;
            points.push(centre - vec2(dir.x * radius * a.cos(), dir.y * radius * a.sin()));
        }
        let mut mesh = egui::Mesh::default();
        for (i, q) in points.iter().enumerate() {
            mesh.colored_vertex(*q, bg);
            if i >= 2 {
                mesh.add_triangle(0, i as u32 - 1, i as u32);
            }
        }
        painter.add(mesh);
    }
}
