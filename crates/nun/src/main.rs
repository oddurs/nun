//! The `nun` binary.

mod app;
mod terminal;

use std::io;
use std::path::Path;
use std::time::Instant;

use app::{App, Outcome};
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

    // Probed before the input reader starts: both want raw bytes from stdin,
    // and only one of them can have them. A keystroke landing inside the probe
    // window is dropped, which is a real if small cost of doing it this way.
    let probe = terminal::probe_palette(terminal::PROBE_TIMEOUT);
    let (ramp, role_problems) = build_ramp(&probe, &settings);
    let palette = Palette::new(ramp);

    let (mut buffer, report) = open(path)?;
    buffer.set_tab_width(settings.config.tab_width);

    let mut app = App::new(buffer, palette);
    // Only the first is shown: the status line holds one message, and the rest
    // are visible through `nun config`.
    if let Some(warning) = warnings(report, &settings, &role_problems).into_iter().next() {
        app.warn(warning);
    }

    // Installed before the screen is entered, so a panic anywhere after this
    // point still puts the terminal back.
    install_panic_hook();

    let mut screen = Screen::open(capabilities(&settings)).map_err(|error| {
        // The usual cause is no tty at all — piped input, or a CI runner — and
        // the platform's own message for that is "Device not configured".
        io::Error::new(error.kind(), format!("nun needs an interactive terminal ({error})"))
    })?;
    let events = Events::start()?;

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
            outcome = combine(outcome, app.handle(event));
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
    Ok(())
}

/// Open the path, or say clearly why not.
///
/// Every failure names the path. A directory gets a real answer rather than an
/// errno, because `nun .` is the first thing anyone types.
fn open(path: &Path) -> io::Result<(Buffer, LoadReport)> {
    let shown = path.display();

    if path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            format!(
                "{shown} is a directory. nun opens one file at a time for now — \
                 the file tree and the project palette are milestone 2.\n\
                 Try `nun {shown}/<file>`."
            ),
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

/// Which terminal features to turn on.
fn capabilities(settings: &Loaded) -> Capabilities {
    Capabilities {
        alternate_screen: settings.config.alternate_screen,
        mouse: settings.config.mouse,
        keyboard_enhancement: settings.config.keyboard_enhancement,
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

/// The strongest outcome of a burst wins.
const fn combine(a: Outcome, b: Outcome) -> Outcome {
    match (a, b) {
        (Outcome::Quit, _) | (_, Outcome::Quit) => Outcome::Quit,
        (Outcome::Suspend, _) | (_, Outcome::Suspend) => Outcome::Suspend,
        (Outcome::Redraw, _) | (_, Outcome::Redraw) => Outcome::Redraw,
        _ => Outcome::Continue,
    }
}

/// Adapts the editor to ratatui's widget trait.
struct AppView<'a>(&'a App);

impl Widget for AppView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        self.0.render(area, cells);
    }
}

/// Probe the terminal and print what was derived from it.
fn theme(subcommand: Option<&str>) -> String {
    match subcommand {
        Some("dump") => {
            let probe = terminal::probe_palette(terminal::PROBE_TIMEOUT);
            let source = match probe.source {
                Source::Terminal => "probed from this terminal",
                Source::ColorFgBg => "no reply; polarity taken from COLORFGBG",
                Source::Builtin => "no reply; nun's built-in neutrals",
            };
            format!(
                "# {source}\n# background {}  foreground {}\n{}",
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
         Usage: nun <file>\n       nun config\n       nun theme dump\n\n\
         Options:\n  \
           -h, --help     Print help\n  \
           -V, --version  Print version\n\n\
         Commands:\n  \
           config         Print the effective configuration and where it came from\n  \
           theme dump     Probe this terminal and print the derived ramp as TOML\n\n\
         Keys:\n  \
           Ctrl+S save   Ctrl+Z undo   Ctrl+Shift+Z redo   Ctrl+A select all   Ctrl+Q quit\n  \
           Click places the caret; the wheel scrolls.\n"
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
    fn opening_a_directory_explains_rather_than_reporting_an_errno() {
        let error = open(Path::new(".")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("is a directory"), "{message}");
        assert!(message.contains("milestone 2"), "it must say why: {message}");
        assert!(message.contains("Try `nun"), "and what to do instead: {message}");
        assert!(!message.contains("os error"), "an errno is not an explanation: {message}");
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

        let capabilities = capabilities(&settings);
        assert!(!capabilities.mouse);
        assert!(!capabilities.keyboard_enhancement);
        assert!(capabilities.alternate_screen, "untouched settings keep their default");
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
        assert_eq!(combine(Outcome::Continue, Outcome::Redraw), Outcome::Redraw);
        assert_eq!(combine(Outcome::Redraw, Outcome::Quit), Outcome::Quit);
        assert_eq!(combine(Outcome::Suspend, Outcome::Redraw), Outcome::Suspend);
        assert_eq!(combine(Outcome::Continue, Outcome::Continue), Outcome::Continue);
    }
}
