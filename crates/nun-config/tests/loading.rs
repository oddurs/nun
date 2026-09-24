//! Loading, merging, and surviving a bad file.

use std::fs;

use nun_config::{Config, Files, Layer, Loaded, Origin, Polarity, Sources, TrustStore};

fn load_text(text: &str) -> (Loaded, std::path::PathBuf) {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let path = dir.path().join("nun.toml");
    fs::write(&path, text).unwrap();

    (load_path(&path), path)
}

fn load_path(path: &std::path::Path) -> Loaded {
    let sources = Sources { user: Some(path.to_path_buf()), project: None };
    nun_config::resolve(&Files::read(&sources, &Files::default()), &TrustStore::in_memory())
}

/// Whether `key` came from line `line` of the user's file at `path`.
fn from_user(loaded: &Loaded, key: &str, path: &std::path::Path) -> bool {
    matches!(loaded.origin(key), Origin::File { layer: Layer::User, path: from, .. } if from == path)
}

#[test]
fn no_file_at_all_is_a_supported_configuration() {
    let loaded = load_path(std::path::Path::new("/nonexistent/nun.toml"));

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
    assert!(from_user(&loaded, "editor.tab_width", &path));
    assert_eq!(loaded.origin("ui.mouse"), Origin::Default);
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
        undercurl = "on"
        "##,
    );

    assert_eq!(loaded.config.tab_width, 8);
    assert_eq!(loaded.config.polarity, Polarity::Dark);
    assert_eq!(loaded.config.roles.get("accent").map(String::as_str), Some("#e0a44b"));
    assert!(!loaded.config.mouse);
    assert!(!loaded.config.alternate_screen);
    assert!(!loaded.config.keyboard_enhancement);
    assert_eq!(loaded.config.undercurl, nun_config::Undercurl::On);
    assert!(loaded.problems.is_empty());
}

// ── a bad file must cost one setting, not the editor ────────────────────────

