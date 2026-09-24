//! Copying text to the clipboard of the machine the person is sitting at.
//!
//! There are two ways there, and neither is always right. A clipboard
//! program — pbcopy, wl-copy, xclip, xsel — says whether it took the text,
//! but it fills the clipboard of the machine nun runs on: over ssh that is
//! the wrong machine, or one with no clipboard at all. OSC 52 asks the
//! terminal nun is drawn in to take the text, which reaches the right machine
//! however many hops away it is; but the terminal never says whether it did,
//! many refuse it, and most cannot say beforehand whether they will.
//!
//! So `ui.clipboard` decides. `system` uses only a program, `osc52` only the
//! terminal, and `auto` — the default — uses a program unless nun is running
//! over ssh or none takes the text, and then the terminal, but only one that
//! has said it accepts OSC 52. What can be known about that:
//!
//! - **Parameter 52 in the device attributes.** kitty 0.43, Ghostty 1.2,
//!   foot 1.23, iTerm2 3.6.6 and Windows Terminal 1.22 list it, each only
//!   while its settings allow a program to write the clipboard, so it is
//!   believed. The start-up probe already asks for the attributes.
//! - **Not XTGETTCAP `Ms`.** It says a terminal knows the sequence, not that
//!   it lets a program use it: kitty answers it with writes turned off.
//! - **tmux** never advertises it, but when nun runs inside it, it can be
//!   asked. With `set-clipboard on` it takes OSC 52 from a program and passes
//!   it on to its terminal. With `external`, its default, it drops OSC 52
//!   from a program without a word — but `tmux load-buffer -w` fills its
//!   buffer and passes the text on all the same. With `off` it passes on
//!   nothing a program sends; `load-buffer -w` would still get through, but
//!   in `auto` nun takes `off` to mean a copy is not wanted there.
//! - **tmux on the far side of ssh** — nun on a remote machine, inside ssh,
//!   inside a tmux on the person's own — answers the version query as tmux
//!   but cannot be asked anything: `$TMUX` is not set where nun runs. Plain
//!   OSC 52 reaches that terminal's clipboard only with `set-clipboard on`,
//!   so `auto` does not send it and says so, and `osc52` sends it and says
//!   what it depends on.
//! - Alacritty, xterm (where it is off unless allowed), stable `WezTerm` and
//!   iTerm2 before 3.6.6 accept it and cannot say so, which is what the
//!   `osc52` setting is for. Terminal.app has no OSC 52.
//!
//! `$TMUX` is believed only where the version query did not contradict it:
//! a terminal started from a tmux pane inherits it. And tmux keeps a pane's
//! environment from when the pane was made, so under tmux whether nun is
//! over ssh is asked of tmux, which updates `SSH_CONNECTION` when a client
//! attaches.
//!
//! A copy that went by OSC 52 is said to have been sent, never to have been
//! copied: nothing comes back to say it arrived. A copy a clipboard program
//! took over ssh is said to be on the remote machine, since it is. Copies
//! are made one at a time, in order, on a thread of their own, so a later
//! copy never loses to an earlier one that took longer.
//!
//! A program in the terminal panel can copy too, with OSC 52 of its own, and
//! its copy goes the same way a selection does: the panel is a terminal, and
//! a terminal lets its programs copy.
//!
//! The text is capped at [`MOST`] bytes. Base64 makes it a third bigger, which
//! keeps it under xterm's 600,000-byte limit on one string and tmux's 1 MiB
//! input buffer; a terminal given more drops the lot. A longer copy is refused
//! rather than cut short, since half a copy pasted is worse than none, and the
//! message says why.

use nun_config::Clipboard;
use nun_ui::CopyOutcome;

/// The most text, in bytes of UTF-8, sent through OSC 52.
pub const MOST: usize = 400_000;

/// Where nun is running, as far as a copy is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
// Four facts about the place, each found out on its own.
#[allow(clippy::struct_excessive_bools)]
pub struct Place {
    /// The terminal said it accepts OSC 52.
    pub advertised: bool,
    /// nun is inside tmux, which can be asked what it does with a copy.
    pub tmux: bool,
    /// nun is drawn in a tmux it cannot ask: one on the far side of ssh.
    pub far_tmux: bool,
    /// nun is running over ssh, so a clipboard program here fills the wrong
    /// machine's clipboard.
    pub remote: bool,
}

