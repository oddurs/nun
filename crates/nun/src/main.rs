//! The `nun` binary.
//!
//! Today this is the entry point and nothing more: the editor itself is being
//! built milestone by milestone, tracked in `cairn/`.

mod terminal;

use nun_theme::{Source, derive};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    print!("{}", run(&args));
}

/// Render the response to a set of command-line arguments.
///
/// Kept separate from `main` so the surface stays testable without a terminal.
fn run(args: &[String]) -> String {
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => format!("nun {VERSION}\n"),
        Some("theme") => theme(args.get(1).map(String::as_str)),
        Some("--help" | "-h") | None => usage(),
        Some(other) => format!("nun: unknown argument `{other}`\n\n{}", usage()),
    }
}

fn usage() -> String {
    format!(
        "nun {VERSION}\n\
         A mouse-first terminal code editor.\n\n\
         Usage: nun [OPTIONS]\n       nun theme dump\n\n\
         Options:\n  \
           -h, --help     Print help\n  \
           -V, --version  Print version\n\n\
         Commands:\n  \
           theme dump     Probe this terminal and print the derived ramp as TOML\n"
    )
}

/// Probe the terminal and print what was derived from it.
///
/// Printed as TOML so it can be pasted straight into `nun.toml` and edited,
/// which is the escape hatch for anyone who wants to pin a role rather than
/// inherit it.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn version_flag_reports_the_package_version() {
        assert_eq!(run(&args(&["--version"])), format!("nun {VERSION}\n"));
        assert_eq!(run(&args(&["-V"])), format!("nun {VERSION}\n"));
    }

    #[test]
    fn no_arguments_prints_usage() {
        assert!(run(&[]).contains("Usage: nun"));
    }

    #[test]
    fn theme_without_a_subcommand_says_what_it_expected() {
        // Deliberately does not touch the terminal, so it is safe in CI.
        let out = run(&args(&["theme"]));
        assert!(out.contains("expected `dump`"));
        assert!(out.contains("Usage: nun theme dump"));
    }

    #[test]
    fn usage_mentions_the_theme_command() {
        assert!(usage().contains("nun theme dump"));
    }

    #[test]
    fn unknown_argument_is_named_and_followed_by_usage() {
        let out = run(&args(&["--frobnicate"]));
        assert!(out.contains("unknown argument `--frobnicate`"));
        assert!(out.contains("Usage: nun"));
    }
}
