//! How each style draws the dock's panel: its colours, edge, corners, shadow, and whether it
//! hangs from the screen edge or floats just below it. Plain values, no Windows calls, so they
//! can be tested.

use crate::config::{DockLook, DockStyle};

/// A colour with its opacity, each from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Skin {
    /// The fill at the panel's top (the screen edge, when it hangs from it) and at its bottom.
    pub fill_top: Rgba,
    pub fill_bottom: Rgba,
    /// The thin line round the panel.
    pub edge: Rgba,
    /// A second line just inside the edge (glass's highlight), if any.
    pub inner_edge: Option<Rgba>,
    /// The lines between groups of icons (separators).
    pub divider: Rgba,
    /// How dark the soft shadow round the panel is next to it (it fades out from there).
    pub shadow: f32,
    /// Corner radius in logical pixels; `None`: round ends (half the panel's height).
    pub radius: Option<f32>,
    /// Space between the screen edge and the panel, in logical pixels; 0: it hangs from the edge.
    pub float: f32,
    /// How strong the hover glow is at its middle.
    pub glow_peak: f32,
}

/// The skin for a style. `look` is the light or dark choice the classic style uses;
/// `windows_dark` is Windows' own app mode, which the capsule follows.
pub fn skin(style: DockStyle, look: DockLook, windows_dark: bool) -> Skin {
    let white = |a: f32| Rgba(1.0, 1.0, 1.0, a);
    let ink = |a: f32| Rgba(0.12, 0.16, 0.24, a);
    match style {
        // CrystalXP's: white, brightening from 8% at the screen edge to 49% at the bottom.
        DockStyle::Classic if look == DockLook::Light => Skin {
            fill_top: white(0.08),
            fill_bottom: white(0.49),
            edge: white(0.72),
            inner_edge: None,
            divider: white(0.51),
            shadow: 0.38,
            radius: Some(7.0),
            float: 0.0,
            glow_peak: 0.6,
        },
        // Smoked glass, deepening toward the bottom edge.
        DockStyle::Classic => Skin {
            fill_top: Rgba(0.06, 0.06, 0.08, 0.55),
            fill_bottom: Rgba(0.06, 0.06, 0.08, 0.80),
            edge: white(0.28),
            inner_edge: None,
            divider: white(0.30),
            shadow: 0.38,
            radius: Some(7.0),
            float: 0.0,
            glow_peak: 0.7,
        },
        // Almost clear: the wallpaper shows through, held by a bright rim.
        DockStyle::Glass => Skin {
            fill_top: white(0.02),
            fill_bottom: white(0.12),
            edge: white(0.62),
            inner_edge: Some(white(0.28)),
            divider: white(0.45),
            shadow: 0.20,
            radius: Some(18.0),
            float: 0.0,
            glow_peak: 0.6,
        },
        // Milky white: icons stand out on any wallpaper.
        DockStyle::Frosted => Skin {
            fill_top: Rgba(0.965, 0.973, 0.984, 0.66),
            fill_bottom: Rgba(0.965, 0.973, 0.984, 0.84),
            edge: white(0.95),
            inner_edge: None,
            divider: ink(0.16),
            shadow: 0.18,
            radius: Some(12.0),
            float: 0.0,
            glow_peak: 0.6,
        },
        DockStyle::Capsule if !windows_dark => Skin {
            fill_top: Rgba(0.973, 0.976, 0.988, 0.72),
            fill_bottom: Rgba(0.973, 0.976, 0.988, 0.72),
            edge: white(0.92),
            inner_edge: None,
            divider: ink(0.16),
            shadow: 0.22,
            radius: None,
            float: 8.0,
            glow_peak: 0.6,
        },
        DockStyle::Capsule => Skin {
            fill_top: Rgba(0.086, 0.094, 0.118, 0.62),
            fill_bottom: Rgba(0.086, 0.094, 0.118, 0.62),
            edge: white(0.20),
            inner_edge: None,
            divider: white(0.25),
            shadow: 0.30,
            radius: None,
            float: 8.0,
            glow_peak: 0.7,
        },
    }
}

