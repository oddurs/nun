//! The marks the UI draws, named by what they mean.
//!
//! Glyphs work the way colours do. A widget asks for a [`Glyph`] — `fold.open`,
//! `tab.close` — and never spells the character itself; a [`Preset`] says what
//! each one looks like, and `[glyphs]` in `nun.toml` can change any of them.
//! [`Palette`](crate::Palette) carries the resolved set beside the colours, so a
//! widget that can colour a mark can already draw it, and nothing on the render
//! path has to reach anywhere else for it.
//!
//! Every glyph is one cell wide, and the geometry around each one is built on
//! that: a tab's close button, the gutter's fold column and every truncating
//! `…` are measured before anything is drawn. So a glyph from the config is
//! [`check`]ed before it is used, and one that would move a single cell is
//! refused and reported, with the preset's standing in. A bad glyph costs you
//! one mark, never the layout.
//!
//! Pure data and pure functions: no terminal anywhere in this module.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use ratatui::buffer::CellWidth;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Where a glyph appears, for grouping them in `nun glyphs` and the docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Area {
    /// The text and its gutter.
    Editor,
    /// The column at the editor's right that marks where the problems are.
    Rail,
    /// The strip of open files.
    Tabs,
    /// The line at the bottom.
    StatusLine,
    /// The file tree.
    Tree,
    /// The sidebar's project search.
    Search,
    /// The preview of a replacement across files.
    Replace,
    /// Hover cards, problem cards, and the markdown drawn in them.
    Cards,
    /// Marks that mean the same thing wherever they are.
    Everywhere,
}

impl Area {
    /// Every area, in the order `nun glyphs` lists them.
    pub const ALL: [Self; 9] = [
        Self::Editor,
        Self::Rail,
        Self::Tabs,
        Self::StatusLine,
        Self::Tree,
        Self::Search,
        Self::Replace,
        Self::Cards,
        Self::Everywhere,
    ];

    /// Its heading.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Editor => "Editor",
            Self::Rail => "Rail",
            Self::Tabs => "Tabs",
            Self::StatusLine => "Status line",
            Self::Tree => "File tree",
            Self::Search => "Search",
            Self::Replace => "Replace preview",
            Self::Cards => "Cards",
            Self::Everywhere => "Everywhere",
        }
    }
}

/// A mark the UI draws, named by what it means rather than by what it looks
/// like.
///
/// One role may be drawn in several places when it means one thing in all of
/// them: the magnifier on the status line, on the file tree and in front of the
/// query is one [`Glyph::SearchIcon`], so changing it changes all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Glyph {
    /// `fold.open`
    FoldOpen,
    /// `fold.closed`
    FoldClosed,
    /// `fold.hidden`
    FoldHidden,
    /// `lightbulb`
    Lightbulb,
    /// `rail.1`
    Rail1,
    /// `rail.2`
    Rail2,
    /// `rail.3`
    Rail3,
    /// `rail.4`
    Rail4,
    /// `tab.close`
    TabClose,
    /// `tab.modified`
    TabModified,
    /// `tab.drop`
    TabDrop,
    /// `status.sidebar.shown`
    SidebarShown,
    /// `status.sidebar.hidden`
    SidebarHidden,
    /// `diagnostic.error`
    DiagnosticError,
    /// `diagnostic.warning`
    DiagnosticWarning,
    /// `diagnostic.info`
    DiagnosticInfo,
    /// `tree.new_file`
    TreeNewFile,
    /// `tree.new_folder`
    TreeNewFolder,
    /// `tree.ignored.hidden`
    TreeIgnoredHidden,
    /// `tree.ignored.shown`
    TreeIgnoredShown,
    /// `tree.expanded`
    TreeExpanded,
    /// `tree.collapsed`
    TreeCollapsed,
    /// `tree.symlink`
    TreeSymlink,
    /// `search.icon`
    SearchIcon,
    /// `search.replace`
    SearchReplace,
    /// `search.back`
    SearchBack,
    /// `search.apply`
    SearchApply,
    /// `search.regex`
    SearchRegex,
    /// `search.case`
    SearchCase,
    /// `search.word`
    SearchWord,
    /// `search.ignored`
    SearchIgnored,
    /// `replace.removed`
    ReplaceRemoved,
    /// `replace.added`
    ReplaceAdded,
    /// `replace.included`
    ReplaceIncluded,
    /// `replace.excluded`
    ReplaceExcluded,
    /// `replace.line_break`
    ReplaceLineBreak,
    /// `card.previous`
    CardPrevious,
    /// `card.next`
    CardNext,
    /// `card.above`
    CardAbove,
    /// `card.below`
    CardBelow,
    /// `card.bullet`
    CardBullet,
    /// `card.quote`
    CardQuote,
    /// `card.task.done`
    CardTaskDone,
    /// `card.task.open`
    CardTaskOpen,
    /// `ellipsis`
    Ellipsis,
    /// `rule.horizontal`
    RuleHorizontal,
    /// `rule.vertical`
    RuleVertical,
}

/// What the role table says about one glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spec {
    key: &'static str,
    area: Area,
    cells: usize,
    about: &'static str,
}

impl Spec {
    /// A one-cell role, which is every role there is so far.
    const fn new(key: &'static str, area: Area, about: &'static str) -> Self {
        Self { key, area, cells: 1, about }
    }
}

impl Glyph {
    /// How many roles there are.
    pub const COUNT: usize = Self::ALL.len();

    /// Every role, in the order `nun glyphs` lists them.
    pub const ALL: [Self; 47] = [
        Self::FoldOpen,
        Self::FoldClosed,
        Self::FoldHidden,
        Self::Lightbulb,
        Self::Rail1,
        Self::Rail2,
        Self::Rail3,
        Self::Rail4,
        Self::TabClose,
        Self::TabModified,
        Self::TabDrop,
        Self::SidebarShown,
        Self::SidebarHidden,
        Self::DiagnosticError,
        Self::DiagnosticWarning,
        Self::DiagnosticInfo,
        Self::TreeNewFile,
        Self::TreeNewFolder,
        Self::TreeIgnoredHidden,
        Self::TreeIgnoredShown,
        Self::TreeExpanded,
        Self::TreeCollapsed,
        Self::TreeSymlink,
        Self::SearchIcon,
        Self::SearchReplace,
        Self::SearchBack,
        Self::SearchApply,
        Self::SearchRegex,
        Self::SearchCase,
        Self::SearchWord,
        Self::SearchIgnored,
        Self::ReplaceRemoved,
        Self::ReplaceAdded,
        Self::ReplaceIncluded,
        Self::ReplaceExcluded,
        Self::ReplaceLineBreak,
        Self::CardPrevious,
        Self::CardNext,
        Self::CardAbove,
        Self::CardBelow,
        Self::CardBullet,
        Self::CardQuote,
        Self::CardTaskDone,
        Self::CardTaskOpen,
        Self::Ellipsis,
        Self::RuleHorizontal,
        Self::RuleVertical,
    ];