impl Place {
    /// The place, from whether the terminal `advertised` OSC 52, the name
    /// it gave in answer to the version query, and the environment
    /// variables `set` says are there.
    pub fn new(advertised: bool, version: Option<&str>, set: impl Fn(&str) -> bool) -> Self {
        let said_tmux = version.map(|name| name.starts_with("tmux"));
        let tmux = set("TMUX") && said_tmux != Some(false);
        Self {
            advertised,
            tmux,
            far_tmux: !tmux && said_tmux == Some(true),
            remote: set("SSH_TTY") || set("SSH_CONNECTION"),
        }
    }
}

/// One way to the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// A clipboard program on this machine.
    System,
    /// OSC 52, straight to the terminal.
    Terminal,
    /// Through tmux, in whichever way its `set-clipboard` allows.
    Tmux,
}

/// What a copy needs from the world: running programs, which the tests
/// stand in for.
pub trait World {
    /// Give `text` to the first clipboard program that takes it.
    ///
    /// # Errors
    ///
    /// When none did, saying which were tried.
    fn system(&self, text: &str) -> Result<(), String>;

    /// tmux's `set-clipboard`, if it will say.
    fn tmux_setting(&self) -> Option<String>;

    /// Whether the client attached to tmux came in over ssh, if tmux will
    /// say.
    fn tmux_remote(&self) -> Option<bool>;

    /// Put `text` in tmux's buffer and have tmux pass it on to the
    /// terminal of the client attached; whether tmux took it.
    fn tmux_load(&self, text: &str) -> bool;
}

/// The routes to try, in order, for `setting` in `place`.
#[must_use]
pub fn routes(setting: Clipboard, place: Place) -> Vec<Route> {
    let escape = if place.tmux { Route::Tmux } else { Route::Terminal };
    match setting {
        Clipboard::System => vec![Route::System],
        Clipboard::Osc52 => vec![escape],
        Clipboard::Auto => {
            // tmux can be asked; a terminal only counts if it said so.
            let known = place.tmux || place.advertised;
            match (known, place.remote) {
                (false, _) => vec![Route::System],
                (true, true) => vec![escape, Route::System],
                (true, false) => vec![Route::System, escape],
            }
        }
    }
}

/// Copy `text` by the first route that takes it, and say how that went.
pub fn copy(text: &str, setting: Clipboard, mut place: Place, world: &impl World) -> CopyOutcome {
    if place.tmux
        && let Some(remote) = world.tmux_remote()
    {
        place.remote = remote;
    }
    let mut problems = Vec::new();
    for route in routes(setting, place) {
        let tried = match route {
            Route::System => world.system(text).map(|()| {
                if place.remote {
                    CopyOutcome::Sent(
                        "the clipboard of the machine nun runs on, not yours: it is over ssh"
                            .to_string(),
                    )
                } else {
                    CopyOutcome::Taken
                }
            }),
            Route::Terminal => fits(text).map(|()| CopyOutcome::Escape {
                bytes: osc52(text),
                to: terminal_note(place).to_string(),
            }),
            Route::Tmux => through_tmux(text, world, setting == Clipboard::Osc52),
        };
        match tried {
            Ok(outcome) => return outcome,
            Err(problem) => problems.push(problem),
        }
    }
    if setting == Clipboard::Auto && place.far_tmux {
        problems.push(
            "nun is inside a tmux on the far side of ssh, which it cannot ask \
             (set ui.clipboard = \"osc52\", and `set -s set-clipboard on` in that tmux)"
                .to_string(),
        );
    } else if setting == Clipboard::Auto && !place.tmux && !place.advertised {
        problems.push(
            "the terminal did not say it accepts OSC 52 (set ui.clipboard = \"osc52\" if it does)"
                .to_string(),
        );
    }
    CopyOutcome::Failed(problems.join("; "))
}

/// Where OSC 52 sent straight to the terminal goes, and what it depends on.
const fn terminal_note(place: Place) -> &'static str {
    if place.far_tmux {
        "tmux on your machine to copy, which passes them on only with `set -s set-clipboard on`"
    } else if place.advertised {
        "the terminal to copy"
    } else {
        "the terminal to copy, which did not say it accepts OSC 52 (ui.clipboard = \"osc52\")"
    }
}