/// Whether the style's colours change with Windows' app mode (light or dark), so the dock
/// must look again when that changes.
pub fn follows_windows(style: DockStyle) -> bool {
    style == DockStyle::Capsule
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLES: [DockStyle; 4] = [DockStyle::Classic, DockStyle::Glass, DockStyle::Frosted, DockStyle::Capsule];

    #[test]
    fn the_classic_style_is_the_dock_as_it_always_was() {
        let light = skin(DockStyle::Classic, DockLook::Light, false);
        assert_eq!((light.fill_top, light.fill_bottom), (Rgba(1.0, 1.0, 1.0, 0.08), Rgba(1.0, 1.0, 1.0, 0.49)));
        assert_eq!((light.edge.3, light.divider.3, light.shadow, light.radius, light.float, light.glow_peak), (0.72, 0.51, 0.38, Some(7.0), 0.0, 0.6));
        let dark = skin(DockStyle::Classic, DockLook::Dark, false);
        assert_eq!((dark.fill_top, dark.fill_bottom), (Rgba(0.06, 0.06, 0.08, 0.55), Rgba(0.06, 0.06, 0.08, 0.80)));
        assert_eq!((dark.edge.3, dark.divider.3, dark.glow_peak), (0.28, 0.30, 0.7));
        // Windows' own mode doesn't change it: the classic style follows its own light or dark.
        assert_eq!(skin(DockStyle::Classic, DockLook::Light, true), light);
        assert_eq!(skin(DockStyle::Classic, DockLook::Dark, true), dark);
    }

    #[test]
    fn only_the_capsule_floats_and_has_round_ends_and_it_follows_windows() {
        for style in STYLES {
            let s = skin(style, DockLook::Light, false);
            let capsule = style == DockStyle::Capsule;
            assert_eq!((s.float > 0.0, s.radius.is_none(), follows_windows(style)), (capsule, capsule, capsule), "{style:?}");
        }
        let (light, dark) = (skin(DockStyle::Capsule, DockLook::Light, false), skin(DockStyle::Capsule, DockLook::Light, true));
        assert!(light.fill_top.0 > 0.9 && dark.fill_top.0 < 0.2, "light with Windows light, dark with Windows dark");
        assert_eq!(skin(DockStyle::Capsule, DockLook::Dark, false), light, "Windows decides, not the classic look");
    }

    #[test]
    fn glass_is_clearer_and_rounder_and_frosted_is_milkier_than_classic() {
        let (classic, glass, frosted) = (skin(DockStyle::Classic, DockLook::Light, false), skin(DockStyle::Glass, DockLook::Light, false), skin(DockStyle::Frosted, DockLook::Light, false));
        assert!(glass.fill_bottom.3 < classic.fill_bottom.3 && glass.radius > classic.radius && glass.inner_edge.is_some());
        assert!(frosted.fill_top.3 > classic.fill_bottom.3 && frosted.radius > classic.radius);
        // On a milky panel, white separators wouldn't show: frosted's are dark.
        assert!(frosted.divider.0 < 0.5);
        // Glass and frosted look the same in either mode; they don't follow the classic look.
        assert_eq!(skin(DockStyle::Glass, DockLook::Dark, true), glass);
        assert_eq!(skin(DockStyle::Frosted, DockLook::Dark, true), frosted);
    }

    #[test]
    fn every_colour_is_a_real_colour() {
        for style in STYLES {
            for (look, dark) in [(DockLook::Light, false), (DockLook::Dark, true)] {
                let s = skin(style, look, dark);
                let colours = [s.fill_top, s.fill_bottom, s.edge, s.divider].into_iter().chain(s.inner_edge);
                for Rgba(r, g, b, a) in colours {
                    assert!([r, g, b, a].iter().all(|v| (0.0..=1.0).contains(v)), "{style:?}");
                }
                assert!((0.0..=1.0).contains(&s.shadow) && (0.0..=1.0).contains(&s.glow_peak));
            }
        }
    }
}
