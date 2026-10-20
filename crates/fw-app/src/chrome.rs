//! The window's own chrome: the title bar with its menus and window
//! controls, the workspace bar, and resizing from the edges (design §10).

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use winit::window::ResizeDirection;

use crate::icons;
use crate::style::{medium, sans, semibold, tokens as t};

pub const TITLE_BAR_HEIGHT: f32 = 34.0;
pub const WORKSPACE_BAR_HEIGHT: f32 = 44.0;
/// How far in from the window's edge a press resizes it.
const RESIZE_BORDER: f32 = 5.0;

/// What the UI asks of the OS window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    /// Move the window with the pointer, through the compositor.
    Drag,
    Resize(ResizeDirection),
    Minimise,
    ToggleMaximise,
    Close,
}

/// What the menus, shortcuts and controls ask of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    Seek(fw_core::Time),
    Save,
    Quit,
    Undo,
    Redo,
    /// Removes the selected clip or node.
    Delete,
    StepBack,
    StepForward,
    GoToStart,
    GoToEnd,
    ResetLayout,
}

/// What the title bar shows.
pub struct TitleBar<'a> {
    pub project: &'a str,
    pub modified: bool,
    pub undo: Option<&'a str>,
    pub redo: Option<&'a str>,
    pub maximised: bool,
    /// Something is selected that Delete would remove.
    pub selection: bool,
}

pub fn title_bar(
    ui: &mut Ui,
    bar: &TitleBar,
    actions: &mut Vec<WindowAction>,
    commands: &mut Vec<AppCommand>,
) {
    let full = ui.max_rect();
    // Behind everything else in the bar: dragging moves the window.
    let background = ui.interact(
        full,
        ui.scope_id().with("title-drag"),
        Sense::click_and_drag(),
    );
    if background.drag_started() {
        actions.push(WindowAction::Drag);
    }
    if background.double_clicked() {
        actions.push(WindowAction::ToggleMaximise);
    }

    ui.horizontal_centered(|ui| {
        ui.add_space(12.0);
        logo(ui);
        ui.add_space(4.0);
        ui.label(egui::RichText::new("framewright").font(semibold(12.0)));
        ui.add_space(14.0);
        menus(ui, bar, commands);

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            window_controls(ui, bar.maximised, actions);
            ui.add_space(10.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            export_button(ui);
            ui.add_space(6.0);
            let redo = icon_button(ui, icons::REDO, bar.redo.is_some()).on_hover_text(
                bar.redo
                    .map_or("Nothing to redo".into(), |l| format!("Redo {l}")),
            );
            if redo.clicked() {
                commands.push(AppCommand::Redo);
            }
            let undo = icon_button(ui, icons::UNDO, bar.undo.is_some()).on_hover_text(
                bar.undo
                    .map_or("Nothing to undo".into(), |l| format!("Undo {l}")),
            );
            if undo.clicked() {
                commands.push(AppCommand::Undo);
            }
        });
    });

    // The project's name and save state, centred on the window.
    let painter = ui.painter();
    let name = painter.layout_no_wrap(bar.project.to_owned(), medium(12.0), t::TEXT);
    let state = if bar.modified { "Edited" } else { "Saved" };
    let state = painter.layout_no_wrap(state.into(), sans(12.0), t::TEXT2);
    let width = name.size().x + 16.0 + state.size().x + if bar.modified { 0.0 } else { 20.0 };
    let mut x = full.center().x - width / 2.0;
    let y = full.center().y;
    let name_width = name.size().x;
    painter.galley(pos2(x, y - name.size().y / 2.0), name, t::TEXT);
    x += name_width + 5.0;
    painter.text(pos2(x, y), Align2::LEFT_CENTER, "·", sans(12.0), t::TEXT3);
    x += 11.0;
    let state_width = state.size().x;
    painter.galley(pos2(x, y - state.size().y / 2.0), state, t::TEXT2);
    if !bar.modified {
        let check = Rect::from_min_size(pos2(x + state_width + 5.0, y - 7.0), vec2(14.0, 14.0));
        icons::paint(painter, check, icons::CHECK, t::SAVED, 1.6, false);
    }
    painter.hline(
        full.x_range(),
        full.bottom() - 0.5,
        Stroke::new(1.0, t::LINE),
    );
}

