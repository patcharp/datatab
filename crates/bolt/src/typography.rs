use gpui::App;
use std::borrow::Cow;

pub struct BundledFontAsset {
    pub family: &'static str,
    pub file_name: &'static str,
    pub data: &'static [u8],
}

// Metrics sweep checklist (design-bundle-application S9b.5):
// - Button label vertical centering at Heights::BUTTON (30px)
// - Input baseline at Heights::INPUT (30px)
// - Doc tab label truncation at Heights::TAB (36px)
// - Status-bar items at Heights::TOOLBAR (32px)
// - Modal body min-height 96px
// - Table row text at Heights::ROW (28px) / ROW_COMPACT (24px)
pub struct AppFonts;

impl AppFonts {
    /// Interface face: labels, buttons, menus, settings, body copy, tabs and tree rows.
    pub const INTERFACE: &'static str = "IBM Plex Sans Thai";
    /// Display face reserved for the wordmark.
    pub const DISPLAY: &'static str = "Archivo Expanded";
    /// Data face: grid cells, keys, queries, identifiers, key hints and numeric readouts.
    pub const MONO: &'static str = "JetBrains Mono";
    /// Face under the data face, for the text [`Self::MONO`] has no glyph for.
    ///
    /// JetBrains Mono covers no Thai code point, so Thai data in a grid cell or
    /// an editor line used to fall through to a system font at a size that did
    /// not match the Latin text beside it. No Noto Sans Mono carries Thai, so
    /// the data face stays the Latin mono family and this bundled Thai family
    /// sits under it.
    pub const MONO_FALLBACK: &'static str = "Noto Sans Thai";
}

