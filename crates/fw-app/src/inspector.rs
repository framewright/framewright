//! The inspector: the selected node's parameters, laid out from its type's
//! schema (design §4.1), edited through the engine's requests.

use std::collections::HashSet;

use egui::{Align2, Rect, Sense, Stroke, Ui, pos2, vec2};
use fw_core::NodeId;
use fw_doc::{Command, Param};
use fw_engine::Engine;
use fw_plugin::{ParamDesc, ParamKind, ParamValue, Unit};

use crate::app::Edit;
use crate::controls::{self, Change, Gestures, Number, Typing};
use crate::icons;
use crate::style::{medium, mono, sans, tokens as t};
use crate::viewer::timecode;

#[derive(Default)]
pub struct Inspector {
    typing: Typing,
    /// Sections the user closed, by title.
    closed: HashSet<String>,
}

pub fn inspector(
    ui: &mut Ui,
    state: &mut Inspector,
    gestures: &mut Gestures,
    engine: &Engine,
    selected: Option<NodeId>,
    nothing_selected: &str,
    edits: &mut Vec<Edit>,
) {
    let doc = engine.document();
    let Some((id, node)) = selected.and_then(|id| Some((id, doc.node(id)?))) else {
        empty(ui, nothing_selected);
        return;
    };
    let Some(known) = node.as_known() else {
        empty(
            ui,
            &format!(
                "{} isn't installed, so this node can't be edited",
                node.type_id()
            ),
        );
        return;
    };
    let Some(desc) = engine.registry().desc(&known.type_id) else {
        empty(ui, &format!("{} isn't installed", known.type_id));
        return;
    };

    // The header: name, and what and where it is.
    let meta = match doc
        .timeline(doc.root())
        .and_then(|t| Some((t, t.clips.get(&id)?)))
    {
        Some((timeline, placement)) => {
            let track = timeline
                .track(placement.track)
                .map_or("", |t| t.name.as_str());
            let rate = timeline.settings.rate;
            // Frames that start within the clip.
            let length = (placement.duration.as_f64() * rate.as_f64()).ceil() as i64;
            format!("{} · {track} · {}", desc.name, timecode(length, rate))
        }
        None => format!("{} · {}", desc.name, desc.category),
    };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.text(
        pos2(rect.left() + 12.0, rect.top() + 16.0),
        Align2::LEFT_CENTER,
        &known.name,
        medium(12.0),
        t::TEXT_BRIGHT,
    );
    painter.text(
        pos2(rect.left() + 12.0, rect.bottom() - 14.0),
        Align2::LEFT_CENTER,
        meta,
        sans(11.0),
        t::TEXT3,
    );
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, t::LINE),
    );

    // Sections in the order their first parameter appears.
    let mut sections: Vec<(&str, Vec<&ParamDesc>)> = vec![];
    for param in &desc.params {
        let title = param.section.as_deref().unwrap_or(&desc.name);
        match sections.iter_mut().find(|(s, _)| *s == title) {
            Some((_, params)) => params.push(param),
            None => sections.push((title, vec![param])),
        }
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let base = ui.scope_id().with(("inspector", id));
            for (title, params) in sections {
                let open = !state.closed.contains(title);
                if section_header(ui, title, open) {
                    if open {
                        state.closed.insert(title.to_owned());
                    } else {
                        state.closed.remove(title);
                    }
                }
                if open {
                    for param in params {
                        let current = known.params.get(&param.id);
                        let row = base.with(&param.id);
                        if let Some(change) = param_row(ui, row, param, current, state, gestures) {
                            edits.push(edit(id, param, change));
                        }
                    }
                    ui.add_space(6.0);
                }
                let y = ui.cursor().top();
                ui.painter()
                    .hline(ui.max_rect().x_range(), y - 0.5, Stroke::new(1.0, t::LINE));
            }
        });
}

/// What the user did to a parameter, before it becomes a command.
enum ParamChange {
    Row(Change),
    Value(ParamValue, Option<u64>),
}

impl From<Change> for ParamChange {
    fn from(change: Change) -> ParamChange {
        ParamChange::Row(change)
    }
}