    /// The role table: every glyph's name, where it is, how wide it must be,
    /// and what it is for. The one place a role is described.
    ///
    /// A match rather than an array, so a role added without a row here does
    /// not compile.
    #[allow(clippy::too_many_lines)] // A table: one row per role, and long for it.
    const fn spec(self) -> Spec {
        use Area::{Cards, Editor, Everywhere, Rail, Replace, Search, StatusLine, Tabs, Tree};
        match self {
            Self::FoldOpen => Spec::new(
                "fold.open",
                Editor,
                "Beside a line that opens a region that can be folded",
            ),
            Self::FoldClosed => {
                Spec::new("fold.closed", Editor, "Beside a folded region's first line")
            }
            Self::FoldHidden => Spec::new(
                "fold.hidden",
                Editor,
                "The chip after a folded line, standing in for the lines it hides",
            ),
            Self::Lightbulb => Spec::new(
                "lightbulb",
                Editor,
                "In the gutter, on the caret's line, when its language server has code actions there",
            ),
            Self::Rail1 => Spec::new(
                "rail.1",
                Rail,
                "One problem on the stretch of file a rail row stands for",
            ),
            Self::Rail2 => Spec::new("rail.2", Rail, "Two problems on one rail row"),
            Self::Rail3 => Spec::new("rail.3", Rail, "Three or four problems on one rail row"),
            Self::Rail4 => Spec::new("rail.4", Rail, "Five or more problems on one rail row"),
            Self::TabClose => {
                Spec::new("tab.close", Tabs, "Closes a tab: on the tab, and on the status line")
            }
            Self::TabModified => Spec::new(
                "tab.modified",
                Tabs,
                "A file with unsaved changes: on its tab, and after its name on the status line",
            ),
            Self::TabDrop => Spec::new("tab.drop", Tabs, "Where a dragged tab will land"),
            Self::SidebarShown => Spec::new(
                "status.sidebar.shown",
                StatusLine,
                "The sidebar button while the sidebar is showing",
            ),
            Self::SidebarHidden => Spec::new(
                "status.sidebar.hidden",
                StatusLine,
                "The sidebar button while the sidebar is hidden",
            ),
            Self::DiagnosticError => {
                Spec::new("diagnostic.error", StatusLine, "Before the count of errors")
            }
            Self::DiagnosticWarning => {
                Spec::new("diagnostic.warning", StatusLine, "Before the count of warnings")
            }
            Self::DiagnosticInfo => Spec::new(
                "diagnostic.info",
                StatusLine,
                "Before the count of information and hints, which are counted together",
            ),
            Self::TreeNewFile => {
                Spec::new("tree.new_file", Tree, "The header button that makes a file")
            }
            Self::TreeNewFolder => {
                Spec::new("tree.new_folder", Tree, "The header button that makes a folder")
            }
            Self::TreeIgnoredHidden => Spec::new(
                "tree.ignored.hidden",
                Tree,
                "The header button that shows ignored files, while they are hidden",
            ),
            Self::TreeIgnoredShown => Spec::new(
                "tree.ignored.shown",
                Tree,
                "The header button that hides ignored files, while they are shown",
            ),
            Self::TreeExpanded => Spec::new(
                "tree.expanded",
                Tree,
                "An open folder, and a search result's file whose lines are showing",
            ),
            Self::TreeCollapsed => Spec::new(
                "tree.collapsed",
                Tree,
                "A closed folder, and a search result's file whose lines are folded away",
            ),
            Self::TreeSymlink => Spec::new("tree.symlink", Tree, "A symbolic link"),
            Self::SearchIcon => Spec::new(
                "search.icon",
                Search,
                "Search: the status line's button, the file tree's button, and the query's prompt",
            ),
            Self::SearchReplace => Spec::new("search.replace", Search, "The replacement's prompt"),
            Self::SearchBack => Spec::new(
                "search.back",
                Search,
                "The button that hands the sidebar back to the file tree",
            ),
            Self::SearchApply => Spec::new(
                "search.apply",
                Search,
                "The button that writes the replacement into the files",
            ),
            Self::SearchRegex => {
                Spec::new("search.regex", Search, "The toggle for matching as a regular expression")
            }
            Self::SearchCase => Spec::new("search.case", Search, "The toggle for matching case"),
            Self::SearchWord => {
                Spec::new("search.word", Search, "The toggle for matching whole words")
            }
            Self::SearchIgnored => {
                Spec::new("search.ignored", Search, "The toggle for searching ignored files")
            }
            Self::ReplaceRemoved => Spec::new(
                "replace.removed",
                Replace,
                "A line as it is now, which the replacement will change",
            ),
            Self::ReplaceAdded => {
                Spec::new("replace.added", Replace, "A line as the replacement will write it")
            }
            Self::ReplaceIncluded => {
                Spec::new("replace.included", Replace, "A file whose lines will be replaced")
            }
            Self::ReplaceExcluded => Spec::new(
                "replace.excluded",
                Replace,
                "A line or file struck out of the replacement",
            ),
            Self::ReplaceLineBreak => Spec::new(
                "replace.line_break",
                Replace,
                "Where a line ends, in a row that shows an edit across more than one line",
            ),
            Self::CardPrevious => {
                Spec::new("card.previous", Cards, "Before a problem card's Previous button")
            }
            Self::CardNext => Spec::new("card.next", Cards, "After a problem card's Next button"),
            Self::CardAbove => {
                Spec::new("card.above", Cards, "A card has more above what it shows")
            }
            Self::CardBelow => {
                Spec::new("card.below", Cards, "A card has more below what it shows")
            }
            Self::CardBullet => {
                Spec::new("card.bullet", Cards, "A bullet in a card's markdown list")
            }
            Self::CardQuote => {
                Spec::new("card.quote", Cards, "The bar beside a quote in a card's markdown")
            }
            Self::CardTaskDone => {
                Spec::new("card.task.done", Cards, "A ticked task in a card's markdown")
            }
            Self::CardTaskOpen => {
                Spec::new("card.task.open", Cards, "An unticked task in a card's markdown")
            }
            Self::Ellipsis => Spec::new(
                "ellipsis",
                Everywhere,
                "Where text was cut short to fit, and after a key that waits for another",
            ),
            Self::RuleHorizontal => Spec::new(
                "rule.horizontal",
                Everywhere,
                "A horizontal line: under the palette's query, between stacked panes",
            ),
            Self::RuleVertical => Spec::new(
                "rule.vertical",
                Everywhere,
                "A vertical line: the sidebar's edge, between panes side by side",
            ),
        }
    }

    /// Its name in `[glyphs]`, like `fold.open`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        self.spec().key
    }

    /// The role with that name.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|glyph| glyph.key() == key)
    }

    /// Where it appears.
    #[must_use]
    pub const fn area(self) -> Area {
        self.spec().area
    }

    /// Exactly how many cells wide a glyph for this role must be.
    #[must_use]
    pub const fn cells(self) -> usize {
        self.spec().cells
    }

    /// What it is for, in one line.
    #[must_use]
    pub const fn about(self) -> &'static str {
        self.spec().about
    }

    /// Its position in [`Glyph::ALL`], which is also its slot in [`Glyphs`].
    const fn index(self) -> usize {
        self as usize
    }
}

