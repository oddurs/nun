use std::path::{Path, PathBuf};

use super::*;

fn user(text: &str) -> File {
    File::parse(Path::new("/home/nun.toml"), Layer::User, text, None)
}

fn project(text: &str) -> File {
    File::parse(Path::new("/p/.nun.toml"), Layer::Project, text, None)
}

fn trusting(files: &Files) -> TrustStore {
    let mut store = TrustStore::in_memory();
    let file = files.project.as_ref().unwrap();
    store.remember(PathBuf::from("/p"), Decision::Trust, trust::fingerprint(file));
    store
}

#[test]
fn zero_config_is_the_defaults() {
    let loaded = resolve(&Files::default(), &TrustStore::in_memory());
    assert_eq!(loaded.config, Config::default());
    assert!(loaded.origins.is_empty());
    assert!(loaded.problems.is_empty());
}

#[test]
fn the_user_file_overrides_the_defaults_and_says_where() {
    let files = Files {
        user: Some(user("[editor]\ntab_width = 2\n\n[ui]\nmouse = false\n")),
        project: None,
    };
    let loaded = resolve(&files, &TrustStore::in_memory());
    assert_eq!(loaded.config.tab_width, 2);
    assert!(!loaded.config.mouse);
    assert_eq!(
        loaded.origin("ui.mouse"),
        Origin::File { layer: Layer::User, path: "/home/nun.toml".into(), line: 5 }
    );
    assert_eq!(loaded.origin("ui.hyperlinks"), Origin::Default);
}

#[test]
fn an_untrusted_project_is_inert_and_says_what_it_would_change() {
    let files = Files {
        user: Some(user("[editor]\ntab_width = 8\n")),
        project: Some(project("[editor]\ntab_width = 2\n[lsp.rust]\ncommand = \"./evil\"\n")),
    };
    let loaded = resolve(&files, &TrustStore::in_memory());
    assert_eq!(loaded.config.tab_width, 8);
    assert_eq!(loaded.config.lsp["rust"].command, "rust-analyzer");
    let project = loaded.project.as_ref().unwrap();
    assert_eq!(project.trust, Trust::Unknown);
    assert!(project.asks());
    assert_eq!(project.would_change(), ["editor.tab_width = 2", "lsp.rust.command = \"./evil\""]);
}

#[test]
fn a_trusted_project_applies_over_the_user() {
    let files = Files {
        user: Some(user("[editor]\ntab_width = 8\n")),
        project: Some(project("[editor]\ntab_width = 2\n[lsp.rust]\nformat_on_save = false\n")),
    };
    let loaded = resolve(&files, &trusting(&files));
    assert_eq!(loaded.config.tab_width, 2);
    assert!(!loaded.config.lsp["rust"].format_on_save);
    assert_eq!(loaded.origin("lsp.rust.format_on_save").layer(), Some(Layer::Project));
    assert!(!loaded.project.as_ref().unwrap().asks());
}

#[test]
fn a_changed_risky_setting_waits_and_the_harmless_ones_still_apply() {
    let before = Files { user: None, project: Some(project("[lsp.rust]\ncommand = \"ra\"\n")) };
    let store = trusting(&before);
    let after = Files {
        user: None,
        project: Some(project("[editor]\ntab_width = 3\n[lsp.rust]\ncommand = \"./evil\"\n")),
    };
    let loaded = resolve(&after, &store);
    assert_eq!(loaded.project.as_ref().unwrap().trust, Trust::Changed);
    assert_eq!(loaded.config.tab_width, 3, "harmless, and the directory is trusted");
    assert_eq!(loaded.config.lsp["rust"].command, "rust-analyzer", "risky: waits");
    assert_eq!(loaded.project.as_ref().unwrap().would_change(), ["lsp.rust.command = \"./evil\""]);
}

#[test]
fn a_new_language_needs_a_command() {
    let files = Files {
        user: Some(user(
            "[lsp.zig]\nargs = [\"x\"]\n[lsp.go]\nargs = [\"serve\"]\n[lsp.nim]\ncommand = \"nimlsp\"\n",
        )),
        project: None,
    };
    let loaded = resolve(&files, &TrustStore::in_memory());
    assert!(!loaded.config.lsp.contains_key("zig"));
    assert_eq!(loaded.config.lsp["go"].command, "gopls", "args alone keep the command");
    assert_eq!(loaded.config.lsp["go"].args, ["serve"]);
    assert_eq!(loaded.config.lsp["nim"].command, "nimlsp");
    assert_eq!(loaded.problems.len(), 1);
    assert_eq!(loaded.problems[0].line, Some(2));
    assert!(loaded.set_by_a_file("lsp.go"));
    assert!(!loaded.set_by_a_file("lsp.rust"));
}

