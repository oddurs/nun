//! The `nun` binary.
//!
//! Today this is the entry point and nothing more: the editor itself is being
//! built milestone by milestone, tracked in `cairn/`.

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
        Some("--help" | "-h") | None => usage(),
        Some(other) => format!("nun: unknown argument `{other}`\n\n{}", usage()),
    }
}

fn usage() -> String {
    format!(
        "nun {VERSION}\n\
         A mouse-first terminal code editor.\n\n\
         Usage: nun [OPTIONS]\n\n\
         Options:\n  \
           -h, --help     Print help\n  \
           -V, --version  Print version\n"
    )
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
    fn unknown_argument_is_named_and_followed_by_usage() {
        let out = run(&args(&["--frobnicate"]));
        assert!(out.contains("unknown argument `--frobnicate`"));
        assert!(out.contains("Usage: nun"));
    }
}