/// A complete set of glyphs, with a name to ask for it by.
///
/// A preset is one function from role to glyph, and the match in it is
/// exhaustive, so a role added without a glyph in every preset does not
/// compile. Adding a preset is adding one such function and one line to
/// [`Preset::ALL`].
#[derive(Clone, Copy)]
pub struct Preset {
    name: &'static str,
    about: &'static str,
    table: fn(Glyph) -> &'static str,
}

impl fmt::Debug for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Preset").field(&self.name).finish()
    }
}

impl PartialEq for Preset {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Preset {}

impl Preset {
    /// The glyphs nun has always drawn.
    pub const DEFAULT: Self = Self {
        name: "default",
        about: "What nun has always drawn: box drawing, arrows and geometric shapes",
        table: default_glyph,
    };

    /// Nothing outside ASCII, for a font or a console that has none of it.
    pub const ASCII: Self = Self {
        name: "ascii",
        about: "Nothing outside ASCII, for a font or a console that has nothing else",
        table: ascii_glyph,
    };

    /// Every preset, the default first.
    pub const ALL: [Self; 2] = [Self::DEFAULT, Self::ASCII];

    /// The preset called `name`.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.name == name)
    }

    /// Its name, as `glyphs.preset` takes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// What it is for.
    #[must_use]
    pub const fn about(self) -> &'static str {
        self.about
    }

    /// Its glyph for `glyph`.
    #[must_use]
    pub fn get(self, glyph: Glyph) -> &'static str {
        (self.table)(glyph)
    }
}

/// The `default` preset: exactly what nun drew before glyphs had roles.
const fn default_glyph(glyph: Glyph) -> &'static str {
    match glyph {
        Glyph::FoldOpen | Glyph::TreeExpanded | Glyph::CardBelow => "▾",
        Glyph::FoldClosed | Glyph::TreeCollapsed => "▸",
        Glyph::FoldHidden => "⋯",
        // A lozenge rather than a light bulb: 💡 is two cells wide and drawn
        // as an emoji, in a colour of its own that no role reaches. This one
        // is a single cell whatever the terminal makes of ambiguous widths (it
        // is neutral, not ambiguous, unlike `•`), has no emoji form to be
        // switched into, and is in every monospace font that has the fold
        // arrows beside it.
        Glyph::Lightbulb => "◊",
        // A bar that fills as the marks add up, so a row of five is visibly
        // not a row of one.
        Glyph::Rail1 => "▎",
        Glyph::Rail2 => "▌",
        Glyph::Rail3 => "▊",
        Glyph::Rail4 => "█",
        Glyph::TabClose => "×",
        Glyph::TabModified | Glyph::CardBullet => "•",
        Glyph::TabDrop => "▏",
        Glyph::SidebarShown => "◧",
        Glyph::SidebarHidden => "▯",
        Glyph::DiagnosticError => "✕",
        Glyph::DiagnosticWarning => "▲",
        Glyph::DiagnosticInfo | Glyph::TreeIgnoredShown => "●",
        Glyph::TreeNewFile | Glyph::ReplaceAdded => "+",
        Glyph::TreeNewFolder => "▪",
        // The same ring on the tree's toggle and the search's, so one mark
        // means one thing across the whole sidebar.
        Glyph::TreeIgnoredHidden | Glyph::SearchIgnored => "○",
        Glyph::TreeSymlink => "↪",
        Glyph::SearchIcon => "⌕",
        // An arrow, because the row below the query is what the query
        // *becomes*, and that relation is the whole of what the field means.
        // It reads in the same direction as the diff underneath it, where a
        // `-` line turns into a `+` one.
        Glyph::SearchReplace => "→",
        // A ruled square reads as a listing of rows, which is what the tree is
        // from one cell away, and it pairs with the magnifier the tree shows
        // for coming the other way: two marks for two views, neither of them
        // an arrow that would only say "back" without saying back to what.
        Glyph::SearchBack => "▤",
        // It points out of the panel, which is what applying does: it takes
        // what the panel is showing and puts it into the files underneath.
        // Not a tick and not a play triangle on purpose: a tick reads as
        // confirming something already decided and a triangle as starting
        // something you could stop, and this is neither. What carries the
        // weight is the colour rather than the shape.
        Glyph::SearchApply => "⇓",
        // The wildcard out of `.*`, the one piece of regex notation that reads
        // as regex outside a regex.
        Glyph::SearchRegex => "*",
        // A capital letter is the distinction the toggle controls, so the
        // glyph is the thing itself rather than a sign for it.
        Glyph::SearchCase => "A",
        // A box the match has to fill exactly, which is what a whole-word
        // match is.
        Glyph::SearchWord => "▭",
        Glyph::ReplaceRemoved => "-",
        Glyph::ReplaceIncluded => "✓",
        Glyph::ReplaceExcluded => "·",
        Glyph::ReplaceLineBreak => "↵",
        Glyph::CardPrevious => "‹",
        Glyph::CardNext => "›",
        Glyph::CardAbove => "▴",
        Glyph::CardQuote | Glyph::RuleVertical => "│",
        Glyph::CardTaskDone => "☑",
        Glyph::CardTaskOpen => "☐",
        Glyph::Ellipsis => "…",
        Glyph::RuleHorizontal => "─",
    }
}

/// The `ascii` preset.
///
/// Where the default leans on a shape, this leans on the ASCII character
/// people already read that way: `v` and `>` for open and closed, `@` for a
/// symlink as `ls -F` has it, `/` for search as `less` and vim have it. The
/// ellipsis is `~` rather than `...`, which would be three cells.
const fn ascii_glyph(glyph: Glyph) -> &'static str {
    match glyph {
        Glyph::FoldOpen
        | Glyph::TreeExpanded
        | Glyph::CardBelow
        | Glyph::SearchApply
        | Glyph::ReplaceIncluded => "v",
        Glyph::FoldClosed
        | Glyph::TreeCollapsed
        | Glyph::CardNext
        | Glyph::SearchReplace
        | Glyph::SidebarHidden => ">",
        Glyph::FoldHidden | Glyph::Ellipsis => "~",
        Glyph::Lightbulb | Glyph::SearchRegex | Glyph::CardBullet => "*",
        Glyph::Rail1 | Glyph::ReplaceExcluded => ".",
        Glyph::Rail2 => ":",
        Glyph::Rail3 | Glyph::TabDrop | Glyph::CardQuote | Glyph::RuleVertical => "|",
        Glyph::Rail4 | Glyph::TreeNewFolder => "#",
        Glyph::TabClose | Glyph::DiagnosticError | Glyph::CardTaskDone => "x",
        Glyph::TabModified | Glyph::TreeNewFile | Glyph::ReplaceAdded => "+",
        Glyph::SidebarShown | Glyph::CardPrevious => "<",
        Glyph::DiagnosticWarning => "!",
        Glyph::DiagnosticInfo => "i",
        Glyph::TreeIgnoredHidden | Glyph::SearchIgnored => "o",
        Glyph::TreeIgnoredShown => "O",
        Glyph::TreeSymlink => "@",
        Glyph::SearchIcon => "/",
        Glyph::SearchBack => "=",
        Glyph::SearchCase => "A",
        Glyph::SearchWord => "w",
        Glyph::ReplaceRemoved | Glyph::RuleHorizontal => "-",
        Glyph::CardAbove => "^",
        // Where `cat -A` and vim's list mode mark the end of a line.
        Glyph::ReplaceLineBreak => "$",
        Glyph::CardTaskOpen => "_",
    }
}

