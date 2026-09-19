//! Loading, merging, and surviving a bad file.

use std::fs;

use nun_config::{Config, Loaded, Origin, Polarity, apply_file};

fn load_text(text: &str) -> (Loaded, std::path::PathBuf) {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let path = dir.path().join("nun.toml");
    fs::write(&path, text).unwrap();

    let mut loaded = Loaded::defaults();
    apply_file(&mut loaded, &path);
    (loaded, path)
}

#[test]
fn no_file_at_all_is_a_supported_configuration() {
    let mut loaded = Loaded::defaults();
    apply_file(&mut loaded, std::path::Path::new("/nonexistent/nun.toml"));

    assert_eq!(loaded.config, Config::default());
    assert!(loaded.problems.is_empty(), "a missing file is not a problem");
}

#[test]
fn an_empty_file_changes_nothing() {
    let (loaded, _) = load_text("");
    assert_eq!(loaded.config, Config::default());
    assert!(loaded.problems.is_empty());
}

#[test]
fn a_file_only_overrides_what_it_mentions() {
    let (loaded, path) = load_text("[editor]\ntab_width = 2\n");

    assert_eq!(loaded.config.tab_width, 2);
    assert_eq!(loaded.config.mouse, Config::default().mouse, "untouched keys keep their default");
    assert_eq!(loaded.origin("tab_width"), Origin::File(path));
    assert_eq!(loaded.origin("mouse"), Origin::Default);
}

#[test]
fn every_setting_can_be_overridden() {
    let (loaded, _) = load_text(
        r##"
        [editor]
        tab_width = 8

        [theme]
        polarity = "dark"

        [theme.roles]
        accent = "#e0a44b"

        [ui]
        mouse = false
        alternate_screen = false
        keyboard_enhancement = false
        "##,
    );

    assert_eq!(loaded.config.tab_width, 8);
    assert_eq!(loaded.config.polarity, Polarity::Dark);
    assert_eq!(loaded.config.roles.get("accent").map(String::as_str), Some("#e0a44b"));
    assert!(!loaded.config.mouse);
    assert!(!loaded.config.alternate_screen);
    assert!(!loaded.config.keyboard_enhancement);
    assert!(loaded.problems.is_empty());
}

// ── a bad file must cost one setting, not the editor ────────────────────────

#[test]
fn a_syntax_error_names_the_line_and_leaves_the_defaults_standing() {
    let (loaded, _) = load_text("[editor]\ntab_width = = 4\n");

    assert_eq!(loaded.config, Config::default(), "nothing was applied");
    assert_eq!(loaded.problems.len(), 1);
    let message = &loaded.problems[0].message;
    assert!(
        message.contains("line 2") || message.contains("expected"),
        "the message must point at the problem: {message}"
    );
}

#[test]
fn an_unknown_key_is_reported_rather_than_silently_ignored() {
    let (loaded, _) = load_text("[editor]\ntab_widht = 2\n");

    assert_eq!(loaded.problems.len(), 1);
    assert!(
        loaded.problems[0].message.contains("tab_widht"),
        "a typo must name itself: {}",
        loaded.problems[0].message
    );
}

#[test]
fn an_unknown_section_is_reported() {
    let (loaded, _) = load_text("[editr]\ntab_width = 2\n");
    assert_eq!(loaded.problems.len(), 1);
    assert!(loaded.problems[0].message.contains("editr"));
}

#[test]
fn an_out_of_range_value_is_refused_and_says_the_range() {
    let (loaded, _) = load_text("[editor]\ntab_width = 0\n");

    assert_eq!(loaded.config.tab_width, Config::default().tab_width, "the default stood");
    assert_eq!(loaded.problems.len(), 1);
    assert!(loaded.problems[0].message.contains("between 1 and 16"));
}

#[test]
fn a_wrong_type_is_reported_not_coerced() {
    let (loaded, _) = load_text("[ui]\nmouse = \"yes\"\n");
    assert_eq!(loaded.config.mouse, Config::default().mouse);
    assert_eq!(loaded.problems.len(), 1);
}

#[test]
fn one_bad_section_does_not_discard_a_good_one() {
    // toml parses the whole document, so a type error anywhere rejects the
    // file. That is the honest behaviour to document: the problem names the
    // line, and every default stands.
    let (loaded, _) = load_text("[editor]\ntab_width = 2\n\n[ui]\nmouse = 3\n");
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.problems.len(), 1);
}

// ── nun config ──────────────────────────────────────────────────────────────

#[test]
fn describe_annotates_only_what_came_from_a_file() {
    let (loaded, path) = load_text("[editor]\ntab_width = 2\n");
    let described = loaded.describe();

    let tab_line = described.lines().find(|line| line.starts_with("tab_width")).unwrap();
    assert!(tab_line.contains(&path.display().to_string()), "{tab_line}");

    let mouse_line = described.lines().find(|line| line.starts_with("mouse")).unwrap();
    assert!(!mouse_line.contains('#'), "a default needs no annotation: {mouse_line}");
}

#[test]
fn describe_covers_every_setting() {
    let described = Loaded::defaults().describe();
    for key in ["tab_width", "polarity", "mouse", "alternate_screen", "keyboard_enhancement"] {
        assert!(described.contains(key), "{key} missing from `nun config`");
    }
}

#[test]
fn describe_surfaces_problems() {
    let (loaded, _) = load_text("[editor]\ntab_widht = 2\n");
    assert!(loaded.describe().contains("# problems"));
}

#[test]
fn describe_round_trips_as_valid_toml() {
    // What `nun config` prints should be pasteable back into nun.toml.
    let (loaded, _) = load_text("[editor]\ntab_width = 3\n\n[theme.roles]\naccent = \"#e0a44b\"\n");
    let described = loaded.describe();
    toml::from_str::<toml::Value>(&described).unwrap_or_else(|error| {
        panic!("`nun config` printed invalid TOML: {error}\n\n{described}")
    });
}

#[test]
fn the_user_path_follows_xdg_when_it_is_set() {
    // Checked through the public helper rather than by setting process-wide
    // environment variables, which would race other tests.
    let path = nun_config::user_config_path().expect("HOME is always set in a test run");
    assert!(path.ends_with("nun/nun.toml"), "{}", path.display());
}

#[test]
fn key_bindings_are_read_as_written() {
    let (loaded, _) =
        load_text("[keys]\n\"ctrl+k ctrl+s\" = \"file.save\"\n\"cmd+p\" = \"palette.files\"\n");
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert_eq!(loaded.config.keys.get("ctrl+k ctrl+s").map(String::as_str), Some("file.save"));
    assert_eq!(loaded.config.keys.len(), 2);
}

#[test]
fn key_bindings_from_a_later_file_add_to_an_earlier_one() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("a.toml");
    let second = dir.path().join("b.toml");
    std::fs::write(&first, "[keys]\n\"ctrl+1\" = \"file.save\"\n").unwrap();
    std::fs::write(&second, "[keys]\n\"ctrl+2\" = \"edit.undo\"\n").unwrap();

    let mut loaded = Loaded::defaults();
    apply_file(&mut loaded, &first);
    apply_file(&mut loaded, &second);
    assert_eq!(loaded.config.keys.len(), 2);
}

#[test]
fn describe_lists_the_key_bindings() {
    let (loaded, _) = load_text("[keys]\n\"ctrl+k ctrl+s\" = \"file.save\"\n");
    assert!(loaded.describe().contains("\"ctrl+k ctrl+s\" = \"file.save\""));
    assert!(Loaded::defaults().describe().contains("nun keys"));
}
