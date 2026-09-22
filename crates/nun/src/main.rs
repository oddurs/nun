//! The `nun` binary.

mod app;
mod commands;
mod session;
mod terminal;

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use app::{App, Outcome};
use commands::KeySet;
use nun_config::{Loaded, Polarity};
use nun_core::{Buffer, LoadReport};
use nun_theme::{Probe, Ramp, Rgb, Role, Source, derive, derive_with_polarity};
use nun_ui::{Capabilities, Events, Palette, Screen, install_panic_hook};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--version" | "-V") => println!("nun {VERSION}"),
        Some("theme") => print!("{}", theme(args.get(1).map(String::as_str))),
        Some("config") => print!("{}", nun_config::load().describe()),
        Some("keys") => print!("{}", commands::reference()),
        Some("--help" | "-h") | None => print!("{}", usage()),
        Some(argument) if argument.starts_with('-') => {
            eprint!("nun: unknown option `{argument}`\n\n{}", usage());
            std::process::exit(2);
        }
        Some(path) => {
            if let Err(error) = edit(Path::new(path)) {
                eprintln!("nun: {error}");
                std::process::exit(1);
            }
        }
    }
}

/// Open a file and run the editor over it.
fn edit(path: &Path) -> io::Result<()> {
    let settings = nun_config::load();

    // Installed before anything touches the terminal, the probe included, so a
    // panic anywhere after this point still puts it back.
    install_panic_hook();

    // Probed before the input reader starts: both want raw bytes from stdin,
    // and only one of them can have them.
    let startup = terminal::probe(terminal::PROBE_TIMEOUT);
    let (ramp, role_problems) = build_ramp(&startup.palette, &settings);
    let palette = Palette::new(ramp);

    // A folder opens the file tree with an empty buffer beside it; a file
    // opens the file, with the tree rooted at the folder it is in.
    let folder = path.is_dir();
    let (mut buffer, report) =
        if folder { (Buffer::new(), LoadReport::default()) } else { open(path)? };
    buffer.set_tab_width(settings.config.tab_width);

    let set = key_set(&settings, startup.kitty_keyboard);
    let (keymap, key_problems) = commands::keymap(set, &settings.config.keys);
    let problems = [role_problems, key_problems].concat();

    let mut app = App::new(buffer, palette, keymap);
    if let Some(path) = session::Session::default_path() {
        app.attach_session(session::Session::load(path));
    }
    if let Some(ms) = settings.config.double_click_ms {
        app.set_double_click(std::time::Duration::from_millis(ms));
    }
    // Only the first is shown: the rest are visible through `nun config`, and a
    // queue of config complaints would bury the editor under them.
    if let Some(warning) = warnings(report, &settings, &problems).into_iter().next() {
        app.warn(warning);
    }
    if let Some(notice) = key_set_notice(&settings, startup.kitty_keyboard) {
        app.warn(notice);
    }

    let mut screen = Screen::open(capabilities(&settings, set)).map_err(|error| {
        // The usual cause is no tty at all — piped input, or a CI runner — and
        // the platform's own message for that is "Device not configured".
        io::Error::new(error.kind(), format!("nun needs an interactive terminal ({error})"))
    })?;
    let events = Events::start()?;

    // Everything the file tree does happens off this thread and comes back
    // through the same channel as the keyboard, so the main thread stays the
    // only thing that touches the tree.
    let sender = events.sender();
    app.open_folder(
        workspace_root(path, folder),
        nun_workspace::trash_or_temp(),
        folder,
        Box::new(move |done| {
            let _ = sender.send(nun_ui::Event::Workspace(done));
        }),
    );

    // Parsing runs on its own thread and reports back through the same
    // channel as everything else.
    let sender = events.sender();
    app.attach_syntax(nun_syntax::Worker::new(Box::new(move |reply| {
        let _ = sender.send(nun_ui::Event::Syntax(reply));
    })));

    // Searching the project has a thread of its own rather than sharing the
    // tree's: a search of a large repository would otherwise sit in front of
    // the directory listings the tree is waiting on.
    let sender = events.sender();
    app.attach_search(nun_workspace::Grep::new(Box::new(move |found| {
        let _ = sender.send(nun_ui::Event::Found(found));
    })));

    let sender = events.sender();
    match nun_workspace::Watcher::new(Box::new(move |change| {
        let _ = sender.send(nun_ui::Event::Files { dir: change.dir, error: change.watch_error });
    })) {
        Ok(watcher) => app.attach_watcher(watcher),
        Err(error) => app.warn(format!("The file tree will not update on its own: {error}")),
    }

    app.set_viewport(screen.area()?);
    screen.draw(AppView(&app))?;

    loop {
        // Sleep until input arrives or something pending falls due, whichever
        // is first. With nothing pending there is no deadline and no wakeup.
        let mut outcome = match events.next_before(app.deadline()) {
            Some(event) => app.handle(event),
            None => app.tick(Instant::now()),
        };
        // Take the whole burst before drawing, so holding a key down costs one
        // frame rather than one frame per repeat.
        for event in events.drain() {
            outcome = outcome.and(app.handle(event));
        }

        match outcome {
            Outcome::Quit => break,
            Outcome::Suspend => {
                screen.suspend()?;
                app.set_viewport(screen.area()?);
                screen.draw(AppView(&app))?;
            }
            Outcome::Redraw => {
                app.set_viewport(screen.area()?);
                screen.draw(AppView(&app))?;
            }
            Outcome::Continue => {}
        }

        // Any-motion reporting only while something on screen reacts to hover.
        // A failure here costs hover, not the session.
        let _ = screen.track_motion(app.wants_motion());
    }

    screen.close();
    // After the terminal is back, so a failure can be said where it is seen.
    // Losing it costs the folds, and nothing else.
    if let Err(error) = app.save_session() {
        eprintln!("nun: could not remember this session's folds: {error}");
    }
    Ok(())
}

