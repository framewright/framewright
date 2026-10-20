//! The app's state and its one page so far: chrome around docked panels.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Stroke, Ui, vec2};
use egui_tiles::{TileId, Tiles, Tree};
use fw_core::Time;
use fw_engine::{Engine, Request};

use crate::chrome::{self, AppCommand, TitleBar, WindowAction, Workspace};
use crate::icons;
use crate::style::{sans, tokens as t};
use crate::viewer::{self, Shown, ViewerState};

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
        built: false,
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

/// A graph opens on Compose; until the node editor exists, its page lists
/// the nodes instead.
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

pub struct App {
    kind: Kind,
    tree: Tree<Pane>,
    /// The frame on screen, set before each frame of the UI.
    pub shown: Option<Shown>,
    /// What the UI asked of the OS window this frame.
    pub actions: Vec<WindowAction>,
    pub maximised: bool,
    /// The last thing that went wrong, shown in the workspace bar.
    message: Option<String>,
    gpu_name: String,
}

impl App {
    pub fn new(engine: &Engine) -> App {
        let doc = engine.document();
        let kind = if doc.timeline(doc.root()).is_some() {
            Kind::Video
        } else {
            Kind::Graph
        };
        App {
            kind,
            tree: layout(kind),
            shown: None,
            actions: vec![],
            maximised: false,
            message: None,
            gpu_name: engine.gpu().info.name.clone(),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, engine: &mut Engine) {
        let mut commands = vec![];
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
        };
        egui::Panel::top("title-bar")
            .exact_size(chrome::TITLE_BAR_HEIGHT)
            .frame(egui::Frame::new().fill(t::CHROME))
            .show_separator_line(false)
            .show(ui, |ui| {
                chrome::title_bar(ui, &bar, &mut self.actions, &mut commands)
            });

        let (workspaces, active): (&[Workspace], usize) = match self.kind {
            Kind::Video => (&VIDEO_WORKSPACES, 1),
            Kind::Graph => (&GRAPH_WORKSPACES, 0),
        };
        let left = match &self.message {
            Some(message) => message.clone(),
            None => describe(engine),
        };
        let right = format!("Draft · {}", self.gpu_name);
        egui::Panel::bottom("workspace-bar")
            .exact_size(chrome::WORKSPACE_BAR_HEIGHT)
            .frame(egui::Frame::new().fill(t::CHROME))
            .show_separator_line(false)
            .show(ui, |ui| {
                chrome::workspace_bar(ui, workspaces, active, &left, &right)
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(t::BG)
                    .inner_margin(egui::Margin::same(4)),
            )
            .show(ui, |ui| {
                let mut tree =
                    std::mem::replace(&mut self.tree, Tree::empty(egui::Id::unique("swap")));
                let mut behaviour = Behaviour {
                    app: self,
                    engine,
                    commands: &mut commands,
                    root: tree.root,
                };
                tree.ui(&mut behaviour, ui);
                self.tree = tree;
            });

        // A resize from the edge wins over a drag of the title bar under it.
        if self
            .actions
            .iter()
            .any(|a| matches!(a, WindowAction::Resize(_)))
        {
            self.actions.retain(|a| *a != WindowAction::Drag);
        }
        for command in commands {
            self.run(command, engine);
        }
    }

    fn run(&mut self, command: AppCommand, engine: &mut Engine) {
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
            AppCommand::Quit => {
                self.actions.push(WindowAction::Close);
                return;
            }
            AppCommand::ResetLayout => {
                self.tree = layout(self.kind);
                return;
            }
        };
        self.message = engine.request(request).err().map(|e| e.to_string());
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
        None => format!("Graph · {} nodes", doc.len().saturating_sub(1)),
    }
}

/// The page's panels as first laid out.
fn layout(kind: Kind) -> Tree<Pane> {
    let mut tiles = Tiles::default();
    let left = match kind {
        Kind::Video => tiles.insert_pane(Pane::Media),
        Kind::Graph => tiles.insert_pane(Pane::Nodes),
    };
    let left = tiles.insert_tab_tile(vec![left]);
    let viewer = tiles.insert_pane(Pane::Viewer);
    let centre = tiles.insert_tab_tile(vec![viewer]);
    let inspector = tiles.insert_pane(Pane::Inspector);
    let right = tiles.insert_tab_tile(vec![inspector]);
    let top = tiles.insert_horizontal_tile(vec![left, centre, right]);
    if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(l))) = tiles.get_mut(top)
    {
        l.shares.set_share(left, 300.0);
        l.shares.set_share(centre, 972.0);
        l.shares.set_share(right, 320.0);
    }
    let root = match kind {
        Kind::Video => {
            let timeline = tiles.insert_pane(Pane::Timeline);
            let bottom = tiles.insert_tab_tile(vec![timeline]);
            let root = tiles.insert_vertical_tile(vec![top, bottom]);
            if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(l))) =
                tiles.get_mut(root)
            {
                l.shares.set_share(top, 562.0);
                l.shares.set_share(bottom, 344.0);
            }
            root
        }
        Kind::Graph => top,
    };
    Tree::new(egui::Id::unique("studio-page"), root, tiles)
}

struct Behaviour<'a> {
    app: &'a mut App,
    engine: &'a mut Engine,
    commands: &'a mut Vec<AppCommand>,
    root: Option<TileId>,
}

impl Behaviour<'_> {
    fn empty(ui: &mut Ui, text: &str) {
        let rect = ui.max_rect();
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            sans(12.0),
            t::TEXT3,
        );
    }
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
                };
                viewer::viewer(ui, &state, self.commands);
            }
            // TODO: the media pool, node list, inspector and timeline.
            Pane::Media => Self::empty(ui, "No media yet"),
            Pane::Nodes => Self::empty(ui, "No nodes yet"),
            Pane::Inspector => Self::empty(ui, "Nothing selected"),
            Pane::Timeline => Self::empty(ui, "The timeline comes next"),
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
        _tiles: &Tiles<Pane>,
        ui: &mut Ui,
        _tile: TileId,
        _tabs: &egui_tiles::Tabs,
    ) {
        ui.add_space(4.0);
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