/// Why a glyph was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// Nothing at all.
    Empty,
    /// A control or format character: something that moves the cursor,
    /// reorders the text, joins or splits characters, or hides, rather than
    /// drawing.
    Control(char),
    /// A noncharacter, which Unicode promises never to assign.
    Noncharacter(char),
    /// A variation selector, which asks for text or emoji presentation, and
    /// which terminals disagree about the width of.
    VariationSelector(char),
    /// A character whose default presentation is an emoji: drawn in colours
    /// no role reaches, at a width terminals disagree about.
    Emoji(char),
    /// Something that takes no cells: a combining mark or a zero-width
    /// character on its own.
    ZeroWidth,
    /// More than one character as the terminal draws them.
    Several(usize),
    /// One character, of the wrong width.
    Width {
        /// The cells it takes.
        found: usize,
        /// The cells its role takes.
        wanted: usize,
    },
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "it is empty"),
            Self::Control(ch) => write!(
                f,
                "{} is a control or format character, which steers the text rather than drawing",
                code_point(*ch)
            ),
            Self::Noncharacter(ch) => write!(f, "{} is a noncharacter", code_point(*ch)),
            Self::VariationSelector(ch) => write!(
                f,
                "{} is a variation selector, and terminals disagree about how wide it makes a \
                 character",
                code_point(*ch)
            ),
            Self::Emoji(ch) => write!(
                f,
                "{} is an emoji, drawn in colours of its own at a width terminals disagree about",
                code_point(*ch)
            ),
            Self::ZeroWidth => write!(f, "it draws nothing: it takes no cells"),
            Self::Several(count) => {
                write!(f, "it is {count} characters as a terminal draws them, and a glyph is one")
            }
            Self::Width { found, wanted } => {
                let cells = |n: usize| if n == 1 { "1 cell".into() } else { format!("{n} cells") };
                write!(f, "it is {} wide, and this role takes {}", cells(*found), cells(*wanted))
            }
        }
    }
}

/// A glyph that passed [`check`], and whether it keeps its width everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checked {
    /// The cells it takes where ambiguous-width characters are drawn wide, as
    /// terminals set up for Chinese, Japanese or Korean usually draw them.
    pub cells_cjk: usize,
}

/// Whether `text` can be drawn as `glyph`: one grapheme cluster, exactly as
/// wide as the role takes, and nothing whose width depends on the terminal or
/// that would join onto the text drawn beside it.
///
/// Width is measured the way ratatui's diff measures a cell, which is what
/// moves the terminal's cursor. A glyph that is the right width only where
/// ambiguous characters are narrow still passes, since most terminals draw
/// them narrow and the default preset has several; [`Checked::cells_cjk`] says
/// so, for a warning.
///
/// # Errors
///
/// The first thing wrong with it, as a [`Rejection`].
pub fn check(glyph: Glyph, text: &str) -> Result<Checked, Rejection> {
    if text.is_empty() {
        return Err(Rejection::Empty);
    }
    if let Some(ch) = text.chars().find(|&ch| is_control(ch)) {
        return Err(Rejection::Control(ch));
    }
    if let Some(ch) = text.chars().find(|&ch| is_noncharacter(ch)) {
        return Err(Rejection::Noncharacter(ch));
    }
    if let Some(ch) = text.chars().find(|&ch| matches!(ch, '\u{fe0e}' | '\u{fe0f}')) {
        return Err(Rejection::VariationSelector(ch));
    }
    if let Some(ch) = text.chars().find(|&ch| is_emoji(ch)) {
        return Err(Rejection::Emoji(ch));
    }
    let found = usize::from(text.cell_width());
    if found == 0 {
        return Err(Rejection::ZeroWidth);
    }
    let clusters = text.graphemes(true).count();
    if clusters != 1 {
        return Err(Rejection::Several(clusters));
    }
    let wanted = glyph.cells();
    if found != wanted {
        return Err(Rejection::Width { found, wanted });
    }
    // The halfwidth sound marks ratatui gives a cell of their own, it gives
    // one in either width table.
    Ok(Checked { cells_cjk: text.width_cjk() + (found - text.width()) })
}

/// Anything that steers rather than draws: the C0 and C1 controls and DEL;
/// every format character (general category `Cf`: the bidirectional marks and
/// overrides, the zero-width space and joiners, the soft hyphen, the tags);
/// the line and paragraph separators; and the prepended marks, which join
/// onto whatever is drawn after them.
///
/// Terminals give format characters no cell while ratatui gives most of them
/// one, and a joiner at the end of a glyph fuses it with the character drawn
/// next to it, so either would move everything after it.
fn is_control(ch: char) -> bool {
    ch.is_control()
        || matches!(
            u32::from(ch),
            0xad | 0x600..=0x605
                | 0x61c
                | 0x6dd
                | 0x70f
                | 0x890..=0x891
                | 0x8e2
                | 0xd4e
                | 0x180e
                | 0x200b..=0x200f
                | 0x2028..=0x202e
                | 0x2060..=0x2064
                | 0x2066..=0x206f
                | 0xfeff
                | 0xfff9..=0xfffb
                | 0x1_10bd
                | 0x1_10cd
                | 0x1_11c2..=0x1_11c3
                | 0x1_193f
                | 0x1_1941
                | 0x1_1a3a
                | 0x1_1a84..=0x1_1a89
                | 0x1_1d46
                | 0x1_3430..=0x1_343f
                | 0x1_bca0..=0x1_bca3
                | 0x1_d173..=0x1_d17a
                | 0xe_0001
                | 0xe_0020..=0xe_007f
        )
}

/// The sixty-six code points Unicode keeps out of text for good.
const fn is_noncharacter(ch: char) -> bool {
    let point = ch as u32;
    (point >= 0xfdd0 && point <= 0xfdef) || point & 0xfffe == 0xfffe
}

