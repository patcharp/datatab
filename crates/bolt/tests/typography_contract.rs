use bolt::typography::{AppFonts, BUNDLED_FONT_ASSETS, bundled_font_data};
use std::path::Path;
use ttf_parser::{Face, Weight};

const FONT_DIRECTORY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts");

#[test]
fn app_fonts_define_shared_family_contract() {
    assert_eq!(AppFonts::INTERFACE, "IBM Plex Sans Thai");
    assert_eq!(AppFonts::DISPLAY, "Archivo Expanded");
    assert_eq!(AppFonts::MONO, "JetBrains Mono");
    assert_eq!(AppFonts::MONO_FALLBACK, "Noto Sans Thai");
}

#[test]
fn the_mono_fallback_is_a_bundled_thai_face_not_a_system_generic() {
    // A generic family is resolved by the platform, which is what rendered Thai
    // data in a system font at the wrong size before #137. The fallback must be
    // a family the app ships.
    assert_ne!(AppFonts::MONO_FALLBACK, "monospace");
    assert_ne!(AppFonts::MONO_FALLBACK, "sans-serif");

    // The Latin data face does not change. Thai arrives from the face under it,
    // never by swapping the family of grid cells and queries.
    assert_eq!(AppFonts::MONO, "JetBrains Mono");

    let thai_faces: Vec<_> = BUNDLED_FONT_ASSETS
        .iter()
        .filter(|asset| asset.family == AppFonts::MONO_FALLBACK)
        .map(|asset| asset.file_name)
        .collect();

    assert_eq!(
        thai_faces,
        vec![
            "NotoSansThai-Regular.ttf",
            "NotoSansThai-Medium.ttf",
            "NotoSansThai-SemiBold.ttf",
            "NotoSansThai-Bold.ttf",
        ]
    );
}

#[test]
fn bundled_font_data_registers_all_shared_font_assets() {
    let bundled_fonts = bundled_font_data();

    let expected_assets = [
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
    ];

    let actual_assets: Vec<_> = BUNDLED_FONT_ASSETS
        .iter()
        .map(|asset| (asset.family, asset.file_name))
        .collect();

    assert_eq!(actual_assets, expected_assets);
    assert_eq!(bundled_fonts.len(), expected_assets.len());

    for (asset, bundled_font) in BUNDLED_FONT_ASSETS.iter().zip(bundled_fonts.iter()) {
        assert_eq!(
            bundled_font.as_ref(),
            asset.data,
            "{} bytes changed",
            asset.file_name
        );
        assert!(
            asset.data.len() > 1_024,
            "{} looks truncated",
            asset.file_name
        );
    }

    for family in [
        AppFonts::INTERFACE,
        AppFonts::DISPLAY,
        AppFonts::MONO,
        AppFonts::MONO_FALLBACK,
    ] {
        assert!(
            BUNDLED_FONT_ASSETS
                .iter()
                .any(|asset| asset.family == family),
            "{family} has no bundled font file"
        );
    }
}

#[test]
fn every_bundled_font_file_exists_on_disk() {
    for asset in BUNDLED_FONT_ASSETS.iter() {
        let path = Path::new(FONT_DIRECTORY).join(asset.file_name);
        assert!(path.is_file(), "{} is missing", path.display());
    }
}

#[test]
fn bundled_font_families_ship_their_licenses() {
    let licenses = [
        ("Archivo-OFL.txt", "The Archivo Project Authors"),
        ("IBMPlexSansThai-OFL.txt", "IBM Corp."),
        (
            "JetBrainsMono-OFL.txt",
            "The JetBrains Mono Project Authors",
        ),
        ("NotoSansThai-OFL.txt", "The Noto Project Authors"),
    ];

    for (file_name, copyright_holder) in licenses {
        let path = Path::new(FONT_DIRECTORY).join(file_name);
        let license = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} is unreadable: {error}", path.display()));

        assert!(license.contains(copyright_holder), "{file_name}");
        assert!(
            license.contains("SIL OPEN FONT LICENSE Version 1.1"),
            "{file_name}"
        );
    }
}

