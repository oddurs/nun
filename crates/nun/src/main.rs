//! The `nun` binary.

mod app;
mod clipboard;
mod commands;
mod hints;
mod reload;
mod restore;
mod session;
mod terminal;

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use app::{App, Outcome};
use commands::KeySet;
use nun_config::{Loaded, Polarity, Undercurl};
use nun_core::{Buffer, LoadReport};
use nun_theme::{Probe, Ramp, Rgb, Role, Source, derive, derive_with_polarity};
use nun_ui::{
    Capabilities, Events, Palette, Screen, UnderlineProbe, Underlines, install_panic_hook,
};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let no_session = take_flag(&mut args, "--no-session");
    let lsp_log = match take_value(&mut args, "--lsp-log") {
        Ok(path) => path.map(PathBuf::from),
        Err(problem) => {
            eprint!("nun: {problem}\n\n{}", usage());
            std::process::exit(2);
        }
    };

    match args.first().map(String::as_str) {
        Some("--version" | "-V") => println!("nun {VERSION}"),
        Some("--capabilities") => {
            let startup = terminal::probe(terminal::PROBE_TIMEOUT);
            print!("{}", capabilities_report(&nun_config::load(), &startup));
        }
        Some("theme") => print!("{}", theme(args.get(1).map(String::as_str))),
        Some("config") => match config(&args[1..]) {
            Ok(out) => print!("{out}"),
            Err(problem) => {
                eprint!("nun: {problem}\n\n{}", usage());
                std::process::exit(2);
            }
        },
        Some("keys") => print!("{}", commands::reference()),
        Some("glyphs") => print!("{}", glyphs(&nun_config::load()).reference()),
        Some("--help" | "-h") | None => print!("{}", usage()),
        Some(argument) if argument.starts_with('-') => {
            eprint!("nun: unknown option `{argument}`\n\n{}", usage());
            std::process::exit(2);
        }
        Some(path) => {
            if let Err(error) = edit(Path::new(path), lsp_log.as_deref(), no_session) {
                eprintln!("nun: {error}");
                std::process::exit(1);
            }
        }
    }
}

/// Take `--name` out of the arguments, wherever it is, and say whether it was
/// there.
fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    let before = args.len();
    args.retain(|arg| arg != name);
    args.len() != before
}

/// Take `--name <value>` or `--name=<value>` out of the arguments, wherever it
/// is, so the rest can be read positionally as they always were.
fn take_value(args: &mut Vec<String>, name: &str) -> Result<Option<String>, String> {
    let joined = format!("{name}=");
    let Some(at) = args.iter().position(|arg| arg == name || arg.starts_with(&joined)) else {
        return Ok(None);
    };
    let arg = args.remove(at);
    if let Some(value) = arg.strip_prefix(&joined) {
        return Ok(Some(value.to_string()));
    }
    if at < args.len() && !args[at].starts_with('-') {
        return Ok(Some(args.remove(at)));
    }
    Err(format!("`{name}` needs a path after it"))
}

/// The language servers the configuration asks for, by language, and
/// anything in `[lsp]` that names a language nun does not know.
fn language_servers(settings: &Loaded) -> (BTreeMap<String, nun_lsp::ServerSpec>, Vec<String>) {
    let mut servers = BTreeMap::new();
    let mut problems = Vec::new();
    for (language, server) in &settings.config.lsp {
        if !nun_lsp::languages().contains(&language.as_str()) {
            problems.push(format!(
                "lsp.{language}: nun has no language called `{language}`; it knows {}",
                nun_lsp::languages().join(", ")
            ));
            continue;
        }
        if !server.enabled {
            continue;
        }
        // A default is quiet when it is not installed; one somebody wrote
        // into their config is worth saying is missing.
        let optional = !settings.set_by_a_file(&format!("lsp.{language}"));
        let spec = nun_lsp::ServerSpec {
            command: server.command.clone(),
            args: server.args.clone(),
            optional,
        };
        servers.insert(language.clone(), spec);
    }
    (servers, problems)
}