/// Open the path, or say clearly why not.
///
/// Every failure names the path.
pub(crate) fn open(path: &Path) -> io::Result<(Buffer, LoadReport)> {
    let shown = path.display();

    if path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            format!("{shown} is a directory, not a file."),
        ));
    }

    if !path.exists() {
        // A path that does not exist yet is a new file, not an error — but only
        // if there is somewhere to put it.
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty())
            && !parent.is_dir()
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} does not exist, so {shown} cannot be created", parent.display()),
            ));
        }
        let mut buffer = Buffer::new();
        buffer.set_path(path);
        return Ok((buffer, LoadReport::default()));
    }

    Buffer::load(path).map_err(|error| {
        let explanation = match error.kind() {
            io::ErrorKind::PermissionDenied => format!("{shown}: permission denied"),
            _ => format!("{shown}: {error}"),
        };
        io::Error::new(error.kind(), explanation)
    })
}

/// Derive the ramp, then apply any role overrides from the config.
fn build_ramp(probe: &Probe, settings: &Loaded) -> (Ramp, Vec<String>) {
    let mut ramp = match settings.config.polarity {
        Polarity::Auto => derive(probe),
        Polarity::Dark => derive_with_polarity(probe, nun_theme::Polarity::Dark),
        Polarity::Light => derive_with_polarity(probe, nun_theme::Polarity::Light),
    };

    let mut problems = Vec::new();
    for (key, value) in &settings.config.roles {
        match (Role::from_key(key), Rgb::from_hex(value)) {
            (Some(role), Some(color)) => ramp.set(role, color),
            (None, _) => problems.push(format!("theme.roles: no role called `{key}`")),
            (_, None) => {
                problems.push(format!("theme.roles.{key}: `{value}` is not a #rrggbb colour"));
            }
        }
    }
    (ramp, problems)
}