fn edit(node: NodeId, param: &ParamDesc, change: ParamChange) -> Edit {
    let (label, value, gesture) = match change {
        ParamChange::Row(Change::Reset) => (format!("Reset {}", param.label), None, None),
        ParamChange::Row(Change::Set { value, gesture }) => {
            (param.label.clone(), Some(ParamValue::Float(value)), gesture)
        }
        ParamChange::Value(value, gesture) => (param.label.clone(), Some(value), gesture),
    };
    Edit {
        label,
        commands: vec![Command::SetParam {
            node,
            param: param.id.clone(),
            value: value.map(Param::Constant),
        }],
        gesture,
    }
}

fn param_row(
    ui: &mut Ui,
    id: egui::Id,
    param: &ParamDesc,
    current: Option<&Param>,
    state: &mut Inspector,
    gestures: &mut Gestures,
) -> Option<ParamChange> {
    // TODO: show and edit keyframed values; for now they're shown read-only.
    let (value, animated) = match current {
        Some(Param::Constant(v)) => (v.clone(), false),
        Some(Param::Keyframed(k)) => (
            k.keyframes
                .first()
                .map_or(param.default.clone(), |k| k.v.clone()),
            true,
        ),
        None => (param.default.clone(), false),
    };
    let label = if animated {
        format!("{} (animated)", param.label)
    } else {
        param.label.clone()
    };
    match (&param.kind, value) {
        (ParamKind::Float { unit }, ParamValue::Float(v)) => {
            let number = Number {
                unit: *unit,
                slider: param.soft_range.or(param.hard_range),
                limits: param.hard_range,
            };
            controls::number_row(
                ui,
                id,
                &label,
                v,
                number,
                !animated,
                &mut state.typing,
                gestures,
            )
            .map(ParamChange::from)
        }
        (ParamKind::Int, ParamValue::Int(v)) => {
            let mut v = v;
            let mut changed = None;
            let reset = controls::custom_row(ui, id, &label, |ui| {
                let response = ui.add_enabled(!animated, egui::DragValue::new(&mut v));
                if response.changed() {
                    changed = Some(v);
                }
            });
            reset
                .map(ParamChange::from)
                .or(changed.map(|v| ParamChange::Value(ParamValue::Int(v), None)))
        }
        (ParamKind::Bool, ParamValue::Bool(b)) => {
            let mut b = b;
            let mut changed = false;
            let reset = controls::custom_row(ui, id, &label, |ui| {
                changed = ui
                    .add_enabled(!animated, egui::Checkbox::without_text(&mut b))
                    .changed();
            });
            reset
                .map(ParamChange::from)
                .or(changed.then_some(ParamChange::Value(ParamValue::Bool(b), None)))
        }
        (ParamKind::Choice { options }, ParamValue::Text(chosen)) => {
            let mut picked = None;
            let reset = controls::custom_row(ui, id, &label, |ui| {
                let shown = options
                    .iter()
                    .find(|o| o.id == chosen)
                    .map_or(chosen.as_str(), |o| o.label.as_str());
                egui::ComboBox::from_id_salt(id.with("choice"))
                    .selected_text(egui::RichText::new(shown).font(sans(12.0)))
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for option in options {
                            if ui
                                .selectable_label(option.id == chosen, &option.label)
                                .clicked()
                                && option.id != chosen
                            {
                                picked = Some(option.id.clone());
                            }
                        }
                    });
            });
            reset
                .map(ParamChange::from)
                .or(picked.map(|p| ParamChange::Value(ParamValue::Text(p), None)))
        }
        (ParamKind::Text, ParamValue::Text(text)) => {
            let mut text = text;
            let mut changed = None;
            let reset = controls::custom_row(ui, id, &label, |ui| {
                let field = egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY);
                let response = ui.add_enabled(!animated, field);
                let widget = response.id;
                if response.changed() {
                    // Typing in one visit to the field is one undo step.
                    changed = Some(gestures.drag(widget));
                }
                if response.lost_focus() {
                    gestures.end(widget);
                }
            });
            reset
                .map(ParamChange::from)
                .or(changed
                    .map(|gesture| ParamChange::Value(ParamValue::Text(text), Some(gesture))))
        }
        (ParamKind::Vec2 { .. }, ParamValue::Vec2(v)) => {
            let mut v = v;
            let mut changed = None;
            let reset = controls::custom_row(ui, id, &label, |ui| {
                let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                for (axis, value) in ["x", "y"].into_iter().zip(&mut v) {
                    let field = egui::DragValue::new(value)
                        .speed(0.01)
                        .prefix(format!("{axis} "));
                    let response = ui.add_enabled_ui(!animated, |ui| {
                        ui.add_sized([width, ui.available_height()], field)
                    });
                    let response = response.inner;
                    if response.changed() {
                        // One drag of a field is one undo step.
                        changed = Some(gestures.drag(response.id));
                    } else if !response.dragged() && !response.has_focus() {
                        gestures.end(response.id);
                    }
                }
            });
            reset
                .map(ParamChange::from)
                .or(changed.map(|gesture| ParamChange::Value(ParamValue::Vec2(v), Some(gesture))))
        }
        // TODO: a picker in the Studio style that keeps values above 1.0 and
        // shows which colour space they're in.
        (ParamKind::Color, ParamValue::Color(color)) => {
            let mut rgba = color.rgba.map(|v| v as f32);
            let mut changed = None;
            let reset = controls::custom_row(ui, id, &label, |ui| {
                let response = ui
                    .add_enabled_ui(!animated, |ui| {
                        ui.color_edit_button_rgba_unmultiplied(&mut rgba)
                    })
                    .inner;
                let widget = response.id;
                if response.changed() {
                    changed = Some(gestures.drag(widget));
                } else if !ui.input(|i| i.pointer.any_down()) {
                    gestures.end(widget);
                }
            });
            reset.map(ParamChange::from).or(changed.map(|gesture| {
                let color = fw_plugin::Color {
                    rgba: rgba.map(f64::from),
                    ..color
                };
                ParamChange::Value(ParamValue::Color(color), Some(gesture))
            }))
        }
        // TODO: relinking media.
        (ParamKind::Media, ParamValue::Media(media)) => {
            let name = std::path::Path::new(&media.path)
                .file_name()
                .map_or(media.path.clone(), |n| n.to_string_lossy().into_owned());
            controls::custom_row(ui, id, &label, |ui| {
                let shown = if name.is_empty() { "None".into() } else { name };
                ui.label(egui::RichText::new(shown).font(mono(11.0)).color(t::TEXT))
                    .on_hover_text(&media.path);
            })
            .map(ParamChange::from)
        }
        (_, value) => controls::custom_row(ui, id, &label, |ui| {
            ui.label(
                egui::RichText::new(describe(&value))
                    .font(mono(11.0))
                    .color(t::TEXT3),
            );
        })
        .map(ParamChange::from),
    }
}

