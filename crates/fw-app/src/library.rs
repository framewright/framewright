//! The add-node list: the registry's node types by category, searchable,
//! opened in the node editor with Tab or a right-click.

use egui::{Align2, CornerRadius, Key, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use fw_plugin::{INPUT_TYPE, NodeTypeDesc, Registry};

use crate::style::{sans, tokens as t};

const WIDTH: f32 = 240.0;
const ROW_HEIGHT: f32 = 26.0;
const CAPTION_HEIGHT: f32 = 22.0;
const LIST_HEIGHT: f32 = 300.0;

pub struct Library {
    /// Where it opened, on screen.
    screen: Pos2,
    /// Where a node added from it lands, in editor units.
    pub at: Pos2,
    query: String,
    /// Whether the graph's container supplies an image, so Input means something.
    supplied: bool,
    /// The type Enter adds, as an index into the matches.
    highlight: usize,
    focused: bool,
}

pub enum Outcome {
    Open,
    Close,
    /// Add a node of this type.
    Add(String),
}

impl Library {
    pub fn new(screen: Pos2, at: Pos2, supplied: bool) -> Library {
        Library {
            screen,
            at,
            query: String::new(),
            supplied,
            highlight: 0,
            focused: false,
        }
    }
}

/// The types a user can add to a graph, by category then name.
fn addable<'a>(registry: &'a Registry, query: &str, supplied: bool) -> Vec<&'a NodeTypeDesc> {
    let query = query.trim().to_lowercase();
    let mut types: Vec<&NodeTypeDesc> = registry
        .iter()
        .map(|ty| &ty.desc)
        // Containers are made by their own commands, and clips (Graph
        // containers too) belong on timelines.
        .filter(|d| d.container.is_none())
        .filter(|d| supplied || d.id != INPUT_TYPE)
        .filter(|d| {
            query.is_empty()
                || d.name.to_lowercase().contains(&query)
                || d.category.to_lowercase().contains(&query)
        })
        .collect();
    types.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
    types
}

pub fn show(ui: &mut Ui, library: &mut Library, registry: &Registry) -> Outcome {
    let types = addable(registry, &library.query, library.supplied);
    let (escape, enter, down, up) = ui.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, Key::Escape),
            i.key_pressed(Key::Enter),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
        )
    });
    if escape {
        return Outcome::Close;
    }
    if down {
        library.highlight += 1;
    }
    if up {
        library.highlight = library.highlight.saturating_sub(1);
    }
    library.highlight = library.highlight.min(types.len().saturating_sub(1));
    if enter && let Some(desc) = types.get(library.highlight) {
        return Outcome::Add(desc.id.clone());
    }

    let mut outcome = Outcome::Open;
    let area = egui::Area::new(ui.scope_id().with("node-library"))
        .order(egui::Order::Foreground)
        .fixed_pos(library.screen)
        .constrain(true)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(t::MENU_BG)
                .stroke(Stroke::new(1.0, t::MENU_BORDER))
                .corner_radius(CornerRadius::same(5))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 14],
                    blur: 40,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(150),
                })
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    ui.set_width(WIDTH);
                    let field = egui::TextEdit::singleline(&mut library.query)
                        .hint_text("Add a node…")
                        .font(sans(12.0))
                        .desired_width(f32::INFINITY);
                    let response = ui.add(field);
                    if !library.focused {
                        response.request_focus();
                        library.focused = true;
                    }
                    if response.changed() {
                        library.highlight = 0;
                    }
                    ui.add_space(4.0);
                    if types.is_empty() {
                        ui.label(
                            egui::RichText::new("No node types match")
                                .font(sans(12.0))
                                .color(t::TEXT3),
                        );
                        return;
                    }
                    egui::ScrollArea::vertical()
                        .max_height(LIST_HEIGHT)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            let mut category = None;
                            for (index, desc) in types.iter().enumerate() {
                                if category != Some(&desc.category) {
                                    category = Some(&desc.category);
                                    caption(ui, &desc.category);
                                }
                                if row(ui, desc, index == library.highlight) {
                                    outcome = Outcome::Add(desc.id.clone());
                                }
                            }
                        });
                });
        });
    let pressed_outside = ui.input(|i| i.pointer.any_pressed())
        && !area.response.contains_pointer()
        && !area.response.rect.contains(
            ui.input(|i| i.pointer.interact_pos())
                .unwrap_or(library.screen),
        );
    if pressed_outside && matches!(outcome, Outcome::Open) {
        return Outcome::Close;
    }
    outcome
}

fn caption(ui: &mut Ui, text: &str) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), CAPTION_HEIGHT), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 8.0, rect.bottom() - 6.0),
        Align2::LEFT_BOTTOM,
        text.to_uppercase(),
        sans(10.0),
        t::TEXT3,
    );
}

/// One type's row. Returns true when clicked.
fn row(ui: &mut Ui, desc: &NodeTypeDesc, highlighted: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Add {}", desc.name))
    });
    let painter = ui.painter_at(rect);
    if highlighted || response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(3), t::MENU_ON);
    }
    let dot = Rect::from_center_size(pos2(rect.left() + 12.0, rect.center().y), vec2(6.0, 6.0));
    painter.rect_filled(dot, CornerRadius::same(3), category_color(&desc.category));
    painter.text(
        pos2(rect.left() + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        &desc.name,
        sans(12.0),
        if highlighted { t::TEXT_BRIGHT } else { t::TEXT },
    );
    response.clicked()
}

/// The color that marks a category, in the list and on nodes.
pub fn category_color(category: &str) -> egui::Color32 {
    match category {
        "Sources" => t::NODE_SOURCE,
        "Color" => t::NODE_COLOR,
        "Outputs" => t::NODE_OUTPUT,
        "Containers" => t::NODE_CONTAINER,
        _ => t::NODE_FILTER,
    }
}