/// The folder the file tree is rooted at.
///
/// A folder argument is its own root. A file is shown in the folder it is in,
/// which is where the rest of its project is.
fn workspace_root(path: &Path, folder: bool) -> PathBuf {
    let root = if folder {
        path.to_path_buf()
    } else {
        let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty());
        parent.map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    };
    // Spelled out in full, so the sidebar is headed by the folder's name
    // rather than by `.`, and so watching and revealing compare like for like.
    root.canonicalize().unwrap_or(root)
}

/// Which key set to use: the full one only when the config allows it and the
/// terminal said it speaks the protocol.
fn key_set(settings: &Loaded, kitty_keyboard: Option<bool>) -> KeySet {
    if settings.config.keyboard_enhancement && kitty_keyboard == Some(true) {
        KeySet::Full
    } else {
        KeySet::Basic
    }
}

/// What to say, once, about falling back to the basic key set.
///
/// It says what was actually found out: a terminal that said no, and one that
/// did not answer in time, are different problems with different fixes.
/// Nothing when the user turned the protocol off themselves: they know.
fn key_set_notice(settings: &Loaded, kitty_keyboard: Option<bool>) -> Option<String> {
    if !settings.config.keyboard_enhancement {
        return None;
    }
    let why = match kitty_keyboard {
        Some(true) => return None,
        Some(false) => "This terminal has no Kitty keyboard protocol",
        None => "The terminal did not say whether it has the Kitty keyboard protocol",
    };
    Some(format!("{why}, so nun is using the basic key set. `nun keys` lists it (docs/keys.md)."))
}

/// Which terminal features to turn on.
///
/// The keyboard flags are pushed only when the terminal said it understands
/// them. Pushing them anyway would be harmless on most terminals, but "most"
/// is an assumption, and capabilities here are detected, never assumed.
fn capabilities(settings: &Loaded, set: KeySet) -> Capabilities {
    Capabilities {
        alternate_screen: settings.config.alternate_screen,
        mouse: settings.config.mouse,
        keyboard_enhancement: set == KeySet::Full,
        hide_cursor: true,
    }
}

/// Anything worth telling the user once, in priority order.
fn warnings(report: LoadReport, settings: &Loaded, role_problems: &[String]) -> Vec<String> {
    let mut warnings = Vec::new();
    if report.lossy {
        warnings.push(
            "This file is not valid UTF-8. Saving it would destroy the original bytes.".into(),
        );
    }
    for problem in &settings.problems {
        warnings.push(format!("Config: {problem}"));
    }
    for problem in role_problems {
        warnings.push(format!("Config: {problem}"));
    }
    if report.mixed_line_endings {
        warnings.push("Mixed line endings; saving normalises them to the dominant one.".into());
    }
    warnings
}

/// Adapts the editor to ratatui's widget trait.
pub(crate) struct AppView<'a>(&'a App);

impl Widget for AppView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        self.0.render(area, cells);
    }
}

/// Probe the terminal and print what was derived from it.
fn theme(subcommand: Option<&str>) -> String {
    match subcommand {
        Some("dump") => {
            let startup = terminal::probe(terminal::PROBE_TIMEOUT);
            let probe = startup.palette;
            let keyboard = match startup.kitty_keyboard {
                Some(true) => "Kitty keyboard protocol: yes; the full key set is used",
                Some(false) => "Kitty keyboard protocol: no; the basic key set is used",
                None => "Kitty keyboard protocol: no answer in time; the basic key set is used",
            };
            let source = match probe.source {
                Source::Terminal => "probed from this terminal",
                Source::ColorFgBg => "no reply; polarity taken from COLORFGBG",
                Source::Builtin => "no reply; nun's built-in neutrals",
            };
            format!(
                "# {source}\n# {keyboard}\n# background {}  foreground {}\n{}",
                probe.background.to_hex(),
                probe.foreground.to_hex(),
                derive(&probe).to_toml()
            )
        }
        _ => "nun theme: expected `dump`\n\nUsage: nun theme dump\n".to_string(),
    }
}

