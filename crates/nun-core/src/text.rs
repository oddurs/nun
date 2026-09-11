//! Line endings, byte-order marks, and what was learned while loading a file.

/// The line terminator a file uses on disk.
///
/// Buffers always hold `\n` internally so that every index calculation has one
/// shape; the original ending is reapplied on save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// `\n` — the default for a new or ambiguous file.
    #[default]
    Lf,
    /// `\r\n`.
    Crlf,
}

impl LineEnding {
    /// The characters this ending writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }

    /// Pick the ending a piece of text predominantly uses.
    ///
    /// Ties and empty input give [`LineEnding::Lf`]. A file with mixed endings
    /// is reported through [`LoadReport::mixed_line_endings`] rather than being
    /// silently rewritten.
    #[must_use]
    pub fn detect(text: &str) -> Self {
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count() - crlf;
        if crlf > lf { Self::Crlf } else { Self::Lf }
    }
}

/// The UTF-8 byte-order mark, preserved verbatim when a file has one.
pub(crate) const BOM: &str = "\u{feff}";

/// What loading a file turned up that the caller should know about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoadReport {
    /// The file began with a UTF-8 byte-order mark, which will be written back.
    pub had_bom: bool,
    /// The file was not valid UTF-8 and invalid sequences were replaced.
    ///
    /// Saving such a buffer would destroy the original bytes, so callers should
    /// surface this rather than treating the load as clean.
    pub lossy: bool,
    /// Both `\n` and `\r\n` appeared. Saving normalises to the dominant one.
    pub mixed_line_endings: bool,
    /// The ending that will be used on save.
    pub line_ending: LineEnding,
}
