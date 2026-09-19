//! From keystrokes to commands.
//!
//! A [`Keymap`] maps key sequences to commands. Most sequences are one key; a
//! chord like `Cmd+K Cmd+T` is two. [`Chords`] holds the keys typed so far
//! while they could still be the start of a chord, and resolves them when the
//! next key arrives or when the chord times out.
//!
//! The map is ordered by sequence, so every sequence that starts with a given
//! prefix sits in one contiguous run after it. "Is this the start of a longer
//! binding?" is one range lookup rather than a walk — the same answer a trie
//! would give, from a structure the standard library already has.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::keys::Key;

/// Key sequences bound to commands.
#[derive(Debug, Clone)]
pub struct Keymap<C> {
    bindings: BTreeMap<Vec<Key>, C>,
}

impl<C> Default for Keymap<C> {
    fn default() -> Self {
        Self { bindings: BTreeMap::new() }
    }
}

impl<C: Clone> Keymap<C> {
    /// An empty keymap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `sequence` to `command`, replacing whatever it was bound to.
    ///
    /// User bindings are added this way over the defaults, so they are
    /// additive: a user binding for one sequence leaves every other default in
    /// place.
    pub fn bind(&mut self, sequence: Vec<Key>, command: C) {
        if !sequence.is_empty() {
            self.bindings.insert(sequence, command);
        }
    }

    /// The command bound to exactly `sequence`.
    #[must_use]
    pub fn get(&self, sequence: &[Key]) -> Option<&C> {
        self.bindings.get(sequence)
    }

    /// Whether some longer binding starts with `sequence`.
    #[must_use]
    pub fn continues(&self, sequence: &[Key]) -> bool {
        self.bindings
            .range(sequence.to_vec()..)
            .find(|(bound, _)| bound.as_slice() != sequence)
            .is_some_and(|(bound, _)| bound.starts_with(sequence))
    }

    /// Every sequence bound to `command`, shortest first.
    ///
    /// The palette shows the first one next to each command, which makes the
    /// palette the keymap reference and leaves nothing separate to document.
    #[must_use]
    pub fn sequences_for(&self, command: &C) -> Vec<&[Key]>
    where
        C: PartialEq,
    {
        let mut found: Vec<&[Key]> = self
            .bindings
            .iter()
            .filter(|(_, bound)| *bound == command)
            .map(|(sequence, _)| sequence.as_slice())
            .collect();
        found.sort_by_key(|sequence| sequence.len());
        found
    }

    /// Every binding, in sequence order.
    pub fn iter(&self) -> impl Iterator<Item = (&[Key], &C)> {
        self.bindings.iter().map(|(sequence, command)| (sequence.as_slice(), command))
    }
}

/// What a keystroke came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved<C> {
    /// Run this command.
    Command(C),
    /// The keys so far are the start of a chord; wait for the next one.
    Pending,
    /// The keys led nowhere. A single unbound key is the caller's to handle —
    /// it is usually text to type. A longer run is a chord that does not exist.
    Unbound(Vec<Key>),
}

/// The keys of a chord typed so far.
#[derive(Debug, Clone)]
pub struct Chords {
    pending: Vec<Key>,
    since: Option<Instant>,
    timeout: Duration,
}

impl Chords {
    /// A resolver that gives up on an unfinished chord after `timeout`.
    #[must_use]
    pub const fn new(timeout: Duration) -> Self {
        Self { pending: Vec::new(), since: None, timeout }
    }

    /// The keys typed so far of an unfinished chord.
    #[must_use]
    pub fn pending(&self) -> &[Key] {
        &self.pending
    }