/// Open a file and run the editor over it.
/// What the settings say about what the pointer does, and what it is shown.
fn pointer_settings(app: &mut App, config: &nun_config::Config) {
    if let Some(ms) = config.double_click_ms {
        app.set_double_click(std::time::Duration::from_millis(ms));
    }
    app.set_hover(std::time::Duration::from_millis(config.hover_delay_ms), config.hyperlinks);
    app.set_lightbulb(config.lightbulb);
}

/// Which languages have their files formatted on save.
fn format_on_save(settings: &Loaded) -> Vec<String> {
    settings
        .config
        .lsp
        .iter()
        .filter(|(_, server)| server.enabled && server.format_on_save)
        .map(|(language, _)| language.clone())
        .collect()
}

/// Everything wrong with the settings, from the files themselves to what
/// the theme, the glyphs, the keys and the servers could not use of them.
fn all_problems(settings: &Loaded, startup: &terminal::Startup, set: KeySet) -> Vec<String> {
    let (_, roles) = build_ramp(&startup.palette, settings);
    let (_, keys) = commands::keymap(set, &settings.config.keys);
    let (_, servers) = language_servers(settings);
    let files = settings.problems.iter().map(nun_config::Problem::brief);
    files.chain(roles).chain(glyphs(settings).problems).chain(keys).chain(servers).collect()
}

/// `nun config [--explain <key>] [<path>]`: the settings that apply at
/// `path` — the current directory, or a file, whose `.editorconfig` is then
/// included — or how one of them was arrived at.
fn config(args: &[String]) -> Result<String, String> {
    let mut args = args.to_vec();
    let explain = take_value(&mut args, "--explain")
        .map_err(|_| "`--explain` needs a setting after it, like editor.tab_width".to_string())?;
    if let Some(unknown) = args.iter().find(|arg| arg.starts_with('-')) {
        return Err(format!("unknown option `{unknown}`"));
    }
    if args.len() > 1 {
        return Err("`nun config` takes one path at most".to_string());
    }
    let target = PathBuf::from(args.first().map_or(".", String::as_str));
    let file = !target.is_dir();
    let root = workspace_root(&target, !file);
    let settings = nun_config::load_in(&root);
    let file = file.then(|| std::path::absolute(&target).unwrap_or(target));
    let configs = file
        .as_deref()
        .map(|file| nun_config::editorconfig::find(file, nun_config::EditorConfig::read));
    let with_file = file.as_deref().zip(configs.as_deref());
    if let Some(key) = explain {
        return Ok(settings.explain(&key, with_file));
    }
    Ok({
        {
            let whitespace = with_file.map(|(file, configs)| {
                (file, nun_config::Whitespace::resolve(&settings, file, configs))
            });
            let whitespace = whitespace.as_ref().map(|(file, whitespace)| (*file, whitespace));
            format!("{}{}", settings.describe(whitespace), glyphs(&settings).summary())
        }
    })
}

