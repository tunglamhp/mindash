//! Theme application.
//!
//! Colours are written to `:root` as custom properties and the stylesheet only
//! ever reads those, so changing a preset repaints without invalidating a single
//! component. The Python version did the same thing, and it is the one part of
//! that architecture worth keeping exactly as it was.

use mindash_core::ThemeColors;
use wasm_bindgen::JsCast;

/// Apply a theme by setting every custom property in one pass.
pub fn apply(theme: &ThemeColors) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Some(root) = document.document_element() else {
        return;
    };
    let Ok(html) = root.dyn_into::<web_sys::HtmlElement>() else {
        return;
    };

    let mut css = String::with_capacity(512);
    let mut push = |k: &str, v: String| {
        css.push_str(&format!("{k}: {v};"));
    };

    push("--bg", theme.bg.clone());
    push("--surface", theme.cards.clone());
    push("--border", theme.border.clone());
    push("--text", theme.text.clone());
    push("--text-muted", theme.text_muted.clone());
    push("--accent", theme.accent.clone());
    push("--danger", theme.danger.clone());
    push("--warning", theme.warning.clone());
    push("--radius", format!("{}px", theme.border_radius));
    push("--max-width", format!("{}%", theme.max_width_desktop));

    // Derived tokens. Doing this here rather than in CSS keeps `color-mix`
    // out of the critical path, which matters because mixing against
    // `transparent` composites inconsistently between engines.
    push("--accent-soft", rgba(&theme.accent, 0.14));
    push("--danger-soft", rgba(&theme.danger, 0.14));
    push("--on-accent", readable_ink(&theme.accent, &theme.bg));

    html.style().set_css_text(&css);

    // Wallpaper is set on the body so the shell keeps its own background.
    if let Some(body) = document.body() {
        if theme.wallpaper.is_empty() {
            let _ = body.style().remove_property("background-image");
            let _ = body.class_list().remove_1("has-wallpaper");
        } else {
            let _ = body
                .style()
                .set_property("background-image", &format!("url(\"{}\")", theme.wallpaper));
            let _ = body.class_list().add_1("has-wallpaper");
        }
    }
}

/// `#rrggbb` plus an alpha as an `rgba()` string.
fn rgba(hex: &str, alpha: f32) -> String {
    let (r, g, b) = match parse_hex(hex) {
        Some(v) => v,
        None => return format!("rgba(127,127,127,{alpha})"),
    };
    format!("rgba({r},{g},{b},{alpha})")
}

fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    let h = hex.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ))
}

fn luminance(hex: &str) -> f32 {
    let Some((r, g, b)) = parse_hex(hex) else {
        return 0.0;
    };
    let f = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
}

fn contrast(a: f32, b: f32) -> f32 {
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// Ink that stays legible on a filled accent. Picks whichever of the theme
/// background or white reads better, so a light accent can never produce
/// near-invisible button text.
fn readable_ink(accent: &str, background: &str) -> String {
    let a = luminance(accent);
    let bg = if parse_hex(background).is_some() {
        background.to_string()
    } else {
        "#0f0f10".to_string()
    };
    if contrast(a, luminance(&bg)) >= contrast(a, 1.0) {
        bg
    } else {
        "#ffffff".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing() {
        assert_eq!(parse_hex("#2ed573"), Some((46, 213, 115)));
        assert_eq!(parse_hex("2ed573"), Some((46, 213, 115)));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("nope"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn rgba_formats() {
        assert_eq!(rgba("#2ed573", 0.5), "rgba(46,213,115,0.5)");
        assert!(rgba("bad", 0.5).starts_with("rgba(127"));
    }

    #[test]
    fn ink_is_readable_on_a_variety_of_accents() {
        for accent in ["#2ed573", "#ffffff", "#000000", "#ffa502", "#64A2A3"] {
            let ink = readable_ink(accent, "#161616");
            let ratio = contrast(luminance(accent), luminance(&ink));
            assert!(ratio >= 4.5, "{accent} -> {ink} only {ratio:.2}");
        }
    }

    #[test]
    fn dark_ink_preferred_for_a_bright_accent() {
        assert_eq!(readable_ink("#ffffff", "#161616"), "#161616");
    }
}