#[test]
fn maps_are_added_to_layer_by_layer() {
    let files = Files {
        user: Some(user(
            "[keys]\n\"ctrl+.\" = \"palette\"\n[glyphs]\nfold.open = \"v\"\n[theme.roles]\naccent = \"#e0a44b\"\n",
        )),
        project: None,
    };
    let loaded = resolve(&files, &TrustStore::in_memory());
    assert_eq!(loaded.config.keys["ctrl+."], "palette");
    assert_eq!(loaded.config.glyphs["fold.open"], "v");
    assert_eq!(loaded.config.roles["accent"], "#e0a44b");
}

#[test]
fn describe_names_the_layer_and_line_of_each_value() {
    let files = Files {
        user: Some(user("[ui]\nmouse = false\n")),
        project: Some(project("[editor]\ntab_width = 2\n")),
    };
    let loaded = resolve(&files, &trusting(&files));
    let out = loaded.describe(None);
    assert!(out.contains("mouse = false    # user: /home/nun.toml:2"), "{out}");
    assert!(out.contains("tab_width = 2    # project: /p/.nun.toml:2"), "{out}");
    assert!(out.contains("project       /p/.nun.toml (trusted)"), "{out}");
    assert!(out.contains("hyperlinks = true\n"), "a default has no note: {out}");
}

#[test]
fn describe_lists_what_an_untrusted_project_would_set() {
    let files = Files { user: None, project: Some(project("[editor]\ntab_width = 2\n")) };
    let out = resolve(&files, &TrustStore::in_memory()).describe(None);
    assert!(out.contains("not trusted yet"), "{out}");
    assert!(out.contains("would set, if trusted:\n# editor.tab_width = 2"), "{out}");
    assert!(out.contains("tab_width = 4\n"), "{out}");
}

#[test]
fn explain_walks_the_layers_and_marks_the_winner() {
    let files = Files {
        user: Some(user("[editor]\ntab_width = 8\n")),
        project: Some(project("[editor]\ntab_width = 2\n")),
    };
    let loaded = resolve(&files, &TrustStore::in_memory());
    let configs = [EditorConfig::parse(Path::new("/p/.editorconfig"), "[*.rs]\ntab_width = 3\n")];
    let out = loaded.explain("editor.tab_width", Some((Path::new("/p/a.rs"), &configs)));
    assert!(out.starts_with("editor.tab_width = 3\n"), "{out}");
    assert!(out.contains("default"), "{out}");
    assert!(out.contains("user: /home/nun.toml:2"), "{out}");
    assert!(out.contains("→ 3                editorconfig: /p/.editorconfig:2"), "{out}");
    assert!(out.contains("not used: this project's settings are not trusted yet"), "{out}");
    assert!(out.contains("Who may set it: your nun.toml, or a trusted project's"), "{out}");
}

#[test]
fn explain_a_personal_setting_and_a_typo() {
    let loaded = resolve(
        &Files { user: Some(user("[ui]\nmouse = false\n")), project: None },
        &TrustStore::in_memory(),
    );
    let out = loaded.explain("ui.mouse", None);
    assert!(out.starts_with("ui.mouse = false\n"), "{out}");
    assert!(out.contains("(overridden)"), "{out}");
    assert!(out.contains("only your own nun.toml"), "{out}");

    let lsp = loaded.explain("lsp.rust.command", None);
    assert!(lsp.starts_with("lsp.rust.command = \"rust-analyzer\""), "{lsp}");
    assert!(lsp.contains("asks for trust again"), "{lsp}");

    assert!(loaded.explain("ui.mosue", None).contains("Did you mean `ui.mouse`?"));
}

#[test]
fn a_project_is_found_up_to_the_repository() {
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(top.join("repo/.git")).unwrap();
    std::fs::create_dir_all(top.join("repo/src/deep")).unwrap();
    std::fs::write(top.join(".nun.toml"), "").unwrap();
    assert_eq!(find_project(&top.join("repo/src/deep")), None, "not above the repository");
    std::fs::write(top.join("repo/.nun.toml"), "").unwrap();
    assert_eq!(find_project(&top.join("repo/src/deep")), Some(top.join("repo/.nun.toml")));
}

#[test]
fn reading_again_keeps_what_a_broken_file_had() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nun.toml");
    let sources = Sources { user: Some(path.clone()), project: None };
    std::fs::write(&path, "[editor]\ntab_width = 2\n").unwrap();
    let good = Files::read(&sources, &Files::default());
    std::fs::write(&path, "[editor\ntab_width = 3\n").unwrap();
    let broken = Files::read(&sources, &good);
    let loaded = resolve(&broken, &TrustStore::in_memory());
    assert_eq!(loaded.config.tab_width, 2);
    assert_eq!(loaded.problems.len(), 1);
    assert_eq!(loaded.problems[0].line, Some(1));
}