fn describe(value: &ParamValue) -> String {
    match value {
        ParamValue::Vec2([x, y]) => format!("{x:.3}, {y:.3}"),
        ParamValue::Color(c) => {
            let [r, g, b, a] = c.rgba;
            format!("{r:.3} {g:.3} {b:.3} {a:.3}")
        }
        ParamValue::Float(v) => Number {
            unit: Unit::None,
            slider: None,
            limits: None,
        }
        .show(*v),
        ParamValue::Int(v) => v.to_string(),
        ParamValue::Bool(b) => b.to_string(),
        ParamValue::Text(s) => s.clone(),
        ParamValue::Media(m) => m.path.clone(),
    }
}

/// A section's title row, with its chevron. Returns true when clicked.
fn section_header(ui: &mut Ui, title: &str, open: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::DisclosureTriangle, true, title));
    let painter = ui.painter_at(rect);
    let chevron = Rect::from_min_size(
        pos2(rect.left() + 10.0, rect.center().y - 6.0),
        vec2(12.0, 12.0),
    );
    let icon = if open { icons::CHEV_D } else { icons::CHEV_R };
    icons::paint(&painter, chevron, icon, t::TEXT3, 1.6, false);
    painter.text(
        pos2(chevron.right() + 6.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        medium(12.0),
        t::TEXT,
    );
    response.clicked()
}

fn empty(ui: &mut Ui, text: &str) {
    let rect = ui.max_rect();
    let galley = ui.painter().layout(
        text.to_owned(),
        sans(12.0),
        t::TEXT3,
        (rect.width() - 32.0).max(80.0),
    );
    let at = rect.center() - galley.size() / 2.0;
    ui.painter().galley(at, galley, t::TEXT3);
}