pub const BUNDLED_FONT_ASSETS: [BundledFontAsset; 17] = [
    BundledFontAsset {
        family: AppFonts::INTERFACE,
        file_name: "IBMPlexSansThai-Regular.ttf",
        data: include_bytes!("../assets/fonts/IBMPlexSansThai-Regular.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::INTERFACE,
        file_name: "IBMPlexSansThai-Medium.ttf",
        data: include_bytes!("../assets/fonts/IBMPlexSansThai-Medium.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::INTERFACE,
        file_name: "IBMPlexSansThai-SemiBold.ttf",
        data: include_bytes!("../assets/fonts/IBMPlexSansThai-SemiBold.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::INTERFACE,
        file_name: "IBMPlexSansThai-Bold.ttf",
        data: include_bytes!("../assets/fonts/IBMPlexSansThai-Bold.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::DISPLAY,
        file_name: "ArchivoExpanded-ExtraBold.ttf",
        data: include_bytes!("../assets/fonts/ArchivoExpanded-ExtraBold.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO,
        file_name: "JetBrainsMono-Regular.ttf",
        data: include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO,
        file_name: "JetBrainsMono-Italic.ttf",
        data: include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO,
        file_name: "JetBrainsMono-Medium.ttf",
        data: include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO,
        file_name: "JetBrainsMono-MediumItalic.ttf",
        data: include_bytes!("../assets/fonts/JetBrainsMono-MediumItalic.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO,
        file_name: "JetBrainsMono-SemiBold.ttf",
        data: include_bytes!("../assets/fonts/JetBrainsMono-SemiBold.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO,
        file_name: "JetBrainsMono-SemiBoldItalic.ttf",
        data: include_bytes!("../assets/fonts/JetBrainsMono-SemiBoldItalic.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO,
        file_name: "JetBrainsMono-Bold.ttf",
        data: include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO,
        file_name: "JetBrainsMono-BoldItalic.ttf",
        data: include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO_FALLBACK,
        file_name: "NotoSansThai-Regular.ttf",
        data: include_bytes!("../assets/fonts/NotoSansThai-Regular.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO_FALLBACK,
        file_name: "NotoSansThai-Medium.ttf",
        data: include_bytes!("../assets/fonts/NotoSansThai-Medium.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO_FALLBACK,
        file_name: "NotoSansThai-SemiBold.ttf",
        data: include_bytes!("../assets/fonts/NotoSansThai-SemiBold.ttf"),
    },
    BundledFontAsset {
        family: AppFonts::MONO_FALLBACK,
        file_name: "NotoSansThai-Bold.ttf",
        data: include_bytes!("../assets/fonts/NotoSansThai-Bold.ttf"),
    },
];

pub fn bundled_font_data() -> Vec<Cow<'static, [u8]>> {
    BUNDLED_FONT_ASSETS
        .iter()
        .map(|font| Cow::Borrowed(font.data))
        .collect()
}

/// Load all bundled fonts into GPUI's text system.
///
/// Called once during UI theme initialization. If registration fails, keep
/// the app running so GPUI can fall back to system fonts instead of aborting
/// startup.
pub fn load_bundled_fonts(cx: &mut App) {
    if let Err(error) = cx.text_system().add_fonts(bundled_font_data()) {
        eprintln!("failed to register bundled UI fonts, falling back to system fonts: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::{AppFonts, BUNDLED_FONT_ASSETS};

    #[test]
    fn font_contract_splits_interface_display_and_data_families() {
        assert_eq!(AppFonts::INTERFACE, "IBM Plex Sans Thai");
        assert_eq!(AppFonts::DISPLAY, "Archivo Expanded");
        // The Latin data face does not change: Thai arrives through the
        // fallback under it, never by swapping the family of grid cells and
        // queries.
        assert_eq!(AppFonts::MONO, "JetBrains Mono");
        assert_eq!(AppFonts::MONO_FALLBACK, "Noto Sans Thai");
    }

    #[test]
    fn bundled_assets_cover_every_family() {
        let assets: Vec<_> = BUNDLED_FONT_ASSETS
            .iter()
            .map(|asset| (asset.family, asset.file_name))
            .collect();

        assert_eq!(
            assets,
            vec![
                (AppFonts::INTERFACE, "IBMPlexSansThai-Regular.ttf"),
                (AppFonts::INTERFACE, "IBMPlexSansThai-Medium.ttf"),
                (AppFonts::INTERFACE, "IBMPlexSansThai-SemiBold.ttf"),
                (AppFonts::INTERFACE, "IBMPlexSansThai-Bold.ttf"),
                (AppFonts::DISPLAY, "ArchivoExpanded-ExtraBold.ttf"),
                (AppFonts::MONO, "JetBrainsMono-Regular.ttf"),
                (AppFonts::MONO, "JetBrainsMono-Italic.ttf"),
                (AppFonts::MONO, "JetBrainsMono-Medium.ttf"),
                (AppFonts::MONO, "JetBrainsMono-MediumItalic.ttf"),
                (AppFonts::MONO, "JetBrainsMono-SemiBold.ttf"),
                (AppFonts::MONO, "JetBrainsMono-SemiBoldItalic.ttf"),
                (AppFonts::MONO, "JetBrainsMono-Bold.ttf"),
                (AppFonts::MONO, "JetBrainsMono-BoldItalic.ttf"),
                (AppFonts::MONO_FALLBACK, "NotoSansThai-Regular.ttf"),
                (AppFonts::MONO_FALLBACK, "NotoSansThai-Medium.ttf"),
                (AppFonts::MONO_FALLBACK, "NotoSansThai-SemiBold.ttf"),
                (AppFonts::MONO_FALLBACK, "NotoSansThai-Bold.ttf"),
            ]
        );
    }

    #[test]
    fn the_data_faces_thai_fallback_is_bundled_not_the_system_generic() {
        // A generic family would resolve to whatever font the platform picks,
        // which is the bug #137 reports: Thai data at the wrong size in the
        // grid. The fallback is a family the app ships.
        assert_ne!(AppFonts::MONO_FALLBACK, "monospace");

        let faces: Vec<_> = BUNDLED_FONT_ASSETS
            .iter()
            .filter(|asset| asset.family == AppFonts::MONO_FALLBACK)
            .map(|asset| asset.file_name)
            .collect();

        assert_eq!(
            faces,
            vec![
                "NotoSansThai-Regular.ttf",
                "NotoSansThai-Medium.ttf",
                "NotoSansThai-SemiBold.ttf",
                "NotoSansThai-Bold.ttf",
            ]
        );
    }

    #[test]
    fn interface_font_files_have_the_requested_family_and_weights() {
        // GPUI's test platform uses NoopTextSystem, which does not load fonts.
        fn u16_at(data: &[u8], offset: usize) -> u16 {
            u16::from_be_bytes(data[offset..offset + 2].try_into().expect("u16"))
        }
        fn u32_at(data: &[u8], offset: usize) -> usize {
            u32::from_be_bytes(data[offset..offset + 4].try_into().expect("u32")) as usize
        }
        fn table(data: &[u8], tag: &[u8; 4]) -> usize {
            (0..usize::from(u16_at(data, 4)))
                .map(|index| 12 + index * 16)
                .find(|&offset| &data[offset..offset + 4] == tag)
                .map(|offset| u32_at(data, offset + 8))
                .expect("font table")
        }
        let faces = [
            ("IBMPlexSansThai-Regular.ttf", 400),
            ("IBMPlexSansThai-Medium.ttf", 500),
            ("IBMPlexSansThai-SemiBold.ttf", 600),
            ("IBMPlexSansThai-Bold.ttf", 700),
        ];
        for (file_name, weight) in faces {
            let asset = BUNDLED_FONT_ASSETS
                .iter()
                .find(|asset| asset.file_name == file_name)
                .expect("bundled face");
            assert_eq!(asset.family, AppFonts::INTERFACE);
            let data = asset.data;
            assert_eq!(
                u16_at(data, table(data, b"OS/2") + 4),
                weight,
                "{file_name}"
            );
            let names = table(data, b"name");
            let strings = names + usize::from(u16_at(data, names + 4));
            let has_family = (0..usize::from(u16_at(data, names + 2))).any(|index| {
                let record = names + 6 + index * 12;
                let platform = u16_at(data, record);
                let name_id = u16_at(data, record + 6);
                if platform != 3 || !matches!(name_id, 1 | 16) {
                    return false;
                }
                let length = usize::from(u16_at(data, record + 8));
                let start = strings + usize::from(u16_at(data, record + 10));
                let utf16 = data[start..start + length]
                    .chunks_exact(2)
                    .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
                    .collect::<Vec<_>>();
                String::from_utf16(&utf16).ok().as_deref() == Some(AppFonts::INTERFACE)
            });
            assert!(has_family, "{file_name} lacks the interface family");
        }
    }
}