/// Copy through tmux, however its `set-clipboard` lets a copy through.
/// `forced` when the setting asks for OSC 52 whatever tmux says.
fn through_tmux(text: &str, world: &impl World, forced: bool) -> Result<CopyOutcome, String> {
    match world.tmux_setting().as_deref() {
        Some("on") => fits(text).map(|()| CopyOutcome::Escape {
            bytes: osc52(text),
            to: "tmux to copy, which passes them to its terminal with set-clipboard on".to_string(),
        }),
        // tmux drops OSC 52 from a program here, but takes this.
        Some("external") => load_into_tmux(text, world),
        Some(_) if forced => load_into_tmux(text, world),
        Some(_) => Err("tmux's set-clipboard is off, which nun takes as a copy not wanted there \
             (ui.clipboard = \"osc52\" sends it all the same)"
            .into()),
        None => Err("tmux did not say how it passes a copy on".into()),
    }
}

/// Put `text` in tmux's buffer, and have tmux pass it on.
fn load_into_tmux(text: &str, world: &impl World) -> Result<CopyOutcome, String> {
    fits(text)?;
    if world.tmux_load(text) {
        return Ok(CopyOutcome::Sent(
            "tmux to copy (load-buffer -w passes them to its terminal)".to_string(),
        ));
    }
    Err("tmux would not take it (load-buffer -w needs tmux 3.2 and an attached client)".into())
}

/// Whether `text` is short enough to send through OSC 52.
fn fits(text: &str) -> Result<(), String> {
    if text.len() <= MOST {
        return Ok(());
    }
    Err(format!(
        "{} KB is more than a terminal takes in one escape ({} KB)",
        text.len().div_ceil(1000),
        MOST / 1000,
    ))
}

/// The OSC 52 escape that puts `text` on the clipboard.
///
/// `c` names the clipboard: an empty selection means the primary selection
/// to xterm. String Terminator rather than BEL, as everything else nun sends.
#[must_use]
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x1b\\", base64(text.as_bytes()))
}

/// Standard base64, padded.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> shift) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A line for `nun --capabilities`: whether the terminal said it accepts
/// OSC 52, and where a copy goes because of it.
#[must_use]
pub fn describe(setting: Clipboard, place: Place) -> String {
    let said = if place.advertised {
        "yes (parameter 52 in its device attributes)"
    } else if place.tmux {
        "not said (tmux never does; its set-clipboard is asked at each copy)"
    } else if place.far_tmux {
        "not said (a tmux on the far side of ssh, which nun cannot ask)"
    } else {
        "not said (Alacritty, xterm and older iTerm2 and WezTerm cannot say)"
    };
    let order: Vec<&str> = routes(setting, place)
        .into_iter()
        .map(|route| match route {
            Route::System => "a clipboard program",
            Route::Terminal => "the terminal, by OSC 52",
            Route::Tmux => "tmux, as its set-clipboard allows",
        })
        .collect();
    let setting = format!("{setting:?}").to_lowercase();
    format!(
        "clipboard escape (OSC 52): {said}\ncopies go to: {} (ui.clipboard = \"{setting}\")",
        order.join(", then ")
    )
}

/// One copy to make.
#[cfg(not(test))]
pub struct Job {
    /// What to copy.
    pub text: String,
    /// `ui.clipboard`.
    pub setting: Clipboard,
    /// Where nun is running.
    pub place: Place,
}

/// Start the thread that makes copies, one at a time and in order, and
/// posts how each went. `None` if no thread could be started.
#[cfg(not(test))]
pub fn worker(
    post: std::sync::Arc<dyn Fn(nun_ui::Event) + Send + Sync>,
) -> Option<std::sync::mpsc::Sender<Job>> {
    let (sender, jobs) = std::sync::mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || {
            for job in jobs {
                let chars = job.text.chars().count();
                let outcome = copy(&job.text, job.setting, job.place, &Programs);
                post(nun_ui::Event::Copied { chars, outcome });
            }
        })
        .ok()?;
    Some(sender)
}

/// The real world: clipboard programs, and tmux.
#[cfg(not(test))]
struct Programs;

#[cfg(not(test))]
impl World for Programs {
    fn system(&self, text: &str) -> Result<(), String> {
        let tried = clipboard_commands();
        if tried.iter().any(|argv| run(argv, text, false).is_some()) {
            return Ok(());
        }
        let names: Vec<&str> = tried.iter().map(|argv| argv[0]).collect();
        Err(format!("no clipboard program took it (tried {})", names.join(", ")))
    }