    /// When an unfinished chord gives up, if one is in progress.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.since.map(|since| since + self.timeout)
    }

    /// Feed one keystroke.
    ///
    /// Usually one result, occasionally two: when a chord in progress is
    /// broken by a key that does not continue it, the prefix's own binding runs
    /// if it has one, and the new key is then resolved from scratch — so
    /// `Ctrl+K` bound on its own still works when followed by an unrelated key.
    pub fn feed<C: Clone>(
        &mut self,
        keymap: &Keymap<C>,
        key: Key,
        now: Instant,
    ) -> Vec<Resolved<C>> {
        let mut sequence = std::mem::take(&mut self.pending);
        sequence.push(key);
        self.since = None;

        if keymap.continues(&sequence) {
            self.pending = sequence;
            self.since = Some(now);
            return vec![Resolved::Pending];
        }
        if let Some(command) = keymap.get(&sequence) {
            return vec![Resolved::Command(command.clone())];
        }

        let prefix = &sequence[..sequence.len() - 1];
        if prefix.is_empty() {
            return vec![Resolved::Unbound(sequence)];
        }
        match keymap.get(prefix) {
            Some(command) => {
                let mut out = vec![Resolved::Command(command.clone())];
                out.extend(self.feed(keymap, key, now));
                out
            }
            None => vec![Resolved::Unbound(sequence)],
        }
    }

    /// The deadline passed with no further key.
    ///
    /// An unfinished chord whose prefix is itself bound runs that binding —
    /// the fallback that lets `Ctrl+K` alone mean something while
    /// `Ctrl+K Ctrl+T` means something else. One that is not bound is dropped
    /// and reported, rather than left waiting forever.
    pub fn expire<C: Clone>(&mut self, keymap: &Keymap<C>, now: Instant) -> Option<Resolved<C>> {
        let deadline = self.deadline()?;
        if now < deadline {
            return None;
        }
        self.since = None;
        let sequence = std::mem::take(&mut self.pending);
        Some(match keymap.get(&sequence) {
            Some(command) => Resolved::Command(command.clone()),
            None => Resolved::Unbound(sequence),
        })
    }

    /// Forget an unfinished chord.
    pub fn cancel(&mut self) {
        self.pending.clear();
        self.since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{parse_key, parse_sequence};

    const TIMEOUT: Duration = Duration::from_millis(800);

    fn keymap(bindings: &[(&str, &'static str)]) -> Keymap<&'static str> {
        let mut map = Keymap::new();
        for (sequence, command) in bindings {
            map.bind(parse_sequence(sequence).unwrap(), *command);
        }
        map
    }

    fn key(text: &str) -> Key {
        parse_key(text).unwrap()
    }

    #[test]
    fn a_single_key_binding_runs_at_once() {
        let map = keymap(&[("ctrl+s", "save")]);
        let mut chords = Chords::new(TIMEOUT);
        assert_eq!(
            chords.feed(&map, key("ctrl+s"), Instant::now()),
            vec![Resolved::Command("save")]
        );
        assert_eq!(chords.deadline(), None);
    }

    #[test]
    fn an_unbound_key_is_handed_back() {
        let map = keymap(&[("ctrl+s", "save")]);
        let mut chords = Chords::new(TIMEOUT);
        assert_eq!(
            chords.feed(&map, key("a"), Instant::now()),
            vec![Resolved::Unbound(vec![key("a")])]
        );
    }

    #[test]
    fn a_chord_waits_for_its_second_key() {
        let map = keymap(&[("ctrl+k ctrl+t", "theme")]);
        let mut chords = Chords::new(TIMEOUT);
        let now = Instant::now();

        assert_eq!(chords.feed(&map, key("ctrl+k"), now), vec![Resolved::Pending]);
        assert_eq!(chords.pending(), &[key("ctrl+k")]);
        assert_eq!(chords.deadline(), Some(now + TIMEOUT));
        assert_eq!(chords.feed(&map, key("ctrl+t"), now), vec![Resolved::Command("theme")]);
        assert!(chords.pending().is_empty());
    }

    #[test]
    fn a_chord_times_out_to_its_prefix_binding() {
        let map = keymap(&[("ctrl+k", "kill line"), ("ctrl+k ctrl+t", "theme")]);
        let mut chords = Chords::new(TIMEOUT);
        let now = Instant::now();

        assert_eq!(chords.feed(&map, key("ctrl+k"), now), vec![Resolved::Pending]);
        assert_eq!(chords.expire(&map, now + TIMEOUT / 2), None, "not yet");
        assert_eq!(chords.expire(&map, now + TIMEOUT), Some(Resolved::Command("kill line")));
        assert_eq!(chords.deadline(), None);
    }

    #[test]
    fn a_chord_with_no_prefix_binding_times_out_to_nothing() {
        let map = keymap(&[("ctrl+k ctrl+t", "theme")]);
        let mut chords = Chords::new(TIMEOUT);
        let now = Instant::now();
        chords.feed(&map, key("ctrl+k"), now);
        assert_eq!(
            chords.expire(&map, now + TIMEOUT),
            Some(Resolved::Unbound(vec![key("ctrl+k")]))
        );
    }

    #[test]
    fn breaking_a_chord_runs_the_prefix_then_the_new_key() {
        let map =
            keymap(&[("ctrl+k", "kill line"), ("ctrl+k ctrl+t", "theme"), ("ctrl+s", "save")]);
        let mut chords = Chords::new(TIMEOUT);
        let now = Instant::now();
        chords.feed(&map, key("ctrl+k"), now);
        assert_eq!(
            chords.feed(&map, key("ctrl+s"), now),
            vec![Resolved::Command("kill line"), Resolved::Command("save")]
        );
    }

    #[test]
    fn breaking_a_chord_whose_prefix_is_unbound_reports_the_whole_chord() {
        let map = keymap(&[("ctrl+k ctrl+t", "theme")]);
        let mut chords = Chords::new(TIMEOUT);
        let now = Instant::now();
        chords.feed(&map, key("ctrl+k"), now);
        assert_eq!(
            chords.feed(&map, key("x"), now),
            vec![Resolved::Unbound(vec![key("ctrl+k"), key("x")])]
        );
        assert!(chords.pending().is_empty(), "and the chord is over");
    }

    #[test]
    fn three_key_chords_work() {
        let map = keymap(&[("ctrl+k ctrl+k ctrl+k", "triple")]);
        let mut chords = Chords::new(TIMEOUT);
        let now = Instant::now();
        assert_eq!(chords.feed(&map, key("ctrl+k"), now), vec![Resolved::Pending]);
        assert_eq!(chords.feed(&map, key("ctrl+k"), now), vec![Resolved::Pending]);
        assert_eq!(chords.feed(&map, key("ctrl+k"), now), vec![Resolved::Command("triple")]);
    }

    #[test]
    fn a_later_binding_replaces_an_earlier_one_for_the_same_keys_only() {
        let mut map = keymap(&[("ctrl+s", "save"), ("ctrl+q", "quit")]);
        map.bind(parse_sequence("ctrl+s").unwrap(), "save all");
        assert_eq!(map.get(&[key("ctrl+s")]), Some(&"save all"));
        assert_eq!(map.get(&[key("ctrl+q")]), Some(&"quit"), "the rest of the defaults survive");
    }

    #[test]
    fn continuation_is_about_prefixes_not_neighbours() {
        let map = keymap(&[("ctrl+k ctrl+t", "theme"), ("ctrl+l", "line")]);
        assert!(map.continues(&[key("ctrl+k")]));
        assert!(!map.continues(&[key("ctrl+j")]), "a sequence sorting just before is not a prefix");
        assert!(
            !map.continues(&[key("ctrl+k"), key("ctrl+t")]),
            "a complete chord continues nowhere"
        );
    }

    #[test]
    fn sequences_for_a_command_come_shortest_first() {
        let map = keymap(&[("ctrl+k ctrl+s", "save"), ("ctrl+s", "save"), ("cmd+s", "save")]);
        let found = map.sequences_for(&"save");
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].len(), 1);
        assert_eq!(found[2].len(), 2);
    }

    #[test]
    fn cancelling_forgets_the_chord() {
        let map = keymap(&[("ctrl+k ctrl+t", "theme")]);
        let mut chords = Chords::new(TIMEOUT);
        chords.feed(&map, key("ctrl+k"), Instant::now());
        chords.cancel();
        assert_eq!(chords.deadline(), None);
        assert!(chords.pending().is_empty());
    }
}