/// Whether `ch` is drawn as an emoji, or makes what it follows one.
///
/// Inside the Basic Multilingual Plane this is `Emoji_Presentation`, a short,
/// fixed list spelled out here; the symbols there that are emoji only when
/// asked (`❤`, `▶`) are left alone, and asking is refused as a variation
/// selector. Outside it, it is every `Extended_Pictographic` character,
/// whatever its default: monospace fonts almost never have them, so a
/// terminal falls back to a colour emoji font, at a width of its choosing.
/// The regional indicators that pair into flags and the keycap mark, which
/// makes `1` an emoji in some terminals and a digit with debris in others,
/// are here too.
const fn is_emoji(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1_f000..=0x1_f0ff
        | 0x1_f10d..=0x1_f10f
        | 0x1_f12f
        | 0x1_f16c..=0x1_f171
        | 0x1_f17e..=0x1_f17f
        | 0x1_f18e
        | 0x1_f191..=0x1_f19a
        | 0x1_f1ad..=0x1_f1ff
        | 0x1_f201..=0x1_f20f
        | 0x1_f21a
        | 0x1_f22f
        | 0x1_f232..=0x1_f23a
        | 0x1_f23c..=0x1_f23f
        | 0x1_f249..=0x1_f53d
        | 0x1_f546..=0x1_f64f
        | 0x1_f680..=0x1_f6ff
        | 0x1_f774..=0x1_f77f
        | 0x1_f7d5..=0x1_f7ff
        | 0x1_f80c..=0x1_f80f
        | 0x1_f848..=0x1_f84f
        | 0x1_f85a..=0x1_f85f
        | 0x1_f888..=0x1_f88f
        | 0x1_f8ae..=0x1_f8ff
        | 0x1_f90c..=0x1_f93a
        | 0x1_f93c..=0x1_f945
        | 0x1_f947..=0x1_faff
        | 0x1_fc00..=0x1_fffd
        | 0x20e3
        | 0x231a..=0x231b
        | 0x23e9..=0x23ec
        | 0x23f0
        | 0x23f3
        | 0x25fd..=0x25fe
        | 0x2614..=0x2615
        | 0x2648..=0x2653
        | 0x267f
        | 0x2693
        | 0x26a1
        | 0x26aa..=0x26ab
        | 0x26bd..=0x26be
        | 0x26c4..=0x26c5
        | 0x26ce
        | 0x26d4
        | 0x26ea
        | 0x26f2..=0x26f3
        | 0x26f5
        | 0x26fa
        | 0x26fd
        | 0x2705
        | 0x270a..=0x270b
        | 0x2728
        | 0x274c
        | 0x274e
        | 0x2753..=0x2755
        | 0x2757
        | 0x2795..=0x2797
        | 0x27b0
        | 0x27bf
        | 0x2b1b..=0x2b1c
        | 0x2b50
        | 0x2b55
    )
}

/// `U+25BE`.
fn code_point(ch: char) -> String {
    format!("U+{:04X}", u32::from(ch))
}

/// `U+0065 U+0301`: every code point in a glyph.
#[must_use]
pub fn code_points(text: &str) -> String {
    text.chars().map(code_point).collect::<Vec<_>>().join(" ")
}

/// Where a glyph in a resolved set came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Preset,
    Yours,
}

/// The glyph for every role, resolved: a preset with any overrides over it.
///
/// Held by [`Palette`](crate::Palette), so every widget that can colour a mark
/// can also draw it, and lookups are an index into an array: nothing to lock,
/// nothing to parse, on the render path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glyphs {
    preset: Preset,
    drawn: [Cow<'static, str>; Glyph::COUNT],
    sources: [Source; Glyph::COUNT],
}

impl Default for Glyphs {
    fn default() -> Self {
        Self::preset(Preset::DEFAULT)
    }
}

impl Glyphs {
    /// Every role as `preset` draws it.
    #[must_use]
    pub fn preset(preset: Preset) -> Self {
        Self {
            preset,
            drawn: std::array::from_fn(|index| Cow::Borrowed(preset.get(Glyph::ALL[index]))),
            sources: [Source::Preset; Glyph::COUNT],
        }
    }

    /// What to draw for `glyph`.
    #[must_use]
    pub fn get(&self, glyph: Glyph) -> &str {
        &self.drawn[glyph.index()]
    }

    /// The preset underneath.
    #[must_use]
    pub const fn preset_in_use(&self) -> Preset {
        self.preset
    }

    /// Whether `glyph` is the user's own rather than the preset's.
    #[must_use]
    pub fn is_yours(&self, glyph: Glyph) -> bool {
        self.sources[glyph.index()] == Source::Yours
    }

    /// Resolve `[glyphs]`: the preset called `preset`, with `overrides`, role
    /// name to glyph, over it.
    ///
    /// Nothing here fails. An unknown preset falls back to the default, and an
    /// unknown role or a glyph [`check`] refuses is skipped, leaving the
    /// preset's glyph where it was; each is reported in
    /// [`Resolution::problems`].
    #[must_use]
    pub fn resolve(preset: &str, overrides: &BTreeMap<String, String>) -> Resolution {
        let mut problems = Vec::new();
        let mut notes = Vec::new();
        let preset = Preset::named(preset).unwrap_or_else(|| {
            let names: Vec<_> = Preset::ALL.iter().map(|preset| preset.name()).collect();
            problems.push(format!(
                "glyphs.preset: no preset called `{preset}`; there are {}",
                names.join(" and ")
            ));
            Preset::DEFAULT
        });
        let mut glyphs = Self::preset(preset);

        for (key, text) in overrides {
            let Some(glyph) = Glyph::from_key(key) else {
                let hint = nearest(key).map_or_else(
                    || " (`nun glyphs` lists them)".to_string(),
                    |near| format!("; did you mean `{near}`?"),
                );
                problems.push(format!("glyphs: no glyph called `{key}`{hint}"));
                continue;
            };
            match check(glyph, text) {
                Ok(checked) => {
                    if checked.cells_cjk != glyph.cells() {
                        notes.push(format!(
                            "glyphs.{key}: {text:?} is {} cells wide where ambiguous characters \
                             are wide, as in most Chinese, Japanese and Korean setups",
                            checked.cells_cjk
                        ));
                    }
                    glyphs.drawn[glyph.index()] = Cow::Owned(text.clone());
                    glyphs.sources[glyph.index()] = Source::Yours;
                }
                Err(why) => problems.push(format!(
                    "glyphs.{key}: {text:?} cannot be used, because {why}; drawing {:?} instead",
                    glyphs.get(glyph)
                )),
            }
        }
        Resolution { glyphs, problems, notes }
    }
}

/// The role name closest to `key`, when one is close enough to be what was
/// meant: a typo or two away, or the same last part (`open` for `fold.open`)
/// when only one role ends that way.
fn nearest(key: &str) -> Option<&'static str> {
    let (distance, near) = Glyph::ALL
        .iter()
        .map(|glyph| (edit_distance(key, glyph.key()), glyph.key()))
        .min_by_key(|&(distance, _)| distance)?;
    if distance <= 2.max(key.chars().count() / 4) {
        return Some(near);
    }
    let last = key.rsplit('.').next().unwrap_or(key);
    let mut ending = Glyph::ALL
        .iter()
        .map(|glyph| glyph.key())
        .filter(|name| name.rsplit('.').next() == Some(last));
    match (ending.next(), ending.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// Levenshtein distance, by char.
fn edit_distance(one: &str, other: &str) -> usize {
    let other: Vec<char> = other.chars().collect();
    let mut previous: Vec<usize> = (0..=other.len()).collect();
    for (row, a) in one.chars().enumerate() {
        let mut current = vec![row + 1];
        for (column, &b) in other.iter().enumerate() {
            let substitute = previous[column] + usize::from(a != b);
            current.push(substitute.min(previous[column + 1] + 1).min(current[column] + 1));
        }
        previous = current;
    }
    previous[other.len()]
}

/// A resolved set of glyphs, and what was wrong with the config it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    /// The glyphs to draw with.
    pub glyphs: Glyphs,
    /// Overrides that were not used, and why: reported like any other config
    /// problem.
    pub problems: Vec<String>,
    /// Overrides that were used but will not keep their width in a terminal
    /// that draws ambiguous characters wide. Said by `nun glyphs` and
    /// `nun config`, not in the editor: the person may know their terminal.
    pub notes: Vec<String>,
}

