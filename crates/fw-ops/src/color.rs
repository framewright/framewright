//! Color-space choices offered by nodes.

use fw_core::ColorSpace;

/// The choice ids and labels for a color-space parameter. `"auto"` means
/// "whatever the file or delivery says" and is resolved by the node.
pub const SPACE_CHOICES: [(&str, &str); 6] = [
    ("auto", "Auto"),
    ("srgb", "sRGB"),
    ("linear_rec709", "Linear Rec.709"),
    ("acescg", "ACEScg"),
    ("acescct", "ACEScct"),
    ("rec709_display", "Rec.709 (2.4)"),
];

/// The color space for a choice id, or `None` for `"auto"`.
pub fn space_from_choice(id: &str) -> Option<ColorSpace> {
    match id {
        "srgb" => Some(ColorSpace::SRGB),
        "linear_rec709" => Some(ColorSpace::LINEAR_REC709),
        "acescg" => Some(ColorSpace::ACESCG),
        "acescct" => Some(ColorSpace::ACESCCT),
        "rec709_display" => Some(ColorSpace::REC709_DISPLAY),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_choice_but_auto_maps_to_a_space() {
        for (id, _) in SPACE_CHOICES {
            assert_eq!(space_from_choice(id).is_none(), id == "auto", "{id}");
        }
    }
}
