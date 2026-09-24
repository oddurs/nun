//! A real program in a real pty: what it is told, what it says back, and that
//! it is gone once the terminal is.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use nun_term::{Pty, Report, Size, Spec};

/// Long enough for a loaded machine running the whole suite at once.
const PATIENCE: Duration = Duration::from_secs(10);

/// Start `script` under `sh`, and a channel of what it reports.
fn run(script: &str, size: Size) -> (Pty, Receiver<Report>) {
    run_as(script, size, |spec| spec)
}

/// The same, with the spec changed by `adjust` first.
fn run_as(script: &str, size: Size, adjust: impl FnOnce(Spec) -> Spec) -> (Pty, Receiver<Report>) {
    let (sender, receiver) = mpsc::channel();
    let report = Arc::new(move |report| {
        let _ = sender.send(report);
    });
    let cwd = std::env::temp_dir();
    let spec = adjust(Spec::program("/bin/sh", &["-c", script], cwd, size));
    let pty = Pty::spawn(7, &spec, report).expect("a pty opens");
    (pty, receiver)
}

/// Read output until it contains `wanted`, and return all of it.
fn until(pty: &Pty, reports: &Receiver<Report>, wanted: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match reports.recv_timeout(left) {
            Ok(Report::Output { id, bytes }) => {
                assert_eq!(id, 7);
                pty.consumed(bytes.len());
                seen.extend(bytes);
                if String::from_utf8_lossy(&seen).contains(wanted) {
                    return String::from_utf8_lossy(&seen).into_owned();
                }
            }
            Ok(Report::Exited { .. }) | Err(_) => {
                panic!("never saw {wanted:?}; got {:?}", String::from_utf8_lossy(&seen))
            }
        }
    }
}

fn alive(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    rustix::process::test_kill_process(pid).is_ok()
}

#[test]
fn the_program_is_told_its_size_and_its_terminal() {
    let (pty, reports) =
        run("stty size; echo \"term=$TERM tmux=${TMUX:-none}\"", Size::new(81, 23));
    let out = until(&pty, &reports, "tmux=");
    assert!(out.contains("23 81"), "{out:?}");
    assert!(out.contains("term=xterm-256color tmux=none"), "{out:?}");
}

#[test]
fn true_colour_is_claimed_only_where_the_outer_terminal_has_it() {
    let script = "echo \"colorterm=${COLORTERM-unset}.\"";
    let (pty, reports) = run(script, Size::new(80, 24));
    assert!(until(&pty, &reports, "colorterm=").contains("colorterm=truecolor."));
    let (pty, reports) = run_as(script, Size::new(80, 24), Spec::without_truecolor);
    let out = until(&pty, &reports, "colorterm=");
    assert!(out.contains("colorterm=unset."), "not even the outer terminal's: {out:?}");
}

#[test]
fn a_cell_size_makes_a_size_in_pixels() {
    let size = Size::new(80, 24).with_cell(Some((9, 18)));
    assert_eq!(size.pixels(), (720, 432));
    assert_eq!(Size::new(80, 24).with_cell(None).pixels(), (0, 0), "unknown is zero");
}

#[test]
fn what_is_written_is_what_the_program_reads() {
    let (pty, reports) = run("read line; echo \"got <$line>\"", Size::new(80, 24));
    pty.write(b"h\xc3\xa9llo\r".to_vec());
    let out = until(&pty, &reports, "llo>");
    assert!(out.contains("got <héllo>"), "{out:?}");
}

#[test]
fn a_resize_sends_sigwinch_with_the_new_size() {
    let script = "trap 'stty size' WINCH; echo ready; while :; do sleep 0.05; done";
    let (mut pty, reports) = run(script, Size::new(80, 24));
    until(&pty, &reports, "ready");
    pty.resize(Size::new(100, 31)).expect("the pty takes the size");
    assert_eq!(pty.size(), Size::new(100, 31));
    let out = until(&pty, &reports, "31 100");
    assert!(out.contains("31 100"), "{out:?}");
}

#[test]
fn the_same_size_again_is_not_a_resize() {
    let script = "trap 'echo winched' WINCH; echo ready; while :; do sleep 0.05; done";
    let (mut pty, reports) = run(script, Size::new(80, 24));
    until(&pty, &reports, "ready");
    pty.resize(Size::new(80, 24)).unwrap();
    pty.write(b"x".to_vec());
    // Nothing but the echo of what was typed comes back.
    let quiet = reports.recv_timeout(Duration::from_millis(400));
    if let Ok(Report::Output { bytes, .. }) = quiet {
        assert!(!String::from_utf8_lossy(&bytes).contains("winched"));
    }
}

#[test]
fn the_end_of_the_program_is_reported() {
    let (pty, reports) = run("echo bye", Size::new(80, 24));
    let deadline = Instant::now() + PATIENCE;
    loop {
        match reports.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Report::Output { bytes, .. }) => pty.consumed(bytes.len()),
            Ok(Report::Exited { id }) => break assert_eq!(id, 7),
            Err(error) => panic!("no exit reported: {error}"),
        }
    }
}

#[test]
fn dropping_the_terminal_ends_everything_it_started_even_what_ignores_a_hang_up() {
    // The shell and its background job both ignore SIGHUP; only the kill
    // after the grace period ends them.
    let script = "trap '' HUP; sleep 300 & echo \"job=$! end\"; wait";
    let (pty, reports) = run(script, Size::new(80, 24));
    let out = until(&pty, &reports, " end");
    let job: u32 = out
        .split("job=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("no pid in {out:?}"));
    let shell = pty.pid();
    assert!(alive(shell) && alive(job));

    let started = Instant::now();
    drop(pty);
    assert!(started.elapsed() < Duration::from_secs(3), "dropping took {:?}", started.elapsed());
    assert!(!alive(shell), "the shell outlived its terminal");
    // The job is not this process's child, so nothing reaps it here; it is
    // gone once its parent is, give or take the moment it takes to die.
    let deadline = Instant::now() + Duration::from_secs(2);
    while alive(job) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!alive(job), "the background job outlived its terminal");
}

#[test]
fn a_well_behaved_program_goes_at_once() {
    let (pty, reports) = run("echo up; exec sleep 300", Size::new(80, 24));
    until(&pty, &reports, "up");
    let pid = pty.pid();
    let started = Instant::now();
    drop(pty);
    assert!(started.elapsed() < Duration::from_millis(400), "{:?}", started.elapsed());
    assert!(!alive(pid));
}

#[test]
fn a_flood_is_held_back_until_it_is_parsed() {
    let (pty, reports) = run("yes 0123456789abcdef", Size::new(80, 24));
    // Take reports without saying they were parsed: the reader must stop
    // at its allowance rather than read the program's output forever.
    let mut unread = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match reports.recv_timeout(Duration::from_millis(200)) {
            Ok(Report::Output { bytes, .. }) => unread += bytes.len(),
            Ok(Report::Exited { .. }) => panic!("yes stopped"),
            Err(_) => break,
        }
    }
    assert!(unread >= nun_term::pty::MOST_UNREAD, "{unread}");
    assert!(unread < nun_term::pty::MOST_UNREAD + 128 * 1024, "{unread}");
    // Saying so lets it go on.
    pty.consumed(unread);
    let more = reports.recv_timeout(PATIENCE);
    assert!(matches!(more, Ok(Report::Output { .. })), "{more:?}");
}
