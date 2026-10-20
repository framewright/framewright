//! The timeline panel: tracks and clips from the document, a ruler, and
//! the playhead. Clicking seeks and selects; dragging a clip moves it.

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use fw_core::{NodeId, Time};
use fw_doc::{Command, Document, Placement, Timeline, TrackKind};
use fw_engine::Engine;

use crate::app::Edit;
use crate::chrome::AppCommand;
use crate::controls::Gestures;
use crate::icons;
use crate::style::{medium, mono, mono_medium, sans, tokens as t};
use crate::viewer::{frame_start, timecode};

const HEADER_WIDTH: f32 = 116.0;
const RULER_HEIGHT: f32 = 26.0;
const VIDEO_HEIGHT: f32 = 50.0;
const AUDIO_HEIGHT: f32 = 44.0;
/// Labelled ticks on the ruler are at least this far apart.
const LABEL_SPACING: f32 = 90.0;

pub struct TimelineView {
    /// Zoom.
    points_per_second: f32,
    /// The time at the lanes' left edge, in seconds.
    scroll: f64,
    /// Zoomed to fit the edit, which happens when first shown.
    fitted: bool,
    drag: Option<ClipDrag>,
}

/// A clip being moved.
struct ClipDrag {
    node: NodeId,
    /// From the clip's start to where it was grabbed, in seconds.
    grab: f64,
}

impl Default for TimelineView {
    fn default() -> TimelineView {
        TimelineView {
            points_per_second: 40.0,
            scroll: 0.0,
            fitted: false,
            drag: None,
        }
    }
}

impl TimelineView {
    /// Zooms so the whole edit fits on the next frame.
    pub fn fit(&mut self) {
        self.fitted = false;
    }

    fn x(&self, seconds: f64, lanes: Rect) -> f32 {
        lanes.left() + ((seconds - self.scroll) * self.points_per_second as f64) as f32
    }

    fn seconds(&self, x: f32, lanes: Rect) -> f64 {
        self.scroll + ((x - lanes.left()) / self.points_per_second) as f64
    }
}

/// A track's row on screen.
struct Row<'a> {
    track: &'a fw_doc::Track,
    rect: Rect,
}