    fn tmux_setting(&self) -> Option<String> {
        let value = run(&["tmux", "show-options", "-sv", "set-clipboard"], "", true)?;
        Some(value.trim().to_string())
    }

    fn tmux_remote(&self) -> Option<bool> {
        // `SSH_CONNECTION=…` when the client attached over ssh, and
        // `-SSH_CONNECTION` when it did not: tmux sets it on attach.
        let line = run(&["tmux", "show-environment", "SSH_CONNECTION"], "", true)?;
        let line = line.trim();
        if line.starts_with("SSH_CONNECTION=") {
            Some(true)
        } else {
            (line == "-SSH_CONNECTION").then_some(false)
        }
    }

    fn tmux_load(&self, text: &str) -> bool {
        // Named, because with no client to pass it to, tmux fills its
        // buffer, passes nothing on and still says it succeeded.
        let Some(client) = run(&["tmux", "display-message", "-p", "#{client_name}"], "", true)
        else {
            return false;
        };
        let client = client.trim();
        !client.is_empty()
            && run(&["tmux", "load-buffer", "-w", "-t", client, "-"], text, false).is_some()
    }
}

/// The programs tried, in order, to put text on the clipboard.
#[cfg(not(test))]
fn clipboard_commands() -> Vec<&'static [&'static str]> {
    if cfg!(target_os = "macos") {
        return vec![&["pbcopy"]];
    }
    let mut commands: Vec<&'static [&'static str]> = Vec::new();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(&["wl-copy"]);
    }
    commands.push(&["xclip", "-selection", "clipboard"]);
    commands.push(&["xsel", "--clipboard", "--input"]);
    commands
}

/// How long a clipboard program, or tmux, has to finish before it is given
/// up on, so a copy that hangs still comes back to say it failed.
#[cfg(not(test))]
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// Run `argv` with `input` on its standard input. What it wrote, when it
/// ran to success within [`PATIENCE`]; empty unless `capture`.
///
/// Output is captured only from tmux: xclip leaves a child behind to serve
/// the selection, which would hold a captured output open for as long as it
/// lives.
#[cfg(not(test))]
fn run(argv: &[&str], input: &str, capture: bool) -> Option<String> {
    use std::io::{Read as _, Write as _};
    use std::process::{Command, Stdio};

    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(if capture { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Dropped once written, so the program sees the end of its input.
    let written =
        child.stdin.take().is_some_and(|mut stdin| stdin.write_all(input.as_bytes()).is_ok());
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() && written => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(Some(_)) => return None,
        }
    }
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_string(&mut output).ok()?;
    }
    Some(output)
}

/// A world with a clipboard program that takes anything, for the tests.
#[cfg(test)]
pub struct Faked;

#[cfg(test)]
impl World for Faked {
    fn system(&self, _: &str) -> Result<(), String> {
        Ok(())
    }

    fn tmux_setting(&self) -> Option<String> {
        None
    }

    fn tmux_remote(&self) -> Option<bool> {
        None
    }

    fn tmux_load(&self, _: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A world whose programs do as told, and which remembers what was
    /// tried.
    #[derive(Default)]
    struct Script {
        system: bool,
        tmux: Option<&'static str>,
        tmux_remote: Option<bool>,
        load: bool,
        tried: RefCell<Vec<&'static str>>,
    }

    impl World for Script {
        fn system(&self, _: &str) -> Result<(), String> {
            self.tried.borrow_mut().push("system");
            if self.system { Ok(()) } else { Err("no clipboard program took it".into()) }
        }

        fn tmux_setting(&self) -> Option<String> {
            self.tried.borrow_mut().push("tmux show");
            self.tmux.map(str::to_string)
        }

        fn tmux_remote(&self) -> Option<bool> {
            self.tmux_remote
        }

        fn tmux_load(&self, _: &str) -> bool {
            self.tried.borrow_mut().push("tmux load");
            self.load
        }
    }

    const LOCAL: Place = Place { advertised: false, tmux: false, far_tmux: false, remote: false };
    const KITTY: Place = Place { advertised: true, ..LOCAL };
    const KITTY_OVER_SSH: Place = Place { advertised: true, remote: true, ..LOCAL };
    const TMUX: Place = Place { tmux: true, ..LOCAL };

    #[test]
    fn the_escape_is_osc_52_to_the_clipboard_in_base64() {
        assert_eq!(osc52("beta"), "\x1b]52;c;YmV0YQ==\x1b\\");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("ö€".as_bytes()), "w7bigqw=");
    }