fn usage() -> String {
    format!(
        "nun {VERSION}\n\
         A mouse-first terminal code editor.\n\n\
         Usage: nun <file>\n       nun <folder>\n       nun config\n       nun keys\n       nun theme dump\n\n\
         Options:\n  \
           -h, --help     Print help\n  \
           -V, --version  Print version\n\n\
         Commands:\n  \
           config         Print the effective configuration and where it came from\n  \
           keys           List every command and the keys bound to it\n  \
           theme dump     Probe this terminal and print the derived ramp as TOML\n\n\
         Keys:\n  \
           Ctrl+S save   Ctrl+Z undo   Ctrl+Y redo   Ctrl+A select all   Ctrl+Q quit\n  \
           `nun keys` lists them all, and the Cmd bindings a Kitty-protocol terminal adds.\n  \
           Click places the caret; the wheel scrolls.\n  \
           Double-click a word, triple-click a line, drag to extend by either.\n  \
           Shift-click extends; Alt-click adds a caret; Alt-drag selects a column.\n  \
           Drag a selection to move it, with Ctrl held at the drop to copy.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_without_a_subcommand_says_what_it_expected() {
        let out = theme(None);
        assert!(out.contains("expected `dump`"));
        assert!(out.contains("Usage: nun theme dump"));
    }

    #[test]
    fn usage_documents_the_commands_and_the_mouse() {
        let usage = usage();
        assert!(usage.contains("nun theme dump"));
        assert!(usage.contains("Click places the caret"));
    }

    #[test]
    fn opening_a_directory_as_a_file_says_so_without_an_errno() {
        let error = open(Path::new(".")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("is a directory"), "{message}");
        assert!(!message.contains("os error"), "an errno is not an explanation: {message}");
    }

    #[test]
    fn a_file_roots_the_tree_at_the_folder_it_is_in() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        std::fs::create_dir(real.join("src")).unwrap();
        std::fs::write(real.join("src/main.rs"), "").unwrap();

        assert_eq!(workspace_root(&real.join("src/main.rs"), false), real.join("src"));
        assert_eq!(workspace_root(&real, true), real);
        assert_eq!(
            workspace_root(Path::new("notes.txt"), false),
            Path::new(".").canonicalize().unwrap(),
            "a bare file name means the folder nun was started in"
        );
        // A folder that is not there keeps the name it was given, so the
        // error the tree shows names what the user typed.
        assert_eq!(workspace_root(Path::new("nowhere"), true), PathBuf::from("nowhere"));
    }

    #[test]
    fn opening_a_missing_file_in_a_real_directory_starts_a_new_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.txt");
        let (buffer, _) = open(&path).expect("a new file is not an error");
        assert_eq!(buffer.path(), Some(path.as_path()));
        assert_eq!(buffer.len_chars(), 0);
    }

    #[test]
    fn opening_a_file_in_a_directory_that_does_not_exist_says_which_part_is_missing() {
        let error = open(Path::new("/nope/nowhere/x.txt")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("/nope/nowhere does not exist"), "{message}");
        assert!(message.contains("x.txt"), "{message}");
    }

    #[test]
    fn an_unreadable_file_names_itself() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.txt");
        std::fs::write(&path, b"x").unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o000))
            .unwrap();

        let message = open(&path).unwrap_err().to_string();
        assert!(message.contains("secret.txt"), "{message}");
        assert!(message.contains("permission denied"), "{message}");
    }

    #[test]
    fn role_overrides_are_applied_and_bad_ones_reported() {
        let mut settings = Loaded::defaults();
        settings.config.roles.insert("accent".into(), "#e0a44b".into());
        settings.config.roles.insert("nonsense".into(), "#000000".into());
        settings.config.roles.insert("error".into(), "not-a-colour".into());

        let (ramp, problems) = build_ramp(&Probe::builtin_dark(), &settings);

        assert_eq!(ramp.get(Role::Accent), Rgb::new(0xe0, 0xa4, 0x4b));
        assert_eq!(problems.len(), 2);
        assert!(problems.iter().any(|p| p.contains("no role called `nonsense`")));
        assert!(problems.iter().any(|p| p.contains("not a #rrggbb colour")));
    }

    #[test]
    fn forcing_a_polarity_overrides_what_the_background_implies() {
        let mut settings = Loaded::defaults();
        settings.config.polarity = Polarity::Light;
        let (ramp, _) = build_ramp(&Probe::builtin_dark(), &settings);
        assert_eq!(ramp.polarity(), nun_theme::Polarity::Light);
    }

    #[test]
    fn capabilities_follow_the_config() {
        let mut settings = Loaded::defaults();
        settings.config.mouse = false;
        settings.config.keyboard_enhancement = false;

        let capabilities = capabilities(&settings, key_set(&settings, Some(true)));
        assert!(!capabilities.mouse);
        assert!(!capabilities.keyboard_enhancement);
        assert!(capabilities.alternate_screen, "untouched settings keep their default");
    }

    #[test]
    fn the_full_key_set_needs_the_terminal_and_the_config_to_agree() {
        let settings = Loaded::defaults();
        assert_eq!(key_set(&settings, Some(true)), KeySet::Full);
        assert_eq!(key_set(&settings, Some(false)), KeySet::Basic);
        assert_eq!(key_set(&settings, None), KeySet::Basic, "detected, never assumed");

        let mut off = Loaded::defaults();
        off.config.keyboard_enhancement = false;
        assert_eq!(key_set(&off, Some(true)), KeySet::Basic, "the user turned it off");
    }

    #[test]
    fn keyboard_flags_are_only_pushed_to_a_terminal_that_said_it_understands_them() {
        let settings = Loaded::defaults();
        assert!(capabilities(&settings, KeySet::Full).keyboard_enhancement);
        assert!(!capabilities(&settings, KeySet::Basic).keyboard_enhancement);
    }

    #[test]
    fn falling_back_is_announced_once_and_points_at_the_list() {
        let settings = Loaded::defaults();
        let notice = key_set_notice(&settings, Some(false)).unwrap();
        assert!(notice.contains("has no Kitty keyboard protocol"), "{notice}");
        assert!(notice.contains("basic key set"), "{notice}");
        assert!(notice.contains("nun keys"), "the notice links to the degraded set: {notice}");
        assert_eq!(key_set_notice(&settings, Some(true)), None);

        let silent = key_set_notice(&settings, None).unwrap();
        assert!(silent.contains("did not say"), "silence is not a no: {silent}");

        let mut off = Loaded::defaults();
        off.config.keyboard_enhancement = false;
        assert_eq!(key_set_notice(&off, Some(false)), None, "they chose it; nothing to say");
    }

    #[test]
    fn a_lossy_file_outranks_a_config_complaint_in_the_status_line() {
        let mut settings = Loaded::defaults();
        settings
            .problems
            .push(nun_config::Problem { path: "nun.toml".into(), message: "something".into() });
        let report = LoadReport { lossy: true, ..LoadReport::default() };

        let warnings = warnings(report, &settings, &[]);
        assert!(warnings[0].contains("not valid UTF-8"), "{warnings:?}");
        assert_eq!(warnings.len(), 2, "the config problem is still reported, just second");
    }

    #[test]
    fn a_clean_load_with_no_config_warns_about_nothing() {
        assert!(warnings(LoadReport::default(), &Loaded::defaults(), &[]).is_empty());
    }

    #[test]
    fn the_strongest_outcome_of_a_burst_wins() {
        assert_eq!(Outcome::Continue.and(Outcome::Redraw), Outcome::Redraw);
        assert_eq!(Outcome::Redraw.and(Outcome::Quit), Outcome::Quit);
        assert_eq!(Outcome::Suspend.and(Outcome::Redraw), Outcome::Suspend);
        assert_eq!(Outcome::Continue.and(Outcome::Continue), Outcome::Continue);
    }
}