fn edit(path: &Path, lsp_log: Option<&Path>, no_session: bool) -> io::Result<()> {
    let folder = path.is_dir();
    let root = workspace_root(path, folder);
    let settings = nun_config::load_in(&root);

    // Installed before anything touches the terminal, the probe included, so a
    // panic anywhere after this point still puts it back.
    install_panic_hook();

    // Probed before the input reader starts: both want raw bytes from stdin,
    // and only one of them can have them.
    let startup = terminal::probe(terminal::PROBE_TIMEOUT);
    let (ramp, role_problems) = build_ramp(&startup.palette, &settings);
    let glyphs = glyphs(&settings);
    let palette = Palette::new(ramp).with_glyphs(glyphs.glyphs);

    // A folder opens the file tree with an empty buffer beside it; a file
    // opens the file, with the tree rooted at the folder it is in.
    let (mut buffer, report) =
        if folder { (Buffer::new(), LoadReport::default()) } else { open(path)? };
    buffer.set_tab_width(settings.config.tab_width);

    let set = key_set(&settings, startup.kitty_keyboard);
    let (keymap, key_problems) = commands::keymap(set, &settings.config.keys);
    let (servers, server_problems) = language_servers(&settings);
    let problems = [role_problems, glyphs.problems, key_problems, server_problems].concat();
    let ctrl_click = ctrl_click_hint(&settings, &startup, &keymap);

    let mut app = App::new(buffer, palette, keymap);
    app.set_format_on_save(format_on_save(&settings));
    if !no_session {
        if let Some(path) = session::Session::default_path() {
            app.attach_session(session::Session::load(path));
        }
        keep_session(&mut app, &root);
    }
    pointer_settings(&mut app, &settings.config);
    // Only the first is shown: the rest are visible through `nun config`, and a
    // queue of config complaints would bury the editor under them.
    if let Some(warning) = warnings(report, &settings, &problems).into_iter().next() {
        app.warn(warning);
    }
    if let Some(notice) = key_set_notice(&settings, startup.kitty_keyboard) {
        app.warn(notice);
    }
    if let Some(hint) = &ctrl_click {
        app.hint(hint.message());
    }

    let underlines = underlines(&settings, &startup.underlines);
    let mut screen = Screen::open(capabilities(&settings, set), underlines).map_err(|error| {
        // The usual cause is no tty at all — piped input, or a CI runner — and
        // the platform's own message for that is "Device not configured".
        io::Error::new(error.kind(), format!("nun needs an interactive terminal ({error})"))
    })?;
    let events = Events::start()?;

    start_reloader(&mut app, &events, root.clone(), settings, startup, set);

    // Everything the file tree does happens off this thread and comes back
    // through the same channel as the keyboard, so the main thread stays the
    // only thing that touches the tree.
    let sender = events.sender();

    app.open_folder(
        root,
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

    // Git has threads of its own too: a status walk of a large repository
    // takes as long as it takes, and the tree is drawn without waiting for it.
    let sender = events.sender();
    app.attach_vcs(nun_vcs::Vcs::new(Box::new(move |reply| {
        let _ = sender.send(nun_ui::Event::Vcs(reply));
    })));

    // Searching the project has a thread of its own rather than sharing the
    // tree's: a search of a large repository would otherwise sit in front of
    // the directory listings the tree is waiting on.
    let sender = events.sender();
    app.attach_search(nun_workspace::Grep::new(Box::new(move |found| {
        let _ = sender.send(nun_ui::Event::Found(found));
    })));

    // Language servers run on a runtime of their own, and everything they say
    // comes back through the same channel. None starts until a file in its
    // language is opened.
    start_language_servers(&mut app, &events, servers, lsp_log);

    attach_terminal(&mut app, &events, path, folder)?;

    attach_watchers(&mut app, &events);

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

        if let Some(underlines) = app.take_underlines() {
            // A failure costs one frame drawn twice, not the session.
            let _ = screen.set_underlines(underlines);
        }
        for escape in app.take_escapes() {
            // The copy was only ever said to have been sent: a terminal that
            // cannot be written to has not been sent it, and the next frame
            // fails louder than this would.
            let _ = screen.send(&escape);
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
    wind_down(&mut app, ctrl_click);
    Ok(())
}

/// Watch and read the settings files on a thread of their own; a change comes
/// back through the same channel as everything else, to be swapped in whole.
fn start_reloader(
    app: &mut App,
    events: &Events,
    root: PathBuf,
    settings: Loaded,
    startup: terminal::Startup,
    set: KeySet,
) {
    let sender = events.sender();
    let files = nun_config::Files {
        user: settings.user.clone(),
        project: settings.project.as_ref().map(|project| project.file.clone()),
    };
    let post = Box::new(move |news| {
        let _ = sender.send(nun_ui::Event::Config(news));
    });
    let reloader = reload::Reloader::start(root, files, post)
        .map_err(|error| app.warn(format!("Settings will not reload on their own: {error}")))
        .ok();
    app.attach_settings(settings, startup, set, reloader);
}

/// Let the terminal panel start shells: in the folder that was opened, or
/// where nun was started from when it was a file, since that is where the
/// person was working.
fn attach_terminal(app: &mut App, events: &Events, path: &Path, folder: bool) -> io::Result<()> {
    let start = if folder { workspace_root(path, true) } else { std::env::current_dir()? };
    let sender = events.sender();
    app.attach_terminal(
        start,
        std::sync::Arc::new(move |event| {
            let _ = sender.send(event);
        }),
    );
    Ok(())
}

/// Everything after the terminal is back: saving what was waiting, keeping
/// what is remembered between sessions, and stopping the language servers.
fn wind_down(app: &mut App, ctrl_click: Option<hints::Unseen>) {
    // A save still waiting on a formatter is made now, unformatted, however
    // the loop ended — a signal as much as a quit.
    app.save_before_quitting();
    // After the terminal is back, so a failure can be said where it is seen.
    // Losing it costs the layout and the folds, and nothing else.
    if let Err(error) = app.save_session() {
        eprintln!("nun: could not remember this session: {error}");
    }
    if let Some(hint) = ctrl_click.filter(|_| app.hint_seen()) {
        let _ = hint.remember();
    }
    // Every shell in the panel is hung up at once, and waited for: each has
    // a short grace to go before it is killed, and nothing it started
    // outlives the editor.
    app.shutdown_terminals();
    // Last, and bounded: a server that will not exit is killed at the
    // deadline rather than waited for.
    app.shutdown_lsp();
}

/// Watch the disk: the tree's expanded folders, and whatever folders the
/// language servers ask to have watched for them.
fn attach_watchers(app: &mut App, events: &Events) {
    let sender = events.sender();
    match nun_workspace::Watcher::new(Box::new(move |change| {
        let _ = sender.send(nun_ui::Event::Files { dir: change.dir, error: change.watch_error });
    })) {
        Ok(watcher) => app.attach_watcher(watcher),
        Err(error) => app.warn(format!("The file tree will not update on its own: {error}")),
    }

    // Language servers that leave watching the disk to their client register
    // the folders they want watched; this watches them. Idle until one does.
    let sender = events.sender();
    match nun_workspace::DiskWatcher::new(Box::new(move |news| {
        let _ = sender.send(nun_ui::Event::Disk(news));
    })) {
        Ok(disk) => app.attach_disk_watcher(disk),
        Err(error) => app.warn(format!(
            "The language servers will not hear of files changed outside nun: {error}"
        )),
    }
}

/// Put back the session `root` was left with, and keep it written down from
/// here on. Whatever is wrong with what was kept, nun starts anyway: a notice
/// says what was lost.
fn keep_session(app: &mut App, root: &Path) {
    let file = restore::path_for(root);
    let mut writable = true;
    match file.as_deref().map(|file| restore::load(file, root)) {
        Some(restore::Loaded::Found(state)) => app.restore_session(&state),
        Some(restore::Loaded::Damaged(why)) => {
            app.warn(format!(
                "The last session here could not be read ({why}), so nun started without it."
            ));
        }
        Some(restore::Loaded::Newer(version)) => {
            // Written over, it would be lost to the nun that wrote it.
            writable = false;
            app.warn(format!(
                "The last session here is from a newer nun (format {version}), so it was not restored and will not be changed."
            ));
        }
        Some(restore::Loaded::Nothing) | None => {}
    }
    match restore::Writer::start() {
        Ok(writer) => app.keep_session(root.to_path_buf(), file.filter(|_| writable), writer),
        Err(error) => app.warn(format!("This session will not be remembered: {error}")),
    }
}

/// Start the language server runtime, posting to `events`.
fn start_language_servers(
    app: &mut App,
    events: &Events,
    servers: BTreeMap<String, nun_lsp::ServerSpec>,
    log: Option<&Path>,
) {
    let report = |sender: std::sync::mpsc::Sender<nun_ui::Event>| -> Box<dyn Fn(nun_lsp::Event) + Send + Sync> {
        Box::new(move |event| {
            let _ = sender.send(nun_ui::Event::Lsp(event));
        })
    };
    match nun_lsp::Lsp::start(servers.clone(), log, report(events.sender())) {
        Ok(lsp) => app.attach_lsp(lsp),
        Err(error) => {
            // Most likely the log could not be created. The servers are worth
            // more than the log, so they start without it.
            let shown = log.map_or_else(String::new, |path| format!(" to {}", path.display()));
            app.warn(format!("Could not log the language servers{shown}: {error}"));
            match nun_lsp::Lsp::start(servers, None, report(events.sender())) {
                Ok(lsp) => app.attach_lsp(lsp),
                Err(error) => app.warn(format!("Language servers are off: {error}")),
            }
        }
    }
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

/// The glyphs `[glyphs]` asks for, and what could not be used of it.
fn glyphs(settings: &Loaded) -> nun_ui::glyph::Resolution {
    nun_ui::Glyphs::resolve(&settings.config.glyph_preset, &settings.config.glyphs)
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

/// What to say about Ctrl-click, where the terminal is known to keep it for
/// itself, and it has not been said before. Nothing with the mouse turned
/// off: no click reaches nun anyway.
fn ctrl_click_hint(
    settings: &Loaded,
    startup: &terminal::Startup,
    keymap: &nun_input::Keymap<commands::Command>,
) -> Option<hints::Unseen> {
    if !settings.config.mouse {
        return None;
    }
    let key = keymap.sequences_for(&commands::Command::GoToDefinition).first().map_or_else(
        || "\"Go to definition\" in the palette".to_string(),
        |keys| nun_input::Sequence(keys).to_string(),
    );
    hints::CtrlClick::detect(startup.underlines.version(), startup.kitty_keyboard)
        .hint(&key)
        .and_then(hints::Unseen::of)
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

/// Which underlines to draw: what the terminal said, unless the config says
/// otherwise. `on` is the person declaring support nun cannot detect — tmux
/// set up with `usstyle`, or Alacritty — not a guess from `$TERM`.
fn underlines(settings: &Loaded, probe: &UnderlineProbe) -> Underlines {
    match settings.config.undercurl {
        Undercurl::Auto => probe.underlines(),
        Undercurl::On => Underlines::FULL,
        Undercurl::Off => Underlines::PLAIN,
    }
}

/// What `nun --capabilities` prints: what the terminal said about itself, and
/// what nun is doing about it.
fn capabilities_report(settings: &Loaded, startup: &terminal::Startup) -> String {
    use std::fmt::Write as _;

    let yes = |on: bool| if on { "yes" } else { "no" };
    let probe = &startup.underlines;
    let mut out = String::new();
    let _ = writeln!(out, "terminal: {}", probe.version().unwrap_or("(did not say)"));
    let _ = writeln!(
        out,
        "palette: {}",
        match startup.palette.source {
            Source::Terminal => "probed from this terminal",
            Source::ColorFgBg => "no reply; polarity from COLORFGBG",
            Source::Builtin => "no reply; nun's built-in neutrals",
        }
    );
    let _ = writeln!(
        out,
        "kitty keyboard protocol: {}",
        match startup.kitty_keyboard {
            Some(true) => "yes",
            Some(false) => "no",
            None => "no answer in time",
        }
    );
    let (curly, colour) = (probe.curly(), probe.colour());
    let _ = writeln!(out, "curly underline: {} ({})", yes(curly.supported()), curly.describe());
    let _ = writeln!(out, "underline colour: {} ({})", yes(colour.supported()), colour.describe());

    let used = underlines(settings, probe);
    let drawn = match (used.curly, used.colour) {
        (true, true) => "a curly underline in the severity's colour",
        (true, false) => "a curly underline in the text's colour",
        (false, true) => "a straight underline in the severity's colour",
        (false, false) => "a straight underline in the text's colour; severity is on the rail",
    };
    let why = match settings.config.undercurl {
        Undercurl::Auto => "undercurl = \"auto\": as the terminal said",
        Undercurl::On => "undercurl = \"on\" in the config",
        Undercurl::Off => "undercurl = \"off\" in the config",
    };
    let _ = writeln!(out, "diagnostics: {drawn} ({why})");
    let ctrl_click = hints::CtrlClick::detect(probe.version(), startup.kitty_keyboard);
    let _ = writeln!(out, "ctrl-click: {}", ctrl_click.describe());
    let place = clipboard::Place::new(startup.takes_osc52(), probe.version(), |name| {
        std::env::var_os(name).is_some()
    });
    let _ = writeln!(out, "{}", clipboard::describe(settings.config.clipboard, place));
    out
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
        warnings.push(format!("Config: {}", problem.brief()));
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
         Usage: nun [--no-session] [--lsp-log <path>] <file>\n       nun [--no-session] [--lsp-log <path>] <folder>\n       nun config [--explain <key>] [<path>]\n       nun keys\n       nun glyphs\n       nun theme dump\n\n\
         Options:\n  \
           -h, --help         Print help\n  \
           -V, --version      Print version\n  \
           --capabilities     Probe this terminal and say what nun will use\n  \
           --no-session       Open clean: neither restore nor remember this folder's layout\n  \
           --lsp-log <path>   Write every message to and from the language servers to <path>\n\n\
         Commands:\n  \
           config         Print the settings in force here, and the layer each came from\n  \
           config --explain <key>  Say what one setting is here, and why\n  \
           keys           List every command and the keys bound to it\n  \
           glyphs         List every glyph nun draws, and what draws it\n  \
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
        settings.problems.push(nun_config::Problem {
            path: "nun.toml".into(),
            line: None,
            message: "something".into(),
        });
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
    fn the_lsp_log_is_taken_out_wherever_it_is() {
        let strings = |args: &[&str]| args.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>();
        for (given, log, rest) in [
            (&["--lsp-log", "a.log", "x.rs"][..], Some("a.log"), &["x.rs"][..]),
            (&["x.rs", "--lsp-log", "a.log"], Some("a.log"), &["x.rs"]),
            (&["--lsp-log=a.log", "x.rs"], Some("a.log"), &["x.rs"]),
            (&["x.rs"], None, &["x.rs"]),
        ] {
            let mut args = strings(given);
            assert_eq!(
                take_value(&mut args, "--lsp-log"),
                Ok(log.map(str::to_string)),
                "{given:?}"
            );
            assert_eq!(args, strings(rest), "{given:?}");
        }
        for given in [&["--lsp-log"][..], &["--lsp-log", "--help"]] {
            let error = take_value(&mut strings(given), "--lsp-log").unwrap_err();
            assert!(error.contains("needs a path"), "{error}");
        }
    }

    #[test]
    fn language_servers_come_from_the_config_and_unknown_languages_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nun.toml");
        std::fs::write(&path, "[lsp.python]\nenabled = false\n\n[lsp.go]\nargs = [\"serve\"]\n\n[lsp.zig]\ncommand = \"zls\"\n").unwrap();
        let sources = nun_config::Sources { user: Some(path), project: None };
        let files = nun_config::Files::read(&sources, &nun_config::Files::default());
        let settings = nun_config::resolve(&files, &nun_config::TrustStore::in_memory());

        let (servers, problems) = language_servers(&settings);
        assert_eq!(servers["rust"].command, "rust-analyzer");
        assert!(servers["rust"].optional, "a default is quiet when it is missing");
        assert!(!servers["go"].optional, "one set up by hand is not");
        assert_eq!(servers["go"].args, ["serve"]);
        assert!(!servers.contains_key("python"), "turned off");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("no language called `zig`"), "{problems:?}");
    }

    #[test]
    fn the_undercurl_setting_overrides_the_probe_both_ways() {
        let mut quiet = UnderlineProbe::new();
        let mut kitty = UnderlineProbe::new();
        let _ = kitty.feed(b"\x1bP1$r0;4:3m\x1b\\\x1bP1$r0;58:2:1:2:3m\x1b\\");
        let _ = quiet.feed(b"\x1bP0$r\x1b\\");

        let mut settings = Loaded::defaults();
        assert_eq!(underlines(&settings, &kitty), Underlines::FULL, "detected");
        assert_eq!(underlines(&settings, &quiet), Underlines::PLAIN, "never assumed");
        settings.config.undercurl = Undercurl::On;
        assert_eq!(underlines(&settings, &quiet), Underlines::FULL, "declared by the person");
        settings.config.undercurl = Undercurl::Off;
        assert_eq!(underlines(&settings, &kitty), Underlines::PLAIN);
    }

    #[test]
    fn the_capabilities_report_says_what_was_found_and_why() {
        let mut underlines = UnderlineProbe::new();
        let _ = underlines.feed(b"\x1bP0$r\x1b\\\x1bP>|tmux 3.5a\x1b\\");
        let startup = terminal::Startup {
            palette: Probe::builtin_dark(),
            kitty_keyboard: Some(false),
            underlines,
            attributes: vec![1, 2],
        };
        let report = capabilities_report(&Loaded::defaults(), &startup);
        assert!(report.contains("terminal: tmux 3.5a"), "{report}");
        assert!(report.contains("curly underline: no (the terminal could not say"), "{report}");
        assert!(report.contains("straight underline in the text's colour"), "{report}");
        assert!(report.contains("undercurl = \"auto\""), "{report}");
    }

    #[test]
    fn glyph_overrides_are_resolved_and_bad_ones_reported_with_the_rest() {
        let mut settings = Loaded::defaults();
        settings.config.glyph_preset = "ascii".into();
        settings.config.glyphs.insert("lightbulb".into(), "?".into());
        settings.config.glyphs.insert("fold.opne".into(), "v".into());
        settings.config.glyphs.insert("tab.close".into(), "😀".into());

        let resolved = glyphs(&settings);
        assert_eq!(resolved.glyphs.get(nun_ui::Glyph::Lightbulb), "?");
        assert_eq!(resolved.glyphs.get(nun_ui::Glyph::TabClose), "x", "the preset's stands in");
        assert_eq!(resolved.problems.len(), 2, "{:?}", resolved.problems);
        assert!(resolved.problems.iter().any(|p| p.contains("did you mean `fold.open`?")));
        assert!(resolved.problems.iter().any(|p| p.contains("U+1F600 is an emoji")));
    }

    #[test]
    fn usage_documents_the_glyphs() {
        assert!(usage().contains("nun glyphs"));
    }

    #[test]
    fn usage_documents_opening_clean() {
        assert!(usage().contains("--no-session"));
    }

    #[test]
    fn the_no_session_flag_is_taken_out_wherever_it_is() {
        let mut args: Vec<String> = ["x.rs", "--no-session"].map(String::from).to_vec();
        assert!(take_flag(&mut args, "--no-session"));
        assert_eq!(args, ["x.rs"]);
        assert!(!take_flag(&mut args, "--no-session"));
    }

    #[test]
    fn usage_documents_the_lsp_log() {
        assert!(usage().contains("--lsp-log <path>"));
    }

    #[test]
    fn the_strongest_outcome_of_a_burst_wins() {
        assert_eq!(Outcome::Continue.and(Outcome::Redraw), Outcome::Redraw);
        assert_eq!(Outcome::Redraw.and(Outcome::Quit), Outcome::Quit);
        assert_eq!(Outcome::Suspend.and(Outcome::Redraw), Outcome::Suspend);
        assert_eq!(Outcome::Continue.and(Outcome::Continue), Outcome::Continue);
    }
}