    #[test]
    fn the_environment_says_where_nun_is() {
        let place = Place::new(true, Some("kitty(0.43.1)"), |name| name == "SSH_TTY");
        assert_eq!(place, KITTY_OVER_SSH);
        assert!(Place::new(false, None, |name| name == "SSH_CONNECTION").remote);
        assert!(Place::new(false, Some("tmux 3.6a"), |name| name == "TMUX").tmux);
        assert!(Place::new(false, None, |name| name == "TMUX").tmux, "no version: believed");
    }

    #[test]
    fn a_tmux_variable_inherited_by_another_terminal_is_not_believed() {
        let place = Place::new(true, Some("ghostty 1.2.0"), |name| name == "TMUX");
        assert!(!place.tmux && !place.far_tmux);
    }

    #[test]
    fn a_tmux_that_answers_with_no_tmux_variable_is_on_the_far_side_of_ssh() {
        let place = Place::new(false, Some("tmux 3.5a"), |name| name == "SSH_TTY");
        assert!(place.far_tmux && !place.tmux && place.remote);
    }

    #[test]
    fn over_ssh_a_terminal_that_said_it_accepts_osc_52_is_sent_the_text() {
        let world = Script { system: true, ..Script::default() };
        let outcome = copy("beta", Clipboard::Auto, KITTY_OVER_SSH, &world);
        let CopyOutcome::Escape { bytes, to } = outcome else { panic!("{outcome:?}") };
        assert_eq!(bytes, osc52("beta"));
        assert_eq!(to, "the terminal to copy", "sent, not copied");
        assert!(world.tried.borrow().is_empty(), "this machine's clipboard is not the person's");
    }

    #[test]
    fn locally_a_clipboard_program_comes_first() {
        let world = Script { system: true, ..Script::default() };
        assert_eq!(copy("beta", Clipboard::Auto, KITTY, &world), CopyOutcome::Taken);
    }

    #[test]
    fn locally_with_no_program_the_terminal_is_next() {
        let world = Script::default();
        let outcome = copy("beta", Clipboard::Auto, KITTY, &world);
        assert!(matches!(outcome, CopyOutcome::Escape { .. }), "{outcome:?}");
        assert_eq!(*world.tried.borrow(), ["system"]);
    }