#[allow(clippy::too_many_arguments)]
pub fn timeline(
    ui: &mut Ui,
    view: &mut TimelineView,
    gestures: &mut Gestures,
    engine: &Engine,
    selected: &mut Option<NodeId>,
    edits: &mut Vec<Edit>,
    commands: &mut Vec<AppCommand>,
) {
    let doc = engine.document();
    let Some(timeline) = doc.timeline(doc.root()) else {
        return;
    };
    let rate = timeline.settings.rate;
    let full = ui.max_rect();
    let ruler = Rect::from_min_max(
        pos2(full.left() + HEADER_WIDTH, full.top()),
        pos2(full.right(), full.top() + RULER_HEIGHT),
    );
    let lanes = Rect::from_min_max(pos2(ruler.left(), ruler.bottom()), full.right_bottom());
    let duration = engine.duration().as_f64();
    if !view.fitted && lanes.width() > 0.0 {
        view.points_per_second =
            (lanes.width() / (duration.max(10.0) * 1.05) as f32).clamp(0.5, 2000.0);
        view.scroll = 0.0;
        view.fitted = true;
    }
    zoom_and_scroll(ui, view, ruler.union(lanes), lanes);

    // Rows top to bottom: video from the highest track down, then audio.
    let mut rows = vec![];
    let mut y = lanes.top();
    let video = timeline
        .tracks
        .iter()
        .rev()
        .filter(|t| t.kind == TrackKind::Video);
    let audio = timeline
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio);
    for track in video.chain(audio) {
        let height = if track.kind == TrackKind::Video {
            VIDEO_HEIGHT
        } else {
            AUDIO_HEIGHT
        };
        let rect = Rect::from_min_size(pos2(full.left(), y), vec2(full.width(), height));
        rows.push(Row { track, rect });
        y += height;
    }

    // Behind the clips: clicking an empty lane seeks and deselects.
    let background = ui.interact(lanes, ui.scope_id().with("lanes"), Sense::click_and_drag());
    if (background.clicked() || background.dragged())
        && let Some(pos) = background.interact_pointer_pos()
    {
        commands.push(AppCommand::Seek(frame_start(
            view.seconds(pos.x, lanes),
            rate,
        )));
        if background.clicked() {
            *selected = None;
        }
    }

    let painter = ui.painter_at(full);
    for row in &rows {
        let lane = Rect::from_min_max(pos2(lanes.left(), row.rect.top()), row.rect.max);
        let fill = if row.track.kind == TrackKind::Video {
            t::LANE_VIDEO
        } else {
            t::LANE_AUDIO
        };
        painter.rect_filled(lane, CornerRadius::ZERO, fill);
        painter.hline(
            lane.x_range(),
            lane.bottom() - 0.5,
            Stroke::new(1.0, t::LANE_LINE),
        );
    }

    clips(
        ui, view, gestures, doc, timeline, &rows, lanes, selected, edits, commands,
    );

    // Track headers, over anything scrolled under them.
    let headers = Rect::from_min_max(
        pos2(full.left(), lanes.top()),
        pos2(lanes.left(), full.bottom()),
    );
    painter.rect_filled(headers, CornerRadius::ZERO, t::PANEL);
    for row in &rows {
        let header = Rect::from_min_max(row.rect.min, pos2(lanes.left(), row.rect.bottom()));
        painter.text(
            pos2(header.left() + 12.0, header.center().y),
            Align2::LEFT_CENTER,
            &row.track.name,
            mono_medium(11.0),
            t::TEXT,
        );
        let kind = if row.track.kind == TrackKind::Video {
            "Video"
        } else {
            "Audio"
        };
        painter.text(
            pos2(header.left() + 44.0, header.center().y),
            Align2::LEFT_CENTER,
            kind,
            sans(11.0),
            t::TEXT3,
        );
        painter.hline(
            header.x_range(),
            header.bottom() - 0.5,
            Stroke::new(1.0, t::LANE_LINE),
        );
    }
    painter.vline(
        lanes.left() - 0.5,
        headers.y_range(),
        Stroke::new(1.0, t::LINE),
    );
    if rows.is_empty() {
        painter.text(
            lanes.center(),
            Align2::CENTER_CENTER,
            "No tracks",
            sans(12.0),
            t::TEXT3,
        );
    }

    ruler_ui(ui, view, ruler, lanes, rate, commands);
    // Frames the frame cache holds, along the ruler's foot.
    let foot = ui.painter_at(ruler);
    for (start, end) in engine.cached_frames() {
        let (a, b) = (start as f64 / rate.as_f64(), end as f64 / rate.as_f64());
        let bar = Rect::from_min_max(
            pos2(view.x(a, lanes), ruler.bottom() - 3.0),
            pos2(
                view.x(b, lanes).max(view.x(a, lanes) + 1.0),
                ruler.bottom() - 1.0,
            ),
        );
        foot.rect_filled(bar, CornerRadius::ZERO, t::SAVED.gamma_multiply(0.8));
    }
    // The corner above the headers: where the playhead is.
    let corner = Rect::from_min_max(full.min, pos2(ruler.left(), ruler.bottom()));
    painter.rect_filled(corner, CornerRadius::ZERO, t::PANEL_HEADER);
    let index = engine.playhead().frame_index(rate).unwrap_or(0);
    painter.text(
        pos2(corner.left() + 12.0, corner.center().y),
        Align2::LEFT_CENTER,
        timecode(index, rate),
        mono(11.0),
        t::ACCENT,
    );

    // The playhead.
    let x = view.x(engine.playhead().as_f64(), lanes);
    if (lanes.left()..=lanes.right()).contains(&x) {
        painter.vline(x, lanes.y_range(), Stroke::new(1.0, t::ACCENT));
        let head = vec![
            pos2(x - 5.0, ruler.top() + 12.0),
            pos2(x + 5.0, ruler.top() + 12.0),
            pos2(x + 5.0, ruler.bottom() - 6.0),
            pos2(x, ruler.bottom()),
            pos2(x - 5.0, ruler.bottom() - 6.0),
        ];
        painter.add(egui::Shape::convex_polygon(head, t::ACCENT, Stroke::NONE));
    }
}