#[test]
fn the_bundled_mono_fallback_faces_carry_the_thai_family_and_weights() {
    let faces = [
        ("NotoSansThai-Regular.ttf", Weight::Normal),
        ("NotoSansThai-Medium.ttf", Weight::Medium),
        ("NotoSansThai-SemiBold.ttf", Weight::SemiBold),
        ("NotoSansThai-Bold.ttf", Weight::Bold),
    ];

    for (file_name, weight) in faces {
        let asset = BUNDLED_FONT_ASSETS
            .iter()
            .find(|asset| asset.file_name == file_name)
            .unwrap_or_else(|| panic!("{file_name} is not bundled"));
        assert_eq!(asset.family, AppFonts::MONO_FALLBACK);

        let face = Face::parse(asset.data, 0)
            .unwrap_or_else(|error| panic!("{file_name} does not parse: {error}"));
        assert_eq!(face.weight(), weight, "{file_name}");

        let families: Vec<_> = face
            .names()
            .into_iter()
            .filter(|name| {
                matches!(
                    name.name_id,
                    ttf_parser::name_id::FAMILY | ttf_parser::name_id::TYPOGRAPHIC_FAMILY
                )
            })
            .filter_map(|name| name.to_string())
            .collect();
        // The text system registers a face under its typographic family when
        // it has one: Medium and SemiBold carry a truncated legacy family
        // ("Noto Sans Thai Med"), so the full name has to be present too.
        assert!(
            families
                .iter()
                .any(|family| family == AppFonts::MONO_FALLBACK),
            "{file_name} names {families:?}"
        );
    }
}

#[test]
fn the_bundled_mono_fallback_faces_cover_the_thai_block() {
    // U+0E01 .. U+0E3A is the Thai consonants, vowels, tone marks and signs.
    // #137 is about these glyphs: the Latin data face has none of them, so
    // they must come from the bundled fallback.
    for file_name in [
        "NotoSansThai-Regular.ttf",
        "NotoSansThai-Medium.ttf",
        "NotoSansThai-SemiBold.ttf",
        "NotoSansThai-Bold.ttf",
    ] {
        let asset = BUNDLED_FONT_ASSETS
            .iter()
            .find(|asset| asset.file_name == file_name)
            .unwrap_or_else(|| panic!("{file_name} is not bundled"));
        let face = Face::parse(asset.data, 0)
            .unwrap_or_else(|error| panic!("{file_name} does not parse: {error}"));

        assert!(
            face.glyph_index('\u{0E01}').is_some(),
            "{file_name} has no glyph for U+0E01"
        );

        let missing: Vec<_> = ('\u{0E01}'..='\u{0E3A}')
            .filter(|code_point| face.glyph_index(*code_point).is_none())
            .map(|code_point| format!("U+{:04X}", code_point as u32))
            .collect();
        assert!(missing.is_empty(), "{file_name} misses {missing:?}");
    }
}

/// The grid's auto-width estimate assumes one 0.6 em mono cell per character
/// (`fonts::grid_char_advance`). Thai renders in the bundled fallback instead,
/// and #532 kept the estimate knowing where it is short: base letters average
/// one cell and combining marks take none, but four of them reach 0.91 em, so a
/// run of wide consonants is up to half again as wide as the estimate (0.91 em
/// against a 0.6 em cell) and clips early. This pins those facts and the bound a
/// later face may not pass.
#[test]
fn the_thai_face_advances_close_to_the_mono_cell_the_column_estimate_assumes() {
    let face_of = |file_name: &str| {
        BUNDLED_FONT_ASSETS
            .iter()
            .find(|asset| asset.file_name == file_name)
            .map(|asset| Face::parse(asset.data, 0).expect("bundled fonts parse"))
            .unwrap_or_else(|| panic!("{file_name} is not bundled"))
    };

    let advance_em = |face: &Face, code_point: char| -> Option<f32> {
        let glyph = face.glyph_index(code_point)?;
        let advance = face.glyph_hor_advance(glyph)?;
        Some(f32::from(advance) / f32::from(face.units_per_em()))
    };

    // One mono cell: the advance the grid counts for every character.
    let mono = face_of("JetBrainsMono-Regular.ttf");
    let cell = advance_em(&mono, 'A').expect("the data face has A");
    assert_eq!(cell, 0.6, "the grid counts one 0.6 em cell per character");

    let thai = face_of("NotoSansThai-Regular.ttf");
    let mut base_advances = Vec::new();
    let mut zero_advance = Vec::new();
    for code_point in ('\u{0E01}'..='\u{0E3A}').filter_map(|cp| char::from_u32(cp as u32)) {
        let Some(advance) = advance_em(&thai, code_point) else {
            continue;
        };
        if advance == 0.0 {
            zero_advance.push(code_point);
        } else {
            base_advances.push(advance);
        }
    }

    // The base letters average one cell: 0.606 em, within 1% of it.
    let mean = base_advances.iter().sum::<f32>() / base_advances.len() as f32;
    assert!(
        (mean - cell).abs() <= 0.012,
        "Thai base letters average {mean:.3} em against the {cell} em cell the \
         column width assumes"
    );

    // The combining marks take no advance, while the estimate counts each as a
    // full cell, so a mark-heavy word measures wider in the estimate than it
    // renders.
    assert!(
        !zero_advance.is_empty(),
        "the Thai face should have zero-advance combining marks"
    );

    let rendered_em = |word: &str| -> f32 {
        word.chars()
            .filter_map(|code_point| advance_em(&thai, code_point))
            .sum()
    };
    let estimated_em = |word: &str| word.chars().count() as f32 * cell;

    let mark_heavy = "สวัสดีครับ";
    assert!(
        estimated_em(mark_heavy) >= rendered_em(mark_heavy),
        "{mark_heavy:?} needs {:.3} em but the estimate gives {:.3} em",
        rendered_em(mark_heavy),
        estimated_em(mark_heavy)
    );

    // The short side of the estimate, accepted in #532: ญาณ, ณฑ, ฌาน and ฒาล
    // are runs of the four widest letters, and each measures wider than its
    // estimate. A column of them clips early.
    let widest = base_advances.iter().copied().fold(f32::MIN, f32::max);
    assert!(
        (widest - 0.91).abs() <= 0.01,
        "the widest Thai base letter advances {widest:.3} em, not the 0.91 em #532 \
         accepted; a face that changes it needs the column estimate looked at again"
    );

    for wide_word in ["ญาณ", "ณฑ", "ฌาน", "ฒาล"] {
        assert!(
            rendered_em(wide_word) > estimated_em(wide_word),
            "{wide_word:?} needs only {:.3} em against a {:.3} em estimate: the \
             wide-consonant shortfall accepted in initial_column_width is gone",
            rendered_em(wide_word),
            estimated_em(wide_word)
        );
    }
}