    #[test]
    fn a_program_over_ssh_is_said_to_have_filled_the_remote_clipboard() {
        let world = Script { system: true, ..Script::default() };
        let remote = Place { remote: true, ..LOCAL };
        let outcome = copy("beta", Clipboard::Auto, remote, &world);
        assert!(
            matches!(&outcome, CopyOutcome::Sent(to) if to.contains("not yours")),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_terminal_that_did_not_say_is_never_sent_the_text_unasked() {
        let world = Script::default();
        let remote = Place { remote: true, ..LOCAL };
        let CopyOutcome::Failed(why) = copy("beta", Clipboard::Auto, remote, &world) else {
            panic!("nothing took it");
        };
        assert!(why.contains("no clipboard program took it"), "{why}");
        assert!(why.contains("did not say it accepts OSC 52"), "{why}");
        assert!(why.contains("ui.clipboard = \"osc52\""), "names the setting: {why}");
    }

    #[test]
    fn where_nothing_can_be_detected_the_setting_decides_and_says_so() {
        let world = Script { system: true, ..Script::default() };
        let CopyOutcome::Escape { to, .. } = copy("beta", Clipboard::Osc52, LOCAL, &world) else {
            panic!("the setting sends it");
        };
        assert!(to.contains("did not say it accepts OSC 52"), "{to}");
        assert!(world.tried.borrow().is_empty(), "no program is tried");

        let world = Script::default();
        let outcome = copy("beta", Clipboard::System, KITTY_OVER_SSH, &world);
        assert!(matches!(outcome, CopyOutcome::Failed(_)), "never the terminal: {outcome:?}");
    }

    #[test]
    fn a_tmux_nun_cannot_ask_is_sent_nothing_unasked_and_the_message_says_what_it_needs() {
        let far = Place { far_tmux: true, remote: true, ..LOCAL };
        let CopyOutcome::Failed(why) = copy("beta", Clipboard::Auto, far, &Script::default())
        else {
            panic!("not detected, not sent");
        };
        assert!(why.contains("far side of ssh") && why.contains("set-clipboard on"), "{why}");

        let outcome = copy("beta", Clipboard::Osc52, far, &Script::default());
        let CopyOutcome::Escape { to, .. } = outcome else { panic!("{outcome:?}") };
        assert!(to.contains("only with `set -s set-clipboard on`"), "{to}");
    }

    #[test]
    fn under_tmux_its_set_clipboard_decides() {
        let world = Script { tmux: Some("on"), ..Script::default() };
        let CopyOutcome::Escape { to, .. } = copy("beta", Clipboard::Auto, TMUX, &world) else {
            panic!("tmux passes it on");
        };
        assert!(to.contains("set-clipboard on"), "{to}");

        let world = Script { system: true, tmux: Some("external"), ..Script::default() };
        let outcome = copy("beta", Clipboard::Auto, TMUX, &world);
        assert_eq!(outcome, CopyOutcome::Taken, "a program first, locally");

        let world = Script { tmux: Some("external"), load: true, ..Script::default() };
        let outcome = copy("beta", Clipboard::Auto, TMUX, &world);
        assert!(matches!(&outcome, CopyOutcome::Sent(to) if to.contains("load-buffer")));
        assert_eq!(*world.tried.borrow(), ["system", "tmux show", "tmux load"]);

        let world = Script { tmux: Some("external"), load: true, ..Script::default() };
        let outcome = copy("beta", Clipboard::Osc52, TMUX, &world);
        assert!(matches!(&outcome, CopyOutcome::Sent(to) if to.contains("load-buffer")));
        assert_eq!(*world.tried.borrow(), ["tmux show", "tmux load"]);
    }

    #[test]
    fn under_tmux_tmux_says_whether_its_client_is_over_ssh() {
        // The pane was made at the machine itself, and attached to later
        // over ssh: its own environment says local, tmux says otherwise.
        let world = Script { tmux: Some("off"), tmux_remote: Some(true), ..Script::default() };
        let CopyOutcome::Failed(why) = copy("beta", Clipboard::Auto, TMUX, &world) else {
            panic!("tmux passes nothing on");
        };
        assert!(why.contains("set-clipboard is off"), "{why}");
        assert_eq!(*world.tried.borrow(), ["tmux show", "system"], "tmux first, then a program");
    }

    #[test]
    fn set_clipboard_off_is_gone_around_only_when_the_setting_says_so() {
        let world = Script { tmux: Some("off"), load: true, ..Script::default() };
        let outcome = copy("beta", Clipboard::Osc52, TMUX, &world);
        assert!(matches!(&outcome, CopyOutcome::Sent(to) if to.contains("load-buffer")));
    }

    #[test]
    fn too_much_for_one_escape_is_refused_whole() {
        let world = Script::default();
        let text = "x".repeat(MOST + 1);
        let CopyOutcome::Failed(why) = copy(&text, Clipboard::Osc52, KITTY, &world) else {
            panic!("never cut short");
        };
        assert!(why.contains("more than a terminal takes"), "{why}");
        let world = Script { tmux: Some("external"), load: true, ..Script::default() };
        let outcome = copy(&text, Clipboard::Osc52, TMUX, &world);
        assert!(matches!(outcome, CopyOutcome::Failed(_)), "tmux passes it on in one escape");
        let text = "x".repeat(MOST);
        let world = Script::default();
        assert!(matches!(copy(&text, Clipboard::Osc52, KITTY, &world), CopyOutcome::Escape { .. }));
    }

    #[test]
    fn the_report_says_what_was_found_and_where_a_copy_goes() {
        let line = describe(Clipboard::Auto, KITTY_OVER_SSH);
        assert!(line.contains("yes (parameter 52"), "{line}");
        assert!(line.contains("the terminal, by OSC 52, then a clipboard program"), "{line}");
        assert!(describe(Clipboard::Auto, LOCAL).contains("not said"));
        assert!(describe(Clipboard::Auto, TMUX).contains("set-clipboard is asked"));
    }
}