/// Ctrl+wheel zooms around the pointer; the wheel scrolls through time.
fn zoom_and_scroll(ui: &Ui, view: &mut TimelineView, area: Rect, lanes: Rect) {
    let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    if !area.contains(pointer) || !ui.rect_contains_pointer(area) {
        return;
    }
    let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
    if zoom != 1.0 {
        let at = view.seconds(pointer.x, lanes);
        view.points_per_second = (view.points_per_second * zoom).clamp(0.5, 2000.0);
        view.scroll = at - ((pointer.x - lanes.left()) / view.points_per_second) as f64;
    }
    let delta = if scroll.x != 0.0 { scroll.x } else { scroll.y };
    if delta != 0.0 {
        view.scroll -= (delta / view.points_per_second) as f64;
    }
    view.scroll = view.scroll.max(-1.0);
}

#[allow(clippy::too_many_arguments)]
fn clips(
    ui: &mut Ui,
    view: &mut TimelineView,
    gestures: &mut Gestures,
    doc: &Document,
    timeline: &Timeline,
    rows: &[Row],
    lanes: Rect,
    selected: &mut Option<NodeId>,
    edits: &mut Vec<Edit>,
    commands: &mut Vec<AppCommand>,
) {
    let painter = ui.painter_at(lanes);
    let doc_rate = timeline.settings.rate;
    for (&node, placement) in &timeline.clips {
        let Some(row) = rows.iter().find(|r| r.track.id == placement.track) else {
            continue;
        };
        let left = view.x(placement.start.as_f64(), lanes);
        let right = view.x(placement.end().as_f64(), lanes);
        let rect = Rect::from_min_max(
            pos2(left, row.rect.top() + 3.0),
            pos2(right.max(left + 2.0), row.rect.bottom() - 3.0),
        );
        if rect.right() < lanes.left() || rect.left() > lanes.right() {
            continue;
        }
        let id = ui.scope_id().with(("clip", node));
        let response = ui
            .interact(rect.intersect(lanes), id, Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::Grab);
        let name = doc.node(node).map_or("Clip", |n| n.name());
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::Button,
                true,
                *selected == Some(node),
                format!("Clip {name}"),
            )
        });
        if response.clicked() || response.drag_started() || response.secondary_clicked() {
            *selected = Some(node);
        }
        if response.double_clicked() {
            commands.push(AppCommand::OpenInCompose);
        }
        response.context_menu(|ui| {
            ui.set_min_width(220.0);
            let open = egui::Button::new("Open in Compose").shortcut_text("Enter");
            if ui
                .add(open)
                .on_hover_text("Build a node graph inside this clip")
                .clicked()
            {
                *selected = Some(node);
                commands.push(AppCommand::OpenInCompose);
                ui.close();
            }
        });
        if response.drag_started()
            && let Some(pos) = response.interact_pointer_pos()
        {
            view.drag = Some(ClipDrag {
                node,
                grab: view.seconds(pos.x, lanes) - placement.start.as_f64(),
            });
        }
        if let (true, Some(drag), Some(pos)) = (
            response.dragged(),
            view.drag.as_ref().filter(|d| d.node == node),
            response.interact_pointer_pos(),
        ) {
            let start = frame_start((view.seconds(pos.x, lanes) - drag.grab).max(0.0), doc_rate);
            // Across tracks of the same kind, as the pointer goes.
            let track = rows
                .iter()
                .find(|r| r.rect.y_range().contains(pos.y) && r.track.kind == row.track.kind)
                .map_or(placement.track, |r| r.track.id);
            let moved = Placement {
                start,
                track,
                ..placement.clone()
            };
            if moved != *placement && timeline.check_placement(&moved, Some(node)).is_ok() {
                edits.push(Edit {
                    label: "Move Clip".into(),
                    commands: vec![Command::SetPlacement {
                        node,
                        placement: moved,
                    }],
                    gesture: Some(gestures.drag(id)),
                });
            }
        }
        if response.drag_stopped() {
            gestures.end(id);
            view.drag = None;
        }

        let (fill, border) = if row.track.kind == TrackKind::Video {
            t::CLIP_VIDEO
        } else {
            t::CLIP_AUDIO
        };
        let is_selected = *selected == Some(node);
        painter.rect(
            rect,
            CornerRadius::same(3),
            fill,
            Stroke::new(
                if is_selected { 1.5 } else { 1.0 },
                if is_selected { t::ACCENT } else { border },
            ),
            StrokeKind::Inside,
        );
        let mut label_area = rect.shrink2(vec2(7.0, 4.0)).intersect(lanes);
        // A clip with a graph inside says so.
        if doc.graph(node).is_some() && label_area.width() > 30.0 {
            let badge = Rect::from_min_size(
                pos2(label_area.right() - 12.0, rect.top() + 6.0),
                vec2(12.0, 12.0),
            );
            icons::paint(&painter, badge, icons::GRAPH, t::TEXT_BRIGHT, 1.5, false);
            label_area.max.x -= 16.0;
        }
        if label_area.width() > 12.0 {
            ui.painter_at(label_area).text(
                pos2(rect.left().max(lanes.left()) + 7.0, rect.top() + 12.0),
                Align2::LEFT_CENTER,
                name,
                medium(11.0),
                t::TEXT_BRIGHT,
            );
        }
    }
}