fn logo(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    let s = 18.0 / 24.0;
    let at = |x: f32, y: f32| pos2(r.left() + x * s, r.top() + y * s);
    let f = [
        at(4.0, 4.0),
        at(14.0, 4.0),
        at(14.0, 10.0),
        at(10.0, 10.0),
        at(10.0, 20.0),
        at(4.0, 20.0),
    ];
    ui.painter().add(egui::Shape::convex_polygon(
        f.to_vec(),
        t::ACCENT,
        Stroke::NONE,
    ));
    ui.painter().rect_filled(
        Rect::from_min_max(at(12.0, 12.0), at(20.0, 20.0)),
        CornerRadius::ZERO,
        Color32::from_rgb(0x5a, 0x5a, 0x62),
    );
}

fn menus(ui: &mut Ui, bar: &TitleBar, commands: &mut Vec<AppCommand>) {
    ui.spacing_mut().item_spacing.x = 2.0;
    // Menu titles are flat text, without a border or fill until hovered.
    let w = &mut ui.visuals_mut().widgets;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.hovered.bg_stroke = Stroke::NONE;
    w.open.bg_stroke = Stroke::NONE;
    let title = |text: &str| egui::RichText::new(text).color(t::TEXT2);
    let mut item = |ui: &mut Ui, label: &str, shortcut: &str, enabled: bool, command| {
        let button = egui::Button::new(label).shortcut_text(shortcut);
        if ui.add_enabled(enabled, button).clicked() {
            commands.push(command);
        }
    };
    ui.menu_button(title("File"), |ui| {
        ui.set_min_width(220.0);
        item(ui, "Save", "Ctrl+S", bar.modified, AppCommand::Save);
        ui.separator();
        item(ui, "Quit", "Ctrl+Q", true, AppCommand::Quit);
    });
    ui.menu_button(title("Edit"), |ui| {
        ui.set_min_width(220.0);
        let undo = bar.undo.map_or("Undo".into(), |l| format!("Undo {l}"));
        item(ui, &undo, "Ctrl+Z", bar.undo.is_some(), AppCommand::Undo);
        let redo = bar.redo.map_or("Redo".into(), |l| format!("Redo {l}"));
        item(
            ui,
            &redo,
            "Ctrl+Shift+Z",
            bar.redo.is_some(),
            AppCommand::Redo,
        );
        ui.separator();
        item(ui, "Delete", "Del", bar.selection, AppCommand::Delete);
    });
    ui.menu_button(title("Playback"), |ui| {
        ui.set_min_width(220.0);
        item(ui, "Previous Frame", "←", true, AppCommand::StepBack);
        item(ui, "Next Frame", "→", true, AppCommand::StepForward);
        ui.separator();
        item(ui, "Go to Start", "Home", true, AppCommand::GoToStart);
        item(ui, "Go to End", "End", true, AppCommand::GoToEnd);
    });
    ui.menu_button(title("View"), |ui| {
        ui.set_min_width(220.0);
        item(ui, "Reset Layout", "", true, AppCommand::ResetLayout);
    });
}

