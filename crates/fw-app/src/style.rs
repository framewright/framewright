//! The Studio design system on egui: color tokens, fonts and visuals
//! (design §3.4).

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle};

/// Color tokens, sRGB.
pub mod tokens {
    use egui::Color32;

    const fn rgb(v: u32) -> Color32 {
        Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }

    pub const BG: Color32 = rgb(0x161618);
    pub const CHROME: Color32 = rgb(0x1b1b1e);
    pub const PANEL: Color32 = rgb(0x1e1e21);
    pub const PANEL_HEADER: Color32 = rgb(0x222225);
    pub const RAISED: Color32 = rgb(0x26262a);
    pub const RAISED2: Color32 = rgb(0x2c2c31);
    pub const LINE: Color32 = rgb(0x2a2a2f);
    pub const LINE2: Color32 = rgb(0x34343a);
    pub const TEXT: Color32 = rgb(0xe6e6e8);
    pub const TEXT_BRIGHT: Color32 = rgb(0xf0f0f2);
    pub const TEXT2: Color32 = rgb(0xa3a3aa);
    pub const TEXT3: Color32 = rgb(0x8f8f97);
    pub const TEXT_DISABLED: Color32 = rgb(0x5c5c63);
    pub const ACCENT: Color32 = rgb(0xe8a33d);
    pub const ACCENT_INK: Color32 = rgb(0x1a1408);
    pub const WELL: Color32 = rgb(0x0f0f11);
    pub const FIELD: Color32 = rgb(0x28282c);
    pub const TRACK: Color32 = rgb(0x34343a);
    pub const TRACK_FILL: Color32 = rgb(0x6b6b73);
    pub const HANDLE: Color32 = rgb(0xd5d5da);
    pub const LANE_VIDEO: Color32 = rgb(0x1c1c1f);
    pub const LANE_AUDIO: Color32 = rgb(0x1a1b1d);
    pub const LANE_LINE: Color32 = rgb(0x26262b);
    /// Clips on the timeline: fill and border.
    pub const CLIP_VIDEO: (Color32, Color32) = (rgb(0x3a5577), rgb(0x4d6c93));
    pub const CLIP_AUDIO: (Color32, Color32) = (rgb(0x2f5238), rgb(0x44704f));
    pub const FIELD_BORDER: Color32 = rgb(0x303036);
    pub const MENU_BG: Color32 = rgb(0x26262a);
    pub const MENU_BORDER: Color32 = rgb(0x3a3a41);
    pub const SAVED: Color32 = rgb(0x7fb38a);
    pub const CLOSE_HOVER: Color32 = rgb(0xc42b1c);
    /// A highlighted row in a menu or list.
    pub const MENU_ON: Color32 = rgb(0x2f2d29);
    /// Something the document won't accept, such as a connection making a loop.
    pub const REFUSED: Color32 = rgb(0xd0605a);
    /// The node editor: node bodies, the grid, connections and ports.
    pub const NODE: Color32 = rgb(0x26262b);
    pub const GRID: Color32 = rgb(0x1d1d21);
    pub const GRID_FINE: Color32 = rgb(0x151518);
    pub const WIRE: Color32 = rgb(0x55555d);
    pub const PORT_IMAGE: Color32 = rgb(0x8a8a92);
    pub const PORT_MASK: Color32 = rgb(0x6f8fc4);
    /// Node categories' colors.
    pub const NODE_SOURCE: Color32 = rgb(0x6f8bb4);
    pub const NODE_FILTER: Color32 = rgb(0x7d7d86);
    pub const NODE_COLOR: Color32 = rgb(0xb58e5c);
    pub const NODE_CONTAINER: Color32 = rgb(0x8f83c2);
    pub const NODE_OUTPUT: Color32 = rgb(0xb47a84);
}

const SANS: &str = "plex-sans";
const SANS_MEDIUM: &str = "plex-sans-medium";
const SANS_SEMIBOLD: &str = "plex-sans-semibold";
const MONO: &str = "plex-mono";
const MONO_MEDIUM: &str = "plex-mono-medium";

pub fn sans(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SANS_MEDIUM.into()))
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SANS_SEMIBOLD.into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn mono_medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MONO_MEDIUM.into()))
}

/// Installs the fonts and visuals on `ctx`.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let files: [(&str, &'static [u8]); 5] = [
        (
            SANS,
            include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
        ),
        (
            SANS_MEDIUM,
            include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"),
        ),
        (
            SANS_SEMIBOLD,
            include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
        ),
        (
            MONO,
            include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
        ),
        (
            MONO_MEDIUM,
            include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf"),
        ),
    ];
    for (name, bytes) in files {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    // egui's own fonts stay as fallbacks, for symbols Plex lacks.
    let families = &mut fonts.families;
    families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, SANS.into());
    families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, MONO.into());
    let fallback = families[&FontFamily::Proportional].clone();
    for name in [SANS_MEDIUM, SANS_SEMIBOLD, MONO_MEDIUM] {
        let mut list = vec![name.to_owned()];
        list.extend(fallback.iter().cloned());
        families.insert(FontFamily::Name(name.into()), list);
    }
    ctx.set_fonts(fonts);

    ctx.global_style_mut(|s| {
        use tokens as t;
        s.text_styles = [
            (TextStyle::Small, sans(11.0)),
            (TextStyle::Body, sans(12.0)),
            (TextStyle::Button, sans(12.0)),
            (TextStyle::Heading, semibold(14.0)),
            (TextStyle::Monospace, mono(11.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(6.0, 0.0);
        s.spacing.button_padding = egui::vec2(8.0, 4.0);
        s.spacing.interact_size.y = 22.0;
        s.spacing.menu_margin = egui::Margin::symmetric(0, 4);
        s.interaction.tooltip_delay = 0.4;
        let v = &mut s.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = t::PANEL;
        v.window_fill = t::MENU_BG;
        v.window_stroke = Stroke::new(1.0, t::MENU_BORDER);
        v.window_corner_radius = egui::CornerRadius::same(5);
        v.menu_corner_radius = egui::CornerRadius::same(5);
        v.extreme_bg_color = t::RAISED;
        v.faint_bg_color = t::RAISED;
        v.override_text_color = Some(t::TEXT);
        v.hyperlink_color = t::ACCENT;
        v.selection.bg_fill = Color32::from_rgba_unmultiplied(0xe8, 0xa3, 0x3d, 90);
        v.selection.stroke = Stroke::new(1.0, t::ACCENT);
        v.text_cursor.stroke = Stroke::new(1.5, t::ACCENT);
        let w = &mut v.widgets;
        for state in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            state.corner_radius = egui::CornerRadius::same(3);
            state.expansion = 0.0;
        }
        w.noninteractive.bg_fill = t::PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, t::LINE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, t::TEXT2);
        w.inactive.bg_fill = t::RAISED;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::new(1.0, t::FIELD_BORDER);
        w.inactive.fg_stroke = Stroke::new(1.0, t::TEXT2);
        w.hovered.bg_fill = t::RAISED2;
        w.hovered.weak_bg_fill = t::RAISED2;
        w.hovered.bg_stroke = Stroke::new(1.0, t::LINE2);
        w.hovered.fg_stroke = Stroke::new(1.0, t::TEXT_BRIGHT);
        w.active.bg_fill = t::RAISED2;
        w.active.weak_bg_fill = t::RAISED2;
        w.active.fg_stroke = Stroke::new(1.0, t::TEXT_BRIGHT);
        w.open.bg_fill = t::RAISED2;
        w.open.weak_bg_fill = t::RAISED2;
    });
}