fn ruler_ui(
    ui: &mut Ui,
    view: &TimelineView,
    ruler: Rect,
    lanes: Rect,
    rate: Time,
    commands: &mut Vec<AppCommand>,
) {
    let response = ui.interact(ruler, ui.scope_id().with("ruler"), Sense::click_and_drag());
    if (response.clicked() || response.dragged())
        && let Some(pos) = response.interact_pointer_pos()
    {
        commands.push(AppCommand::Seek(frame_start(
            view.seconds(pos.x, lanes).max(0.0),
            rate,
        )));
    }
    let painter = ui.painter_at(ruler);
    painter.rect_filled(ruler, CornerRadius::ZERO, t::PANEL_HEADER);
    painter.hline(
        ruler.x_range(),
        ruler.bottom() - 0.5,
        Stroke::new(1.0, t::LINE),
    );

    // The smallest step, in frames, whose labels don't crowd.
    let fps = rate.as_f64().round().max(1.0);
    let frame_points = view.points_per_second as f64 / rate.as_f64();
    let mut steps = [1.0, 2.0, 5.0, 10.0].into_iter().chain(
        [
            1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
        ]
        .map(|s| s * fps),
    );
    let step = steps
        .clone()
        .find(|s| s * frame_points >= LABEL_SPACING as f64)
        .unwrap_or(3600.0 * fps);
    // The finest step that divides the labelled one and isn't crowded.
    let minor = steps.find(|s| *s < step && (step / s).fract() == 0.0 && s * frame_points >= 8.0);
    let first = (view.seconds(ruler.left(), lanes).max(0.0) * rate.as_f64()).floor();
    let last = view.seconds(ruler.right(), lanes) * rate.as_f64();
    if let Some(minor) = minor {
        let mut f = (first / minor).floor() * minor;
        while f <= last {
            let x = view.x(f / rate.as_f64(), lanes);
            painter.vline(
                x,
                (ruler.bottom() - 5.0)..=ruler.bottom(),
                Stroke::new(1.0, t::LINE2),
            );
            f += minor;
        }
    }
    let mut f = (first / step).floor() * step;
    while f <= last {
        let x = view.x(f / rate.as_f64(), lanes);
        painter.vline(
            x,
            (ruler.bottom() - 10.0)..=ruler.bottom(),
            Stroke::new(1.0, t::TEXT3),
        );
        painter.text(
            pos2(x + 4.0, ruler.top() + 9.0),
            Align2::LEFT_CENTER,
            timecode(f as i64, rate),
            mono(10.0),
            t::TEXT3,
        );
        f += step;
    }
}
