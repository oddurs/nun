//! From capture names to semantic roles.
//!
//! Capture names belong to grammars and there is no agreed list of them: one
//! says `function.method`, another `function.method.builtin`, a third invents
//! `function.macro`. Binding them to colours directly would put a theme back
//! inside the editor, which is the thing being avoided — so they resolve to
//! roles, and roles come from the ramp derived from the terminal.
//!
//! Resolution is longest-prefix: `@function.method.builtin` tries
//! `function.method.builtin`, then `function.method`, then `function`. A name
//! nobody has thought of lands on body text, which is legible rather than
//! invisible.

use nun_theme::Role;

/// The table. Longest names first is not required — resolution trims the name
/// rather than searching — but keeping families together makes it readable.
const ROLES: &[(&str, Role)] = &[
    // Keywords and the words that behave like them.
    ("keyword", Role::Keyword),
    ("conditional", Role::Keyword),
    ("repeat", Role::Keyword),
    ("include", Role::Keyword),
    ("exception", Role::Keyword),
    ("storageclass", Role::Keyword),
    ("tag", Role::Keyword),
    // Types.
    ("type", Role::Type),
    ("constructor", Role::Type),
    ("namespace", Role::Type),
    ("module", Role::Type),
    ("attribute", Role::Type),
    ("attribute.builtin", Role::Type),
    // Text that is data.
    ("string", Role::StringLiteral),
    ("character", Role::StringLiteral),
    ("string.escape", Role::Number),
    ("escape", Role::Number),
    ("number", Role::Number),
    ("float", Role::Number),
    ("boolean", Role::Number),
    ("constant", Role::Number),
    // Things that are called.
    ("function", Role::Function),
    ("method", Role::Function),
    ("constructor.function", Role::Function),
    // Notes to the reader.
    ("comment", Role::Comment),
    ("comment.documentation", Role::Comment),
    // The scaffolding between everything else.
    ("punctuation", Role::Punctuation),
    ("operator", Role::Punctuation),
    ("delimiter", Role::Punctuation),
    ("bracket", Role::Punctuation),
    // Names of things.
    ("variable", Role::Text),
    ("variable.parameter", Role::Dim),
    ("parameter", Role::Dim),
    ("property", Role::Text),
    ("field", Role::Text),
    ("label", Role::Accent),
    // Markup, for the languages that have it.
    ("text", Role::Text),
    ("emphasis", Role::Text),
    ("title", Role::Accent),
    ("uri", Role::Accent),
    ("link", Role::Accent),
    // What a grammar says when it cannot make sense of something.
    ("error", Role::Error),
    ("warning", Role::Warn),
    ("note", Role::Info),
];

/// What body text is, for anything the table does not name.
pub const FALLBACK: Role = Role::Text;

/// The role a capture takes.
///
/// Total on purpose: every capture resolves to something, so a grammar nun has
/// never seen still renders legibly rather than in whatever the terminal was
/// last set to.
#[must_use]
pub fn role_of(capture: &str) -> Role {
    let mut name = capture;
    loop {
        if let Some((_, role)) = ROLES.iter().find(|(known, _)| *known == name) {
            return *role;
        }
        match name.rfind('.') {
            Some(dot) => name = &name[..dot],
            None => return FALLBACK,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_obvious_captures_land_where_you_would_expect() {
        assert_eq!(role_of("keyword"), Role::Keyword);
        assert_eq!(role_of("string"), Role::StringLiteral);
        assert_eq!(role_of("comment"), Role::Comment);
        assert_eq!(role_of("function"), Role::Function);
        assert_eq!(role_of("type"), Role::Type);
        assert_eq!(role_of("number"), Role::Number);
        assert_eq!(role_of("punctuation.bracket"), Role::Punctuation);
    }

    #[test]
    fn a_long_name_resolves_through_its_family() {
        // Nothing names this one, but its family does.
        assert_eq!(role_of("function.method.builtin"), Role::Function);
        assert_eq!(role_of("keyword.control.conditional"), Role::Keyword);
        assert_eq!(role_of("string.special.symbol"), Role::StringLiteral);
    }

    #[test]
    fn the_more_specific_entry_wins_over_its_family() {
        assert_eq!(role_of("variable"), Role::Text);
        assert_eq!(role_of("variable.parameter"), Role::Dim, "parameters are quieter");
        assert_eq!(role_of("string"), Role::StringLiteral);
        assert_eq!(role_of("string.escape"), Role::Number, "an escape is not more string");
    }

    #[test]
    fn a_capture_nobody_has_thought_of_is_still_legible() {
        assert_eq!(role_of("brand.new.thing"), FALLBACK);
        assert_eq!(role_of(""), FALLBACK);
        assert_eq!(role_of("."), FALLBACK);
    }

    #[test]
    fn every_capture_in_every_shipped_grammar_resolves() {
        // Total by construction, so this is about the table being useful: most
        // of what the grammars actually emit should land on a role of its own
        // rather than on body text.
        let mut named = 0;
        let mut total = 0;
        for language in nun_syntax::all() {
            for capture in language.capture_names() {
                total += 1;
                if role_of(capture) != FALLBACK {
                    named += 1;
                }
            }
        }
        assert!(total > 50, "the grammars have captures to map");
        assert!(
            named * 100 / total >= 70,
            "only {named} of {total} captures have a role of their own"
        );
    }

    #[test]
    fn the_roles_used_are_the_ones_the_ramp_derives() {
        // Every role in the table has to be one the theme actually produces,
        // or syntax would ask for a colour nobody has mixed.
        for (_, role) in ROLES {
            assert!(Role::ALL.contains(role), "{role:?} is not in the ramp");
        }
    }
}
