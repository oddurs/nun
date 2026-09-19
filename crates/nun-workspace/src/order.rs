//! The order names appear in within a directory.

use std::cmp::Ordering;
use std::iter::Peekable;
use std::str::Chars;

/// Compare two file names the way a person expects to read them listed.
///
/// Case-insensitive, so `Makefile` does not sort away from `main.rs`, and
/// natural, so `file2` comes before `file10`. Runs of ASCII digits compare by
/// value; everything else compares by its lowercase form, character by
/// character. Names that differ only in case or leading zeros fall back to a
/// plain byte comparison, which makes this a total order: no two distinct names
/// compare equal, so a sort is stable across refreshes and never shuffles rows
/// under the pointer.
///
/// Non-ASCII names compare by code point after lowercasing. That is not a
/// locale collation, and is not meant to be: it is predictable, cheap, and the
/// same on every machine.
#[must_use]
pub fn compare_names(a: &str, b: &str) -> Ordering {
    natural(a, b).then_with(|| a.cmp(b))
}

fn natural(a: &str, b: &str) -> Ordering {
    let mut a = a.chars().peekable();
    let mut b = b.chars().peekable();
    loop {
        let (x, y) = match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => (x, y),
        };
        let ordering = if x.is_ascii_digit() && y.is_ascii_digit() {
            compare_digit_runs(&digit_run(&mut a), &digit_run(&mut b))
        } else {
            a.next();
            b.next();
            x.to_lowercase().cmp(y.to_lowercase())
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
}

fn digit_run(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut run = String::new();
    while let Some(c) = chars.next_if(char::is_ascii_digit) {
        run.push(c);
    }
    run
}

/// Compare two runs of digits by value without parsing them, so a name with a
/// forty-digit number in it cannot overflow anything.
fn compare_digit_runs(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut names: Vec<String> = names.iter().map(ToString::to_string).collect();
        names.sort_by(|a, b| compare_names(a, b));
        names
    }

    #[test]
    fn case_does_not_separate_names() {
        assert_eq!(
            sorted(&["b.rs", "Makefile", "a.rs", "README.md"]),
            ["a.rs", "b.rs", "Makefile", "README.md"]
        );
    }

    #[test]
    fn numbers_compare_by_value() {
        assert_eq!(
            sorted(&["file10", "file2", "file1", "file02"]),
            ["file1", "file02", "file2", "file10"]
        );
    }

    #[test]
    fn names_differing_only_in_case_still_have_an_order() {
        assert_eq!(compare_names("A", "a"), "A".cmp("a"));
        assert_ne!(compare_names("Readme", "README"), Ordering::Equal);
    }

    #[test]
    fn non_ascii_names_sort_predictably() {
        assert_eq!(
            sorted(&["日本.txt", "émoji-🦀.rs", "Zeta", "alpha", "Écrit"]),
            ["alpha", "Zeta", "Écrit", "émoji-🦀.rs", "日本.txt"]
        );
    }

    #[test]
    fn enormous_numbers_do_not_overflow() {
        let big = format!("v{}", "9".repeat(60));
        assert_eq!(compare_names("v1", &big), Ordering::Less);
    }

    fn name() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just("a"),
                Just("A"),
                Just("b"),
                Just("0"),
                Just("1"),
                Just("9"),
                Just("10"),
                Just("."),
                Just("-"),
                Just("É"),
                Just("é"),
                Just("日"),
                Just("🦀"),
                Just("İ"),
            ],
            0..8,
        )
        .prop_map(|parts| parts.concat())
    }

    proptest! {
        /// Only identical names compare equal, and swapping the arguments
        /// reverses the answer.
        #[test]
        fn the_order_is_antisymmetric(a in name(), b in name()) {
            let forward = compare_names(&a, &b);
            prop_assert_eq!(forward, compare_names(&b, &a).reverse());
            prop_assert_eq!(forward == Ordering::Equal, a == b);
        }

        /// After sorting, every earlier name compares at most equal to every
        /// later one, which fails if the order is not transitive.
        #[test]
        fn the_order_is_transitive(mut names in proptest::collection::vec(name(), 0..12)) {
            names.sort_by(|a, b| compare_names(a, b));
            for (i, earlier) in names.iter().enumerate() {
                for later in &names[i..] {
                    prop_assert_ne!(compare_names(earlier, later), Ordering::Greater);
                }
            }
        }
    }
}