/// The menus' shortcuts, when no text field has the keyboard.
pub fn shortcuts(ctx: &egui::Context, commands: &mut Vec<AppCommand>) {
    use egui::{Key, KeyboardShortcut, Modifiers};
    let command = Modifiers::COMMAND;
    let shortcuts = [
        (KeyboardShortcut::new(command, Key::S), AppCommand::Save),
        (KeyboardShortcut::new(command, Key::Q), AppCommand::Quit),
        (
            KeyboardShortcut::new(command | Modifiers::SHIFT, Key::Z),
            AppCommand::Redo,
        ),
        (KeyboardShortcut::new(command, Key::Z), AppCommand::Undo),
        (KeyboardShortcut::new(command, Key::Y), AppCommand::Redo),
    ];
    let plain = [
        (Key::ArrowLeft, AppCommand::StepBack),
        (Key::ArrowRight, AppCommand::StepForward),
        (Key::Home, AppCommand::GoToStart),
        (Key::End, AppCommand::GoToEnd),
        (Key::Delete, AppCommand::Delete),
        (Key::Backspace, AppCommand::Delete),
    ];
    let typing = ctx.egui_wants_keyboard_input();
    ctx.input_mut(|input| {
        for (shortcut, c) in shortcuts {
            if input.consume_shortcut(&shortcut) {
                commands.push(c);
            }
        }
        if !typing {
            for (key, c) in plain {
                if input.consume_key(Modifiers::NONE, key) {
                    commands.push(c);
                }
            }
        }
    });
}

fn window_controls(ui: &mut Ui, maximised: bool, actions: &mut Vec<WindowAction>) {
    // Right to left: close, maximise, minimise.
    let controls = [
        (icons::CLOSE, "Close", WindowAction::Close),
        (
            if maximised {
                icons::RESTORE
            } else {
                icons::MAXIMISE
            },
            if maximised { "Restore" } else { "Maximise" },
            WindowAction::ToggleMaximise,
        ),
        (icons::MINIMISE, "Minimise", WindowAction::Minimise),
    ];
    for (icon, label, action) in controls {
        let (rect, response) = ui.allocate_exact_size(vec2(44.0, TITLE_BAR_HEIGHT), Sense::click());
        let close = action == WindowAction::Close;
        if response.hovered() {
            let fill = if close { t::CLOSE_HOVER } else { t::RAISED2 };
            ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
        }
        let colour = if response.hovered() {
            t::TEXT_BRIGHT
        } else {
            t::TEXT2
        };
        let icon_rect = Rect::from_center_size(rect.center(), vec2(14.0, 14.0));
        icons::paint(ui.painter(), icon_rect, icon, colour, 1.4, false);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
        if response.clicked() {
            actions.push(action);
        }
    }
}

fn export_button(ui: &mut Ui) {
    // TODO: enable once the render queue exists.
    let text = ui
        .painter()
        .layout_no_wrap("Export".into(), semibold(12.0), t::ACCENT_INK);
    let (rect, response) = ui.allocate_exact_size(vec2(text.size().x + 24.0, 24.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(3), t::ACCENT.gamma_multiply(0.45));
    let at = pos2(rect.left() + 12.0, rect.center().y - text.size().y / 2.0);
    ui.painter().galley(at, text, t::ACCENT_INK);
    response.on_hover_text("Export isn't built yet");
}

/// A 26-point square button showing an icon.
pub fn icon_button(ui: &mut Ui, icon: &str, enabled: bool) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(26.0, 26.0), sense);
    if enabled && response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(3), t::RAISED2);
    }
    let colour = match (enabled, response.hovered()) {
        (false, _) => t::TEXT_DISABLED,
        (true, true) => t::TEXT_BRIGHT,
        (true, false) => t::TEXT2,
    };
    let icon_rect = Rect::from_center_size(rect.center(), vec2(15.0, 15.0));
    icons::paint(ui.painter(), icon_rect, icon, colour, 1.6, false);
    response
}

/// A workspace in the bar along the bottom.
pub struct Workspace {
    pub name: &'static str,
    pub icon: &'static str,
    pub built: bool,
}

