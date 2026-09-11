//! The `nun` binary.

mod app;
mod terminal;

use std::io;
use std::path::Path;

use app::{App, Outcome};
use nun_core::Buffer;
use nun_theme::{Source, derive};
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
    // Probed before the input reader starts: both want raw bytes from stdin,
    // and only one of them can have them. A keystroke landing inside the probe
    // window is dropped, which is a real if small cost of doing it this way.
    let probe = terminal::probe_palette(terminal::PROBE_TIMEOUT);
    let palette = Palette::new(derive(&probe));

    let (buffer, report) = if path.exists() {
        Buffer::load(path)?
    } else {
        let mut buffer = Buffer::new();
        buffer.set_path(path);
        (buffer, nun_core::LoadReport::default())
    };

    let mut app = App::new(buffer, palette);
    if report.lossy {
        app.warn("This file is not valid UTF-8. Saving it would destroy the original bytes.");
    } else if report.mixed_line_endings {
        app.warn("Mixed line endings; saving normalises them to the dominant one.");
    }

    // Installed before the screen is entered, so a panic anywhere after this
    // point still puts the terminal back.
    install_panic_hook();

    let mut screen = Screen::open(Capabilities::default()).map_err(|error| {
        // The usual cause is no tty at all — piped input, or a CI runner — and
        // the platform's own message for that is "Device not configured".
        io::Error::new(error.kind(), format!("nun needs an interactive terminal ({error})"))
    })?;
    let events = Events::start()?;

    app.set_viewport(screen.area()?);
    screen.draw(AppView(&app))?;

    loop {
        // Take the whole burst before drawing, so holding a key down costs one
        // frame rather than one frame per repeat.
        let mut outcome = app.handle(events.next());
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
    }

    screen.close();
    Ok(())
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
         Usage: nun <file>\n       nun theme dump\n\n\
         Options:\n  \
           -h, --help     Print help\n  \
           -V, --version  Print version\n\n\
         Commands:\n  \
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
    fn the_strongest_outcome_of_a_burst_wins() {
        assert_eq!(combine(Outcome::Continue, Outcome::Redraw), Outcome::Redraw);
        assert_eq!(combine(Outcome::Redraw, Outcome::Quit), Outcome::Quit);
        assert_eq!(combine(Outcome::Suspend, Outcome::Redraw), Outcome::Suspend);
        assert_eq!(combine(Outcome::Continue, Outcome::Continue), Outcome::Continue);
    }
}