impl Resolution {
    /// The glyphs in use, one line per area, as `nun config` prints them
    /// under the rest of the configuration.
    #[must_use]
    pub fn summary(&self) -> String {
        let glyphs = &self.glyphs;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "\n# glyphs in use: preset {:?}{}; `nun glyphs` has code points and widths",
            glyphs.preset.name(),
            if Glyph::ALL.iter().any(|&glyph| glyphs.is_yours(glyph)) {
                ", and * marks yours"
            } else {
                ""
            }
        );
        for area in Area::ALL {
            let _ = write!(out, "#   {:<16}", area.title());
            for glyph in Glyph::ALL.into_iter().filter(|glyph| glyph.area() == area) {
                let mark = if glyphs.is_yours(glyph) { "*" } else { "" };
                let _ = write!(out, " {}{mark} {}", glyph.key(), glyphs.get(glyph));
            }
            out.push('\n');
        }
        for line in self.problems.iter().chain(&self.notes) {
            let _ = writeln!(out, "# {line}");
        }
        out
    }

    /// Every role as a Markdown table per area: what `nun glyphs` prints and,
    /// with nothing configured, what `docs/glyphs.md` holds.
    #[must_use]
    pub fn reference(&self) -> String {
        let glyphs = &self.glyphs;
        let preset = glyphs.preset;
        let others: Vec<Preset> =
            Preset::ALL.into_iter().filter(|other| *other != preset).collect();

        let mut out = String::new();
        out.push_str("# Glyphs\n\n");
        out.push_str(
            "Every mark nun draws — the fold arrows, the lightbulb, the rail, the\n\
             sidebar's buttons — has a role, the way every colour it draws has one. A\n\
             preset gives each role a glyph, and `[glyphs]` in `~/.config/nun/nun.toml`\n\
             changes any of them:\n\n\
             ```toml\n\
             [glyphs]\n\
             preset = \"ascii\"   # or \"default\"\n\
             lightbulb = \"?\"    # any role below; the rest keep the preset's glyph\n\
             ```\n\n\
             A glyph must be one character as a terminal draws it, exactly as many\n\
             cells wide as its role takes, which is one for every role here. nun\n\
             refuses control characters, a glyph that draws nothing, emoji, and\n\
             variation selectors, whose width terminals disagree about; it draws the\n\
             preset's glyph instead and `nun config` says why. Cells says \"2 in CJK\"\n\
             where a terminal that draws ambiguous-width characters wide, as most\n\
             Chinese, Japanese and Korean setups do, would give the glyph two cells;\n\
             the `ascii` preset has none of those.\n\n\
             This file is generated from the source by `nun glyphs`. Do not edit it by hand.\n\n",
        );
        let _ = writeln!(out, "Presets:\n");
        for each in Preset::ALL {
            let _ = writeln!(out, "- `{}`: {}.", each.name(), each.about());
        }
        let yours = Glyph::ALL.iter().filter(|&&glyph| glyphs.is_yours(glyph)).count();
        let _ = write!(out, "\nIn use: `{}`", preset.name());
        match yours {
            0 => out.push_str(".\n"),
            1 => out.push_str(", and one glyph of your own, marked *yours*.\n"),
            _ => {
                let _ = writeln!(out, ", and {yours} glyphs of your own, marked *yours*.");
            }
        }

        for area in Area::ALL {
            let _ = write!(out, "\n## {}\n\n| Role | Glyph | Code points | Cells |", area.title());
            for other in &others {
                let _ = write!(out, " {} |", other.name());
            }
            out.push_str(" Description |\n|---|---|---|---|");
            out.push_str(&"---|".repeat(others.len()));
            out.push_str("---|\n");
            for glyph in Glyph::ALL.into_iter().filter(|glyph| glyph.area() == area) {
                let drawn = glyphs.get(glyph);
                let yours = if glyphs.is_yours(glyph) { " *yours*" } else { "" };
                let cjk = drawn.width_cjk();
                let cells = if cjk == drawn.width() {
                    drawn.width().to_string()
                } else {
                    format!("{}, {cjk} in CJK", drawn.width())
                };
                let _ = write!(
                    out,
                    "| `{}` | {}{yours} | {} | {cells} |",
                    glyph.key(),
                    code_span(drawn),
                    code_points(drawn)
                );
                for other in &others {
                    let _ = write!(out, " {} |", code_span(other.get(glyph)));
                }
                let _ = writeln!(out, " {} |", glyph.about());
            }
        }

        if !self.problems.is_empty() || !self.notes.is_empty() {
            out.push_str("\n## Problems\n\n");
            for line in self.problems.iter().chain(&self.notes) {
                let _ = writeln!(out, "- {line}");
            }
        }
        out
    }
}

