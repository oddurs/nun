//! Derivation against real terminal palettes, and against pathological ones.
//!
//! The guarantee nun makes is not "the colours look nice" — that depends on the
//! terminal — but "whatever you configured, the result is legible". These tests
//! pin that down.

use nun_theme::{Ansi, Polarity, Probe, Rgb, Role, Source, contrast_ratio, derive};

/// Build a probe from hex strings, the way a real reply would arrive.
fn probe(background: &str, foreground: &str, palette: [&str; 16]) -> Probe {
    Probe {
        background: Rgb::from_hex(background).unwrap(),
        foreground: Rgb::from_hex(foreground).unwrap(),
        cursor: None,
        palette: palette.map(|hex| Rgb::from_hex(hex).unwrap()),
        source: Source::Terminal,
    }
}

fn solarized_dark() -> Probe {
    probe(
        "002b36",
        "839496",
        [
            "073642", "dc322f", "859900", "b58900", "268bd2", "d33682", "2aa198", "eee8d5",
            "002b36", "cb4b16", "586e75", "657b83", "839496", "6c71c4", "93a1a1", "fdf6e3",
        ],
    )
}

fn solarized_light() -> Probe {
    probe(
        "fdf6e3",
        "657b83",
        [
            "073642", "dc322f", "859900", "b58900", "268bd2", "d33682", "2aa198", "eee8d5",
            "002b36", "cb4b16", "586e75", "657b83", "839496", "6c71c4", "93a1a1", "fdf6e3",
        ],
    )
}

fn gruvbox_dark() -> Probe {
    probe(
        "282828",
        "ebdbb2",
        [
            "282828", "cc241d", "98971a", "d79921", "458588", "b16286", "689d6a", "a89984",
            "928374", "fb4934", "b8bb26", "fabd2f", "83a598", "d3869b", "8ec07c", "ebdbb2",
        ],
    )
}

fn nord() -> Probe {
    probe(
        "2e3440",
        "d8dee9",
        [
            "3b4252", "bf616a", "a3be8c", "ebcb8b", "81a1c1", "b48ead", "88c0d0", "e5e9f0",
            "4c566a", "bf616a", "a3be8c", "ebcb8b", "81a1c1", "b48ead", "8fbcbb", "eceff4",
        ],
    )
}

fn dracula() -> Probe {
    probe(
        "282a36",
        "f8f8f2",
        [
            "21222c", "ff5555", "50fa7b", "f1fa8c", "bd93f9", "ff79c6", "8be9fd", "f8f8f2",
            "6272a4", "ff6e6e", "69ff94", "ffffa5", "d6acff", "ff92df", "a4ffff", "ffffff",
        ],
    )
}

fn github_light() -> Probe {
    probe(
        "ffffff",
        "24292f",
        [
            "24292f", "cf222e", "116329", "4d2d00", "0969da", "8250df", "1b7c83", "6e7781",
            "57606a", "a40e26", "1a7f37", "633c01", "218bff", "a475f9", "3192aa", "8c959f",
        ],
    )
}

/// Every palette nun is expected to behave well on.
fn corpus() -> Vec<(&'static str, Probe)> {
    vec![
        ("solarized dark", solarized_dark()),
        ("solarized light", solarized_light()),
        ("gruvbox dark", gruvbox_dark()),
        ("nord", nord()),
        ("dracula", dracula()),
        ("github light", github_light()),
        ("builtin dark", Probe::builtin_dark()),
        ("builtin light", Probe::builtin_light()),
    ]
}

#[test]
fn polarity_follows_the_background() {
    for (name, probe) in corpus() {
        let expected = if name.contains("light") { Polarity::Light } else { Polarity::Dark };
        assert_eq!(derive(&probe).polarity(), expected, "{name}");
    }
}

#[test]
fn the_ground_is_exactly_what_the_terminal_reported() {
    for (name, probe) in corpus() {
        assert_eq!(derive(&probe).get(Role::Ground), probe.background, "{name}");
    }
}

#[test]
fn every_foreground_role_clears_its_contrast_floor() {
    // Floors mirror the ones derivation enforces. Text is WCAG AA; faint is
    // deliberately below it but still has to be visible.
    let requirements = [
        (Role::Text, 4.5),
        (Role::Dim, 3.0),
        (Role::Faint, 2.2),
        (Role::Accent, 3.0),
        (Role::Error, 3.0),
        (Role::Warn, 3.0),
        (Role::Info, 3.0),
        (Role::Added, 3.0),
        (Role::Removed, 3.0),
        (Role::Keyword, 3.0),
        (Role::Type, 3.0),
        (Role::StringLiteral, 3.0),
        (Role::Number, 3.0),
        (Role::Function, 3.0),
        (Role::Comment, 2.2),
        (Role::Punctuation, 3.0),
    ];

    for (name, probe) in corpus() {
        let ramp = derive(&probe);
        let ground = ramp.get(Role::Ground);
        for (role, minimum) in requirements {
            let ratio = contrast_ratio(ramp.get(role), ground);
            assert!(
                ratio >= minimum - 0.01,
                "{name}: {role:?} is {ratio:.2}:1 against the ground, below {minimum}"
            );
        }
    }
}

#[test]
fn surfaces_are_distinguishable_from_the_ground_but_not_loud() {
    for (name, probe) in corpus() {
        let ramp = derive(&probe);
        let ground = ramp.get(Role::Ground);
        for role in [Role::Raised, Role::Overlay, Role::Sunken] {
            let ratio = contrast_ratio(ramp.get(role), ground);
            assert!(ratio > 1.0, "{name}: {role:?} is identical to the ground");
            assert!(ratio < 3.0, "{name}: {role:?} at {ratio:.2}:1 reads as a different theme");
        }
        assert_ne!(ramp.get(Role::Raised), ground, "{name}: raised must be visible");
    }
}