/// The workspace bar: what's open on the left, the workspaces in the
/// middle, the engine's state on the right.
pub fn workspace_bar(
    ui: &mut Ui,
    workspaces: &[Workspace],
    active: usize,
    left: &str,
    right: &str,
) {
    let r = ui.max_rect();
    let painter = ui.painter();
    painter.hline(r.x_range(), r.top() + 0.5, Stroke::new(1.0, t::LINE));
    painter.text(
        pos2(r.left() + 14.0, r.center().y),
        Align2::LEFT_CENTER,
        left,
        sans(11.0),
        t::TEXT3,
    );
    painter.text(
        pos2(r.right() - 14.0, r.center().y),
        Align2::RIGHT_CENTER,
        right,
        crate::style::mono(11.0),
        t::TEXT3,
    );
    let widths: Vec<f32> = workspaces
        .iter()
        .map(|w| {
            let label = painter.layout_no_wrap(w.name.into(), sans(11.0), Color32::WHITE);
            label.size().x.max(18.0) + 28.0
        })
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 2.0 * (workspaces.len() as f32 - 1.0);
    let mut x = r.center().x - total / 2.0;
    for (i, workspace) in workspaces.iter().enumerate() {
        let b = Rect::from_min_size(pos2(x, r.top()), vec2(widths[i], WORKSPACE_BAR_HEIGHT));
        let response = ui.interact(b, ui.scope_id().with(("workspace", i)), Sense::hover());
        let on = i == active;
        let text = match (on, workspace.built, response.hovered()) {
            (true, _, _) => t::TEXT_BRIGHT,
            (false, false, _) => t::TEXT_DISABLED,
            (false, true, true) => t::TEXT_BRIGHT,
            (false, true, false) => t::TEXT3,
        };
        let icon = if on { t::ACCENT } else { text };
        let icon_rect =
            Rect::from_center_size(pos2(b.center().x, b.top() + 15.0), vec2(18.0, 18.0));
        icons::paint(ui.painter(), icon_rect, workspace.icon, icon, 1.6, false);
        ui.painter().text(
            pos2(b.center().x, b.top() + 31.0),
            Align2::CENTER_CENTER,
            workspace.name,
            sans(11.0),
            text,
        );
        if on {
            let underline =
                Rect::from_center_size(pos2(b.center().x, b.top() + 41.0), vec2(18.0, 2.0));
            ui.painter()
                .rect_filled(underline, CornerRadius::same(1), t::ACCENT);
        }
        if !workspace.built {
            response.on_hover_text(format!("The {} workspace isn't built yet", workspace.name));
        }
        x += widths[i] + 2.0;
    }
}

/// Resizing from the window's edges: shows the cursor near an edge, and
/// returns the edge pressed. Call before anything else takes the press.
pub fn resize_edges(ctx: &egui::Context) -> Option<ResizeDirection> {
    let rect = ctx.content_rect();
    let (pos, pressed) = ctx.input(|i| (i.pointer.hover_pos(), i.pointer.primary_pressed()));
    let pos = pos?;
    let near = |a: f32, b: f32| (a - b).abs() <= RESIZE_BORDER;
    let (w, e) = (near(pos.x, rect.left()), near(pos.x, rect.right()));
    let (n, s) = (near(pos.y, rect.top()), near(pos.y, rect.bottom()));
    use ResizeDirection as D;
    let direction = match (n, s, w, e) {
        (true, _, true, _) => D::NorthWest,
        (true, _, _, true) => D::NorthEast,
        (_, true, true, _) => D::SouthWest,
        (_, true, _, true) => D::SouthEast,
        (true, ..) => D::North,
        (_, true, ..) => D::South,
        (.., true, _) => D::West,
        (.., true) => D::East,
        _ => return None,
    };
    let cursor = match direction {
        D::North | D::South => egui::CursorIcon::ResizeVertical,
        D::East | D::West => egui::CursorIcon::ResizeHorizontal,
        D::NorthWest | D::SouthEast => egui::CursorIcon::ResizeNwSe,
        D::NorthEast | D::SouthWest => egui::CursorIcon::ResizeNeSw,
    };
    ctx.set_cursor_icon(cursor);
    pressed.then_some(direction)
}