#[test]
fn the_data_face_itself_has_no_thai_glyphs() {
    // The reason the fallback exists, pinned so a later change cannot drop the
    // fallback while believing the data face covers Thai.
    let asset = BUNDLED_FONT_ASSETS
        .iter()
        .find(|asset| asset.file_name == "JetBrainsMono-Regular.ttf")
        .expect("the data face is bundled");
    assert_eq!(asset.family, AppFonts::MONO);

    let face = Face::parse(asset.data, 0).expect("the data face parses");
    let covered: Vec<_> = ('\u{0E01}'..='\u{0E3A}')
        .filter(|code_point| face.glyph_index(*code_point).is_some())
        .map(|code_point| format!("U+{:04X}", code_point as u32))
        .collect();
    assert!(covered.is_empty(), "the data face covers {covered:?}");
}

#[test]
fn letter_spacing_widens_shaped_text_by_one_step_per_character() {
    use gpui::{NoopTextSystem, TextSystem, WindowTextSystem, font, px};
    use std::sync::Arc;

    let text_system =
        WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(NoopTextSystem::new()))));
    let text: gpui::SharedString = "SECTION".into();
    let runs = [gpui::TextRun {
        len: text.len(),
        font: font(AppFonts::DISPLAY),
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }];

    let shape = |letter_spacing| {
        text_system
            .shape_text_with_letter_spacing(
                text.clone(),
                px(11.),
                &runs,
                None,
                None,
                letter_spacing,
            )
            .expect("text should shape")
    };

    let plain = shape(px(0.));
    let spaced = shape(px(1.5));
    let default_shape = text_system
        .shape_text(text.clone(), px(11.), &runs, None, None)
        .expect("text should shape");

    assert_eq!(default_shape[0].width(), plain[0].width());
    assert_eq!(
        spaced[0].width(),
        plain[0].width() + px(1.5) * text.chars().count() as f32
    );

    let plain_positions: Vec<_> = plain[0].unwrapped_layout.runs[0]
        .glyphs
        .iter()
        .map(|glyph| glyph.position.x)
        .collect();
    let spaced_positions: Vec<_> = spaced[0].unwrapped_layout.runs[0]
        .glyphs
        .iter()
        .map(|glyph| glyph.position.x)
        .collect();

    assert_eq!(plain_positions.len(), text.chars().count());

    for (index, (plain_x, spaced_x)) in plain_positions.iter().zip(&spaced_positions).enumerate() {
        assert_eq!(*spaced_x, *plain_x + px(1.5) * index as f32);
    }
}