#[test]
fn a_syntax_error_names_the_line_and_leaves_the_defaults_standing() {
    let (loaded, _) = load_text("[editor]\ntab_width = = 4\n");

    assert_eq!(loaded.config, Config::default(), "nothing was applied");
    assert_eq!(loaded.problems.len(), 1);
    assert_eq!(loaded.problems[0].line, Some(2), "the problem names its line");
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
fn one_bad_value_does_not_discard_the_good_ones() {
    let (loaded, _) = load_text("[editor]\ntab_width = 2\n\n[ui]\nmouse = 3\n");
    assert_eq!(loaded.config.tab_width, 2, "the good value applies");
    assert_eq!(loaded.config.mouse, Config::default().mouse);
    assert_eq!(loaded.problems.len(), 1);
    assert_eq!(loaded.problems[0].line, Some(5));
}

// ── nun config ──────────────────────────────────────────────────────────────

#[test]
fn describe_annotates_only_what_came_from_a_file() {
    let (loaded, path) = load_text("[editor]\ntab_width = 2\n");
    let described = loaded.describe(None);

    let tab_line = described.lines().find(|line| line.starts_with("tab_width")).unwrap();
    assert!(tab_line.contains(&path.display().to_string()), "{tab_line}");

    let mouse_line = described.lines().find(|line| line.starts_with("mouse")).unwrap();
    assert!(!mouse_line.contains('#'), "a default needs no annotation: {mouse_line}");
}

#[test]
fn describe_covers_every_setting() {
    let described = Loaded::defaults().describe(None);
    for key in
        ["tab_width", "polarity", "mouse", "alternate_screen", "keyboard_enhancement", "undercurl"]
    {
        assert!(described.contains(key), "{key} missing from `nun config`");
    }
}

#[test]
fn describe_surfaces_problems() {
    let (loaded, _) = load_text("[editor]\ntab_widht = 2\n");
    assert!(loaded.describe(None).contains("# problems"));
}

#[test]
fn describe_round_trips_as_valid_toml() {
    // What `nun config` prints should be pasteable back into nun.toml.
    let (loaded, _) = load_text("[editor]\ntab_width = 3\n\n[theme.roles]\naccent = \"#e0a44b\"\n");
    let described = loaded.describe(None);
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
fn describe_lists_the_key_bindings() {
    let (loaded, _) = load_text("[keys]\n\"ctrl+k ctrl+s\" = \"file.save\"\n");
    assert!(loaded.describe(None).contains("\"ctrl+k ctrl+s\" = \"file.save\""));
    assert!(Loaded::defaults().describe(None).contains("nun keys"));
}

#[test]
fn the_double_click_threshold_can_be_set_within_reason() {
    let (loaded, _) = load_text("[ui]\ndouble_click_ms = 350\n");
    assert_eq!(loaded.config.double_click_ms, Some(350));

    let (loaded, _) = load_text("[ui]\ndouble_click_ms = 5\n");
    assert_eq!(loaded.config.double_click_ms, None, "falls back to the platform value");
    assert!(loaded.problems[0].message.contains("between 100 and 2000"));
}

#[test]
fn rust_analyzer_is_the_default_rust_server() {
    let config = Config::default();
    let rust = config.lsp.get("rust").expect("rust has a default server");
    assert_eq!(rust.command, "rust-analyzer");
    assert!(rust.enabled);
    assert!(config.lsp.contains_key("python"), "defaults cover more than one language");
}

#[test]
fn a_language_server_can_be_changed_one_field_at_a_time() {
    let (loaded, path) = load_text("[lsp.rust]\nargs = [\"--log-file\", \"/tmp/ra.log\"]\n");
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    let rust = &loaded.config.lsp["rust"];
    assert_eq!(rust.command, "rust-analyzer", "the command was not mentioned, so it stays");
    assert_eq!(rust.args, ["--log-file", "/tmp/ra.log"]);
    assert!(from_user(&loaded, "lsp.rust.args", &path));
    assert!(!loaded.set_by_a_file("lsp.python"));
}

#[test]
fn a_language_server_can_be_turned_off_or_added() {
    let (loaded, _) = load_text("[lsp.python]\nenabled = false\n\n[lsp.zig]\ncommand = \"zls\"\n");
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert!(!loaded.config.lsp["python"].enabled);
    assert_eq!(loaded.config.lsp["zig"].command, "zls");
    assert!(loaded.config.lsp["zig"].args.is_empty());
}

#[test]
fn a_new_language_server_without_a_command_is_reported() {
    let (loaded, _) = load_text("[lsp.zig]\nargs = [\"x\"]\n");
    assert_eq!(loaded.problems.len(), 1);
    assert!(
        loaded.problems[0].message.contains("lsp.zig needs a command"),
        "{:?}",
        loaded.problems
    );
    assert!(!loaded.config.lsp.contains_key("zig"));
}

#[test]
fn an_empty_server_command_is_refused_with_the_way_to_turn_it_off() {
    let (loaded, _) = load_text("[lsp.rust]\ncommand = \"\"\n");
    assert!(loaded.problems[0].message.contains("enabled = false"), "{:?}", loaded.problems);
    assert_eq!(loaded.config.lsp["rust"].command, "rust-analyzer", "the default stands");
}

#[test]
fn an_unknown_key_in_a_server_section_is_reported() {
    let (loaded, _) = load_text("[lsp.rust]\ncomand = \"ra\"\n");
    assert_eq!(loaded.problems.len(), 1, "{:?}", loaded.problems);
}

#[test]
fn describe_lists_the_language_servers_and_stays_valid_toml() {
    let (loaded, _) = load_text("[lsp.python]\nenabled = false\n");
    let described = loaded.describe(None);
    assert!(described.contains("[lsp.rust]\ncommand = \"rust-analyzer\""), "{described}");
    assert!(described.contains("enabled = false"), "{described}");
    toml::from_str::<toml::Value>(&described).unwrap_or_else(|error| {
        panic!("`nun config` printed invalid TOML: {error}\n\n{described}")
    });
}

#[test]
fn format_on_save_is_on_only_where_one_formatter_is_the_standard() {
    let config = Config::default();
    let on: Vec<&str> = config
        .lsp
        .iter()
        .filter(|(_, server)| server.format_on_save)
        .map(|(language, _)| language.as_str())
        .collect();
    assert_eq!(on, ["go", "rust"], "rustfmt and gofmt; every other formatter is a choice");
}

#[test]
fn format_on_save_is_set_per_language_and_keeps_the_server() {
    let (loaded, _) =
        load_text("[lsp.rust]\nformat_on_save = false\n\n[lsp.python]\nformat_on_save = true\n");
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert!(!loaded.config.lsp["rust"].format_on_save);
    assert_eq!(loaded.config.lsp["rust"].command, "rust-analyzer");
    assert!(loaded.config.lsp["python"].format_on_save);
    assert!(loaded.config.lsp["go"].format_on_save, "a language not mentioned keeps its default");
    assert!(loaded.describe(None).contains("[lsp.python]"), "and it says so");
    assert!(loaded.describe(None).contains("format_on_save = true"));
}

#[test]
fn the_hover_delay_can_be_set_within_reason_and_links_left_unmarked() {
    let (loaded, _) = load_text("[ui]\nhover_delay_ms = 250\nhyperlinks = false\n");
    assert_eq!(loaded.config.hover_delay_ms, 250);
    assert!(!loaded.config.hyperlinks);
    assert!(loaded.describe(None).contains("hover_delay_ms = 250"));

    let (loaded, _) = load_text("[ui]\nhover_delay_ms = 5\n");
    assert_eq!(loaded.config.hover_delay_ms, 400, "keeps the default");
    assert!(loaded.problems.iter().any(|problem| problem.message.contains("hover_delay_ms")));
}

#[test]
fn the_code_action_mark_is_on_unless_turned_off() {
    let (loaded, _) = load_text("");
    assert!(loaded.config.lightbulb);

    let (loaded, _) = load_text("[ui]\nlightbulb = false\n");
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert!(!loaded.config.lightbulb);
    assert!(loaded.describe(None).contains("lightbulb = false"));
}

#[test]
fn glyphs_start_from_the_default_preset_with_nothing_changed() {
    let config = Config::default();
    assert_eq!(config.glyph_preset, "default");
    assert!(config.glyphs.is_empty());
}

#[test]
fn a_glyph_role_reads_the_same_dotted_quoted_or_as_a_section() {
    for text in [
        "[glyphs]\nfold.open = \"v\"\n",
        "[glyphs]\n\"fold.open\" = \"v\"\n",
        "[glyphs.fold]\nopen = \"v\"\n",
    ] {
        let (loaded, path) = load_text(text);
        assert!(loaded.problems.is_empty(), "{text}: {:?}", loaded.problems);
        assert_eq!(loaded.config.glyphs.get("fold.open").map(String::as_str), Some("v"), "{text}");
        assert!(from_user(&loaded, "glyphs.fold.open", &path), "{text}");
    }
}

#[test]
fn the_glyph_preset_is_taken_out_of_the_roles() {
    let (loaded, path) =
        load_text("[glyphs]\npreset = \"ascii\"\ntab.close = \"x\"\nrail.1 = \".\"\n");
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert_eq!(loaded.config.glyph_preset, "ascii");
    assert!(from_user(&loaded, "glyphs.preset", &path));
    assert_eq!(loaded.config.glyphs.len(), 2, "{:?}", loaded.config.glyphs);
    assert_eq!(loaded.config.glyphs.get("rail.1").map(String::as_str), Some("."));
}

#[test]
fn a_glyph_that_is_not_a_string_is_reported_and_the_rest_kept() {
    let (loaded, _) = load_text("[glyphs]\nlightbulb = 1\ntab.close = \"x\"\n");
    assert_eq!(loaded.problems.len(), 1, "{:?}", loaded.problems);
    assert!(loaded.problems[0].message.contains("glyphs.lightbulb must be a string"));
    assert_eq!(loaded.config.glyphs.get("tab.close").map(String::as_str), Some("x"));
}

#[test]
fn describe_lists_the_glyphs_and_stays_valid_toml() {
    let (loaded, path) = load_text("[glyphs]\npreset = \"ascii\"\nfold.open = \"▿\"\n");
    let described = loaded.describe(None);
    assert!(described.contains("[glyphs]\npreset = \"ascii\""), "{described}");
    let line = described.lines().find(|line| line.starts_with("\"fold.open\"")).unwrap();
    assert!(line.contains("\"▿\"") && line.contains(&path.display().to_string()), "{line}");
    toml::from_str::<toml::Value>(&described).unwrap_or_else(|error| {
        panic!("`nun config` printed invalid TOML: {error}\n\n{described}")
    });
    assert!(Loaded::defaults().describe(None).contains("nun glyphs"));
}

#[test]
fn a_glyph_role_set_twice_under_two_spellings_is_reported() {
    let (loaded, _) = load_text("[glyphs]\nfold.open = \"a\"\n\"fold.open\" = \"b\"\n");
    assert_eq!(loaded.problems.len(), 1, "{:?}", loaded.problems);
    assert!(loaded.problems[0].message.contains("glyphs.fold.open is set twice"));
}

#[test]
fn describe_writes_a_combining_glyph_as_toml_can_read_it_back() {
    let (loaded, _) = load_text("[glyphs]\nfold.open = \"e\\u0301\"\nlightbulb = \"\\\"\"\n");
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    let described = loaded.describe(None);
    let back: toml::Value = toml::from_str(&described).unwrap_or_else(|error| {
        panic!("`nun config` printed invalid TOML: {error}\n\n{described}")
    });
    assert_eq!(back["glyphs"]["fold.open"].as_str(), Some("e\u{301}"));
    assert_eq!(back["glyphs"]["lightbulb"].as_str(), Some("\""));
}

#[test]
fn describe_for_a_file_stays_valid_toml() {
    let (loaded, _) = load_text("[editor]\nindent_style = \"space\"\n");
    let dir = tempfile::tempdir().unwrap();
    let config = nun_config::EditorConfig::parse(
        &dir.path().join(".editorconfig"),
        "[*]\nend_of_line = crlf\ncharset = utf-8-bom\ninsert_final_newline = true\n",
    );
    let file = dir.path().join("a.rs");
    let whitespace = nun_config::Whitespace::resolve(&loaded, &file, &[config]);
    let described = loaded.describe(Some((&file, &whitespace)));
    assert!(described.contains("end_of_line = \"crlf\""), "{described}");
    assert!(described.contains("indent_style = \"space\""), "{described}");
    toml::from_str::<toml::Value>(&described).unwrap_or_else(|error| {
        panic!("`nun config <file>` printed invalid TOML: {error}\n\n{described}")
    });
}