/// `text` as a Markdown code span that survives a table cell: a backtick gets
/// a longer fence, and a pipe is escaped so it does not end the cell.
fn code_span(text: &str) -> String {
    let text = text.replace('|', "\\|");
    if text.contains('`') { format!("`` {text} ``") } else { format!("`{text}`") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(preset: &str, overrides: &[(&str, &str)]) -> Resolution {
        let overrides =
            overrides.iter().map(|(key, text)| ((*key).to_string(), (*text).to_string())).collect();
        Glyphs::resolve(preset, &overrides)
    }

    #[test]
    fn every_role_is_listed_once_in_the_order_of_its_slot() {
        for (index, glyph) in Glyph::ALL.into_iter().enumerate() {
            assert_eq!(glyph.index(), index, "{glyph:?} is out of place in Glyph::ALL");
        }
    }

    #[test]
    fn role_names_are_unique_round_trip_and_never_prefix_one_another() {
        // `fold` and `fold.open` could not both be set in TOML, where
        // `fold.open = "v"` makes `fold` a table.
        for glyph in Glyph::ALL {
            assert_eq!(Glyph::from_key(glyph.key()), Some(glyph));
            for other in Glyph::ALL.into_iter().filter(|other| *other != glyph) {
                assert_ne!(glyph.key(), other.key());
                assert!(
                    !other.key().starts_with(&format!("{}.", glyph.key())),
                    "`{}` would make `{}` a table",
                    other.key(),
                    glyph.key()
                );
            }
        }
    }

    #[test]
    fn role_names_are_toml_bare_keys_so_they_can_be_written_unquoted() {
        for glyph in Glyph::ALL {
            let bare = glyph.key().split('.').all(|part| {
                !part.is_empty()
                    && part.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            });
            assert!(bare, "{}", glyph.key());
        }
    }

    #[test]
    fn every_area_has_roles_and_every_role_says_what_it_is_for() {
        for area in Area::ALL {
            assert!(Glyph::ALL.iter().any(|glyph| glyph.area() == area), "{area:?} is empty");
        }
        for glyph in Glyph::ALL {
            assert!(!glyph.about().is_empty(), "{glyph:?}");
            assert!(!glyph.about().ends_with('.'), "{glyph:?}: a table cell, not a sentence");
        }
    }

    #[test]
    fn every_glyph_in_every_preset_passes_its_own_check() {
        for preset in Preset::ALL {
            for glyph in Glyph::ALL {
                let text = preset.get(glyph);
                assert!(
                    check(glyph, text).is_ok(),
                    "{} {}: {text:?}: {:?}",
                    preset.name(),
                    glyph.key(),
                    check(glyph, text)
                );
            }
        }
    }

    #[test]
    fn the_ascii_preset_is_ascii_and_keeps_its_width_everywhere() {
        for glyph in Glyph::ALL {
            let text = Preset::ASCII.get(glyph);
            assert!(text.is_ascii(), "{}: {text:?}", glyph.key());
            assert_eq!(text.width_cjk(), glyph.cells(), "{}: {text:?}", glyph.key());
        }
    }

    #[test]
    fn the_default_preset_is_what_nun_drew_before() {
        // A sample across every area; the render tests hold the rest in place.
        let default = Glyphs::default();
        for (glyph, text) in [
            (Glyph::FoldOpen, "▾"),
            (Glyph::FoldClosed, "▸"),
            (Glyph::Lightbulb, "◊"),
            (Glyph::Rail3, "▊"),
            (Glyph::TabClose, "×"),
            (Glyph::DiagnosticWarning, "▲"),
            (Glyph::TreeNewFolder, "▪"),
            (Glyph::SearchApply, "⇓"),
            (Glyph::ReplaceExcluded, "·"),
            (Glyph::CardNext, "›"),
            (Glyph::Ellipsis, "…"),
        ] {
            assert_eq!(default.get(glyph), text, "{}", glyph.key());
        }
    }

    #[test]
    fn the_lightbulb_keeps_one_cell_where_ambiguous_characters_are_wide() {
        // The one mark drawn inside the text's own columns, where a second
        // cell would push the code to the right.
        assert_eq!(check(Glyph::Lightbulb, "◊"), Ok(Checked { cells_cjk: 1 }));
    }

    #[test]
    fn an_override_goes_over_the_preset_and_leaves_the_rest() {
        let resolved = resolve("ascii", &[("fold.open", "▿")]);
        assert!(resolved.problems.is_empty(), "{:?}", resolved.problems);
        assert_eq!(resolved.glyphs.get(Glyph::FoldOpen), "▿");
        assert!(resolved.glyphs.is_yours(Glyph::FoldOpen));
        assert_eq!(resolved.glyphs.get(Glyph::FoldClosed), ">", "the preset's");
        assert!(!resolved.glyphs.is_yours(Glyph::FoldClosed));
        assert_eq!(resolved.glyphs.preset_in_use(), Preset::ASCII);
    }

    #[test]
    fn a_space_is_a_glyph() {
        // The way to draw no lightbulb while keeping its column.
        let resolved = resolve("default", &[("lightbulb", " ")]);
        assert!(resolved.problems.is_empty(), "{:?}", resolved.problems);
        assert_eq!(resolved.glyphs.get(Glyph::Lightbulb), " ");
    }

    #[test]
    fn a_combining_sequence_is_one_glyph() {
        assert_eq!(check(Glyph::FoldOpen, "e\u{301}"), Ok(Checked { cells_cjk: 1 }));
    }

    #[test]
    fn an_unknown_preset_falls_back_to_the_default_and_names_the_real_ones() {
        let resolved = resolve("nerd", &[]);
        assert_eq!(resolved.glyphs, Glyphs::default());
        assert_eq!(resolved.problems.len(), 1);
        assert!(
            resolved.problems[0].contains("no preset called `nerd`"),
            "{:?}",
            resolved.problems
        );
        assert!(resolved.problems[0].contains("default and ascii"), "{:?}", resolved.problems);
    }

    #[test]
    fn an_unknown_role_is_reported_with_what_it_probably_meant() {
        for (typo, meant) in [
            ("fold.opne", "fold.open"),
            ("tab.closed", "tab.close"),
            ("rail1", "rail.1"),
            ("symlink", "tree.symlink"),
        ] {
            let resolved = resolve("default", &[(typo, "x")]);
            assert_eq!(resolved.problems.len(), 1, "{typo}");
            let problem = &resolved.problems[0];
            assert!(problem.contains(&format!("no glyph called `{typo}`")), "{problem}");
            assert!(problem.contains(&format!("did you mean `{meant}`?")), "{problem}");
        }
    }

    #[test]
    fn a_role_nothing_resembles_points_at_the_list() {
        let resolved = resolve("default", &[("frobnicate.widget", "x")]);
        assert!(!resolved.problems[0].contains("did you mean"), "{:?}", resolved.problems);
        assert!(resolved.problems[0].contains("nun glyphs"), "{:?}", resolved.problems);
    }

    #[test]
    fn a_refused_override_is_reported_and_the_preset_stands_in() {
        let resolved = resolve("default", &[("fold.open", "😀")]);
        assert_eq!(resolved.glyphs.get(Glyph::FoldOpen), "▾");
        assert!(!resolved.glyphs.is_yours(Glyph::FoldOpen));
        let problem = &resolved.problems[0];
        assert!(problem.starts_with("glyphs.fold.open: \"😀\" cannot be used"), "{problem}");
        assert!(problem.contains("U+1F600 is an emoji"), "{problem}");
        assert!(problem.contains("drawing \"▾\" instead"), "{problem}");
    }

    #[test]
    fn an_empty_glyph_is_refused() {
        assert_eq!(check(Glyph::Ellipsis, ""), Err(Rejection::Empty));
    }

    #[test]
    fn control_and_format_characters_are_refused() {
        for (text, ch) in [
            ("\t", '\t'),
            ("\u{1b}", '\u{1b}'),
            ("a\u{7f}", '\u{7f}'),
            ("\u{85}", '\u{85}'),
            ("\u{202e}x", '\u{202e}'),
            ("x\u{2066}", '\u{2066}'),
            ("\u{200f}", '\u{200f}'),
            ("\u{2028}", '\u{2028}'),
            ("\u{200b}", '\u{200b}'),
            ("\u{feff}", '\u{feff}'),
            ("\u{ad}", '\u{ad}'),
            // A joiner at the end fuses the glyph with a pictograph after it.
            ("\u{263a}\u{200d}", '\u{200d}'),
            // A prepended mark joins onto whatever follows it.
            ("\u{600}", '\u{600}'),
            ("\u{110bd}", '\u{110bd}'),
            // Tag characters, which ride along after a flag.
            ("a\u{e0041}", '\u{e0041}'),
        ] {
            assert_eq!(check(Glyph::Ellipsis, text), Err(Rejection::Control(ch)), "{text:?}");
        }
    }

    #[test]
    fn noncharacters_are_refused() {
        for ch in ['\u{fdd0}', '\u{fffe}', '\u{ffff}', '\u{1fffe}'] {
            assert_eq!(check(Glyph::Ellipsis, &ch.to_string()), Err(Rejection::Noncharacter(ch)));
        }
    }

    #[test]
    fn variation_selectors_are_refused_either_way() {
        // U+2764 is a text heart by default and an emoji with VS16; which one
        // a terminal draws, and at what width, is its own business.
        assert_eq!(
            check(Glyph::Ellipsis, "\u{2764}\u{fe0f}"),
            Err(Rejection::VariationSelector('\u{fe0f}'))
        );
        assert_eq!(
            check(Glyph::Ellipsis, "\u{25b6}\u{fe0e}"),
            Err(Rejection::VariationSelector('\u{fe0e}'))
        );
    }

    #[test]
    fn emoji_are_refused_whatever_their_width() {
        // U+1F321 and U+1F5E8 default to text, and are one cell to
        // unicode-width, but no monospace font has them.
        for text in [
            "😀",
            "\u{2705}",
            "\u{26a1}",
            "\u{2b50}",
            "🇮🇸",
            "\u{1f1ee}",
            "1\u{20e3}",
            "\u{1f321}",
            "\u{1f5e8}",
            "\u{1f170}",
        ] {
            let first = text.chars().find(|&ch| is_emoji(ch)).unwrap();
            assert_eq!(check(Glyph::Ellipsis, text), Err(Rejection::Emoji(first)), "{text:?}");
        }
    }

    #[test]
    fn text_presentation_symbols_next_to_emoji_are_not_mistaken_for_them() {
        for text in ["\u{2764}", "\u{2714}", "\u{25b6}", "\u{2603}"] {
            assert!(
                check(Glyph::Ellipsis, text).is_ok(),
                "{text:?}: {:?}",
                check(Glyph::Ellipsis, text)
            );
        }
    }

    #[test]
    fn something_that_draws_nothing_is_refused() {
        for text in ["\u{301}", "\u{301}\u{301}", "\u{1160}", "\u{3164}"] {
            assert_eq!(check(Glyph::Ellipsis, text), Err(Rejection::ZeroWidth), "{text:?}");
        }
    }

    #[test]
    fn more_than_one_character_is_refused() {
        assert_eq!(check(Glyph::Ellipsis, "..."), Err(Rejection::Several(3)));
        assert_eq!(check(Glyph::Ellipsis, "ab"), Err(Rejection::Several(2)));
        assert_eq!(check(Glyph::Ellipsis, "a\u{3164}"), Err(Rejection::Several(2)));
    }

    #[test]
    fn a_wide_character_is_refused_for_a_one_cell_role() {
        assert_eq!(check(Glyph::TabClose, "中"), Err(Rejection::Width { found: 2, wanted: 1 }));
        assert_eq!(
            check(Glyph::TabClose, "\u{ff38}"),
            Err(Rejection::Width { found: 2, wanted: 1 }),
            "a fullwidth X"
        );
        // unicode-width counts the sound mark as nothing; the terminal, and
        // ratatui with it, give it a cell of its own.
        assert_eq!(
            check(Glyph::TabClose, "\u{ff76}\u{ff9e}"),
            Err(Rejection::Width { found: 2, wanted: 1 })
        );
    }

    #[test]
    fn an_ambiguous_width_override_is_used_and_noted() {
        let resolved = resolve("default", &[("tab.modified", "○")]);
        assert!(resolved.problems.is_empty(), "{:?}", resolved.problems);
        assert_eq!(resolved.glyphs.get(Glyph::TabModified), "○");
        assert_eq!(resolved.notes.len(), 1, "{:?}", resolved.notes);
        assert!(resolved.notes[0].contains("2 cells wide where ambiguous"), "{:?}", resolved.notes);
    }

    #[test]
    fn rejections_explain_themselves() {
        let reasons = [
            (Rejection::Empty, "empty"),
            (Rejection::Control('\t'), "U+0009 is a control or format character"),
            (Rejection::Noncharacter('\u{ffff}'), "U+FFFF is a noncharacter"),
            (Rejection::VariationSelector('\u{fe0f}'), "U+FE0F is a variation selector"),
            (Rejection::Emoji('😀'), "U+1F600 is an emoji"),
            (Rejection::ZeroWidth, "draws nothing"),
            (Rejection::Several(3), "3 characters"),
            (Rejection::Width { found: 2, wanted: 1 }, "2 cells wide, and this role takes 1 cell"),
        ];
        for (rejection, said) in reasons {
            assert!(rejection.to_string().contains(said), "{rejection}");
        }
    }

    #[test]
    fn a_code_span_survives_a_table_cell() {
        assert_eq!(code_span("|"), "`\\|`");
        assert_eq!(code_span("`"), "`` ` ``");
        assert_eq!(code_span("▾"), "`▾`");
    }

    #[test]
    fn code_points_are_listed_for_every_char_in_a_glyph() {
        assert_eq!(code_points("▾"), "U+25BE");
        assert_eq!(code_points("e\u{301}"), "U+0065 U+0301");
    }

    #[test]
    fn the_reference_marks_yours_and_says_what_was_refused() {
        let resolved = resolve("ascii", &[("fold.open", "▿"), ("fold.closed", "\t")]);
        let reference = resolved.reference();
        assert!(reference.contains("In use: `ascii`, and one glyph of your own"), "{reference}");
        assert!(reference.contains("| `fold.open` | `▿` *yours* | U+25BF |"), "{reference}");
        assert!(
            reference.contains("| Role | Glyph | Code points | Cells | default |"),
            "{reference}"
        );
        assert!(reference.contains("## Problems"), "{reference}");
        assert!(reference.contains("glyphs.fold.closed: \"\\t\" cannot be used"), "{reference}");
    }

    #[test]
    fn the_summary_lists_every_role_once() {
        let summary = resolve("default", &[("lightbulb", "?")]).summary();
        for glyph in Glyph::ALL {
            assert_eq!(summary.matches(&format!(" {}", glyph.key())).count(), 1, "{}", glyph.key());
        }
        assert!(summary.contains(" lightbulb* ?"), "{summary}");
    }

    #[test]
    fn the_documented_glyphs_match_the_code() {
        let documented = include_str!("../../../docs/glyphs.md");
        assert_eq!(
            documented,
            Resolution::default().reference(),
            "docs/glyphs.md is stale; regenerate it with `nun glyphs > docs/glyphs.md` and no \
             [glyphs] in your config"
        );
    }
}