#[test]
fn overlay_sits_further_from_the_ground_than_raised() {
    for (name, probe) in corpus() {
        let ramp = derive(&probe);
        let ground = ramp.get(Role::Ground);
        let raised = contrast_ratio(ramp.get(Role::Raised), ground);
        let overlay = contrast_ratio(ramp.get(Role::Overlay), ground);
        assert!(overlay > raised, "{name}: a floating surface must read as further forward");
    }
}

#[test]
fn tabstops_are_washes_the_selection_still_reads_over() {
    for (name, probe) in corpus() {
        let ramp = derive(&probe);
        let ground = ramp.get(Role::Ground);
        let [other, current, selection] = [Role::Tabstop, Role::TabstopCurrent, Role::Selection]
            .map(|role| contrast_ratio(ramp.get(role), ground));
        assert!(other > 1.0, "{name}: a tab-stop is invisible");
        assert!(current > other, "{name}: the current stop must stand out from the rest");
        assert!(selection > current, "{name}: a selection must read over a stop");
    }
}

#[test]
fn text_on_the_accent_is_legible() {
    for (name, probe) in corpus() {
        let ramp = derive(&probe);
        let ratio = contrast_ratio(ramp.get(Role::OnAccent), ramp.get(Role::Accent));
        assert!(ratio >= 4.5, "{name}: text on the accent is only {ratio:.2}:1");
    }
}

// ── palettes that would break a naive derivation ────────────────────────────

#[test]
fn a_near_monochrome_palette_still_yields_distinguishable_roles() {
    // Someone who has desaturated their terminal to greys still needs to be
    // able to tell an error from a string.
    let grey = probe(
        "1a1a1a",
        "c8c8c8",
        [
            "2a2a2a", "6e6e6e", "747474", "7a7a7a", "707070", "767676", "727272", "c0c0c0",
            "4a4a4a", "8e8e8e", "949494", "9a9a9a", "909090", "969696", "929292", "e0e0e0",
        ],
    );
    let ramp = derive(&grey);
    let ground = ramp.get(Role::Ground);

    for role in [Role::Text, Role::Error, Role::Accent, Role::StringLiteral] {
        assert!(
            contrast_ratio(ramp.get(role), ground) >= 3.0,
            "{role:?} vanished into a monochrome ground"
        );
    }
    assert_ne!(
        ramp.get(Role::Accent),
        ramp.get(Role::Text),
        "the accent must not collapse into body text"
    );
}

#[test]
fn a_palette_with_no_contrast_at_all_is_forced_apart() {
    // Mid grey on mid grey: faithful reproduction would be unreadable.
    let flat = probe("808080", "858585", ["808080"; 16]);
    let ramp = derive(&flat);
    let ratio = contrast_ratio(ramp.get(Role::Text), ramp.get(Role::Ground));
    assert!(ratio >= 4.5, "text was left at {ratio:.2}:1 on an unreadable palette");
}

#[test]
fn pure_black_and_pure_white_grounds_still_have_somewhere_to_go() {
    for (name, background, foreground) in [
        ("pure black", Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)),
        ("pure white", Rgb::new(255, 255, 255), Rgb::new(0, 0, 0)),
    ] {
        let p = Probe { background, foreground, ..Probe::builtin_dark() };
        let ramp = derive(&p);
        assert_ne!(
            ramp.get(Role::Raised),
            ramp.get(Role::Ground),
            "{name}: a raised surface had nowhere to move"
        );
        assert_ne!(ramp.get(Role::Overlay), ramp.get(Role::Raised), "{name}: overlay collapsed");
    }
}

#[test]
fn a_desaturated_blue_still_produces_a_usable_accent() {
    let mut p = Probe::builtin_dark();
    p.palette[Ansi::Blue as usize] = Rgb::new(0x70, 0x72, 0x74); // blue, but grey
    let ramp = derive(&p);
    let accent = ramp.get(Role::Accent);
    let chroma = nun_theme::Oklch::from(accent).c;
    assert!(
        chroma >= 0.10,
        "the chroma floor did not rescue a greyed-out blue: {}",
        accent.to_hex()
    );
}

// ── the escape hatch ────────────────────────────────────────────────────────

#[test]
fn one_role_can_be_overridden_without_losing_the_rest() {
    let mut ramp = derive(&Probe::builtin_dark());
    let before = ramp.get(Role::Text);
    let amber = Rgb::new(0xe0, 0xa4, 0x4b);

    ramp.set(Role::Accent, amber);

    assert_eq!(ramp.get(Role::Accent), amber);
    assert_eq!(ramp.get(Role::Text), before, "overriding one role left the others derived");
}

#[test]
fn roles_round_trip_through_their_config_keys() {
    for role in Role::ALL {
        assert_eq!(Role::from_key(role.key()), Some(role), "{role:?} key did not round-trip");
    }
    assert_eq!(Role::from_key("not_a_role"), None);
}

#[test]
fn the_dump_is_valid_toml_covering_every_role() {
    let toml = derive(&Probe::builtin_dark()).to_toml();
    assert!(toml.starts_with("[theme.roles]\n"));
    for role in Role::ALL {
        assert!(toml.contains(&format!("{} = \"#", role.key())), "{role:?} missing from the dump");
    }
    assert_eq!(toml.lines().count(), Role::ALL.len() + 1);
}
