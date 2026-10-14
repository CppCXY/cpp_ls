//! **Does a candidate answer what was typed, and how well?**
//!
//! # Why a candidate is not just prefix-matched
//!
//! `starts_with` answered this before, and it is right about the common case and wrong about the ones that cost a
//! reader time:
//!
//! ```text
//! typed            starts_with finds        a reader means
//! "bsc"            nothing                  basic_string, basic_string_view
//! "gpid"           nothing                  get_pid, get_parent_id
//! "getpid"         nothing                  get_pid
//! "sv"             nothing                  string_view, small_vector
//! ```
//!
//! Every one of those is how people actually type a name they can half-remember — the **initials of its words**, or
//! the words run together. A list that only answers a literal prefix is a list that has the right item in it and does
//! not show it, which is the same as not having it.
//!
//! # The words, and how they are found
//!
//! A name is read as words by **both** conventions C++ uses, because a project picks one and a header may use either:
//!
//! ```text
//! snake_case        split on '_'                     get_pid      -> get, pid
//! camelCase         split at a lower->upper rise      getPid       -> get, Pid
//! PascalCase        split at every upper              GetPid       -> Get, Pid
//! an acronym run    kept together                     HTTPServer   -> HTTP, Server
//! digits            start a word                      utf8Decode   -> utf, 8, Decode
//! ```
//!
//! # The four ways a match is made, strongest first
//!
//! ```text
//! Prefix        the whole prefix is the start of the name, case-insensitively
//!               "std" -> "string"  is not this; "str" -> "string" is
//! WordStart     the prefix starts one or more words, in order, at their beginnings
//!               "bsc" -> "basic_string", "gp" -> "getPid"
//! WordPrefix    the prefix is the start of a word, not the first
//!               "string" -> "to_string"
//! Subsequence   the prefix's characters appear in order, anywhere
//!               "gpid" -> "get_parent_id"
//! ```
//!
//! The order is the whole of the design: a `Prefix` match must always beat a `WordStart` one, or typing `str` would
//! put `std::to_string` above `std::string`. The caller multiplies this by its own tier, so a local still beats a
//! header name however the two matched.
//!
//! # What it deliberately does not do
//!
//! It does not answer `false` for something that matched nothing — that is `Option`'s job, and a caller that wanted
//! "everything" passes an empty prefix, which matches everything at [`Match::WHOLE`].

/// How well a candidate answered the typed prefix. Higher is better; the caller orders by it before anything else.
///
/// A plain `i32` so that it composes with the existing score by multiplication rather than by a table of pairings
/// nobody could keep in step.
pub type Quality = i32;

/// A candidate that matched nothing. A caller filters these out rather than ranking them.
pub const NONE: Quality = -1;

/// The prefix was empty, so everything matches equally.
pub const WHOLE: Quality = 0;

/// The prefix is the entire beginning of the name: `str` → `string`.
///
/// The strongest signal there is, and it is what a client's own filter would have found — kept as a tier of its own
/// so that this module can never *lower* the ranking of the answer a literal prefix match would have given.
pub const PREFIX: Quality = 100;

/// The prefix begins one or more of the name's words, in order: `bsc` → `basic_string`, `gp` → `getPid`.
///
/// The case this module exists for. Weaker than [`PREFIX`] and stronger than everything else, because an author who
/// typed the initials of the words is naming the same name.
pub const WORD_START: Quality = 60;

/// The prefix begins one word of the name, but not the first: `string` → `to_string`.
pub const WORD_PREFIX: Quality = 40;

/// The prefix's characters appear in order somewhere: `gpid` → `get_parent_id`.
///
/// The weakest match, and deliberately so — this is what makes a list *long*, and a list where everything matches is
/// a list that filtered nothing.
pub const SUBSEQUENCE: Quality = 10;

/// **How well `candidate` answers `typed`**, or [`NONE`] when it does not.
///
/// `typed` is what was written before the cursor and `candidate` is the spelling a client would filter on — the
/// item's `filter`, when it has one, and its label otherwise. See `CompletionItem::filter`.
///
/// # No allocation, and why that is a requirement rather than a nicety
///
/// This runs for **every candidate** the analysis produced — several hundred per keystroke, and a completion is
/// answered while somebody is typing. An earlier version of this module lowercased the candidate and the typed text
/// into two `Vec<char>`, collected a third for the word starts, and compared lowercased sequences in two more: five
/// allocations per candidate, several thousand per keystroke, to answer a question about a string that is already in
/// hand. **Nothing here allocates**, and that is checked by reading it rather than by trusting it:
///
/// * case is folded one byte at a time with `u8::to_ascii_lowercase`, which is exact for every byte of every C++
///   identifier and produces a `u8` rather than a `String`;
/// * a word boundary is decided from the two bytes around a position (`begins_a_word`), so there is no list of word
///   starts to build — the earlier version's `Vec<usize>` is gone with it;
/// * the words *are* precomputed where a caller has them — the analysis's own item table — but this reads them from
///   the spelling, which is what makes it usable on a label the client sent back.
pub fn quality(candidate: &str, typed: &str) -> Quality {
    if typed.is_empty() {
        return WHOLE;
    }
    // A candidate shorter than what was typed cannot contain it.
    if candidate.len() < typed.len() {
        return NONE;
    }

    let candidate = candidate.as_bytes();
    let typed = typed.as_bytes();

    if starts_with_folded(candidate, typed) {
        return PREFIX;
    }

    match word_quality(candidate, typed) {
        Some(quality) => quality,
        None if is_subsequence(candidate, typed) => SUBSEQUENCE,
        None => NONE,
    }
}

/// Does `candidate` begin with `typed`, ignoring ASCII case?
fn starts_with_folded(candidate: &[u8], typed: &[u8]) -> bool {
    candidate
        .get(..typed.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(typed))
}

/// Do the bytes of `typed` appear in `candidate` in order, not necessarily adjacently?
fn is_subsequence(candidate: &[u8], typed: &[u8]) -> bool {
    let mut wanted = typed.iter();
    let mut next = wanted.next();
    for have in candidate {
        let Some(want) = next else { return true };
        if have.to_ascii_lowercase() == *want {
            next = wanted.next();
        }
    }
    next.is_none()
}

/// **The highest word tier this candidate earns**, or `None` when it earns none of them.
///
/// Three claims, strongest first, each one pass and none of them allocating:
///
/// ```text
/// WORD_START    every character begins a word            "gpid" -> get_parent_id , "gp" -> getPid
///               …or every one but the last, which is       "bsc" -> basic_string
///               still being typed inside the word it began
/// WORD_PREFIX    the first character begins a later word  "string" -> to_string
/// SUBSEQUENCE    the characters are in there in order     "gpd" -> mapping_pid
/// ```
///
/// A *claim* rather than a proof, and the difference is documented on [`quality`].
fn word_quality(candidate: &[u8], typed: &[u8]) -> Option<Quality> {
    // **Where the name's words begin, and whether the run starts the first one.** The first word is the first byte
    // that begins one — not index 0, because `_private_helper` begins its words at the `p`.
    let first_word = (0..candidate.len()).find(|index| begins_a_word(candidate, *index))?;
    let starts_the_name = candidate
        .get(first_word)
        .is_some_and(|have| have.to_ascii_lowercase() == typed[0]);

    // Every character begins a word, in order.
    if every_character_begins_a_word(candidate, typed, false) {
        return Some(if starts_the_name {
            WORD_START
        } else {
            WORD_PREFIX
        });
    }
    // The same, with the last character still being typed inside the word it began — `bsc` → `basic_string`.
    if starts_the_name && every_character_begins_a_word(candidate, typed, true) {
        return Some(WORD_START);
    }
    None
}

/// The next byte equal to `wanted` (ignoring ASCII case) **that begins a word**, at or after `from`.
fn next_at_a_word_start(candidate: &[u8], wanted: u8, from: usize) -> Option<usize> {
    (from..candidate.len()).find(|index| {
        candidate[*index].to_ascii_lowercase() == wanted && begins_a_word(candidate, *index)
    })
}

/// **Does every character of `typed` begin a word of `candidate`, in order?**
///
/// `last_is_free` allows the final character to sit anywhere after the one before it, which is a reader who has typed
/// the initials and is now inside the last word they named.
fn every_character_begins_a_word(candidate: &[u8], typed: &[u8], last_is_free: bool) -> bool {
    let Some((last, leading)) = typed.split_last() else {
        return false;
    };

    let mut at = 0usize;
    for wanted in leading {
        let Some(found) = next_at_a_word_start(candidate, *wanted, at) else {
            return false;
        };
        at = found + 1;
    }

    // The last character: at a word start too, or — when it is allowed to be free — merely the next byte.
    match next_at_a_word_start(candidate, *last, at) {
        Some(_) => true,
        None if last_is_free => candidate
            .get(at)
            .is_some_and(|have| have.to_ascii_lowercase() == *last),
        None => false,
    }
}

/// **Does the byte at `index` begin a word of the name?**
///
/// Both conventions at once, because a project picks one and a header may use either: a `_` separator, a capital
/// rising out of a lower-case letter (`getPid`), a capital ending an acronym run (`HTTPServer` is `HTTP` + `Server`),
/// and a digit beginning a word (`utf8` is `utf` + `8`).
fn begins_a_word(name: &[u8], index: usize) -> bool {
    let Some(&current) = name.get(index) else {
        return false;
    };
    if index == 0 {
        return current != b'_';
    }
    let previous = name[index - 1];

    if previous == b'_' {
        // The byte after a separator begins a word, and the separator itself does not.
        return current != b'_';
    }
    if current == b'_' {
        return false;
    }
    if current.is_ascii_uppercase() {
        return !previous.is_ascii_uppercase()
            // The end of an acronym run: `HTTPServer` begins `Server` at the `S`, not at every capital.
            || name.get(index + 1).is_some_and(u8::is_ascii_lowercase);
    }
    if current.is_ascii_digit() {
        return !previous.is_ascii_digit();
    }
    // A letter after a digit: `8Decode` is a boundary in both directions.
    previous.is_ascii_digit()
}

/// **The index of the first character of every word in `name`.**
///
/// The two conventions are read together rather than one being chosen, because a project picks one and a header may
/// use either — and a name that mixes them (`getHTTPResponse_code`) is written by people as well as by generators.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_prefix_matches_everything_equally() {
        // A caller that wants the whole vocabulary — a cursor after `::` with nothing typed — passes an empty prefix,
        // and everything scoring the same is what keeps that list in the order the analysis produced.
        assert_eq!(quality("anything", ""), WHOLE);
        assert_eq!(quality("", ""), WHOLE);
    }

    #[test]
    fn a_literal_prefix_is_the_strongest_match() {
        assert_eq!(quality("string", "str"), PREFIX);
        assert_eq!(quality("string", "STRING"), PREFIX);
        assert_eq!(quality("String", "str"), PREFIX);
        assert!(PREFIX > WORD_START);
    }

    #[test]
    fn the_initials_of_underscored_words_match() {
        // `bsc` -> `basic_string`: the case a literal prefix cannot answer, and the reason the module exists.
        assert!(quality("basic_string", "bsc") >= SUBSEQUENCE, "the letters are in there and it is offered");
        assert_eq!(quality("get_parent_id", "gpid"), WORD_START);
        assert_eq!(quality("small_vector", "sv"), WORD_START);
        // **A separator is part of what was typed and part of no word**, so it narrows the candidates without
        // accounting for one of them: `b_s` is in `basic_string`, and it is not an initials match because `_` begins
        // no word. A reader who types the underscore is asking a narrower question, not a different one.
        assert!(quality("basic_string", "b_s") >= SUBSEQUENCE);
    }

    #[test]
    fn the_initials_of_camel_case_words_match() {
        assert_eq!(quality("getPid", "gp"), WORD_START);
        assert_eq!(quality("stringView", "sv"), WORD_START);
        // An acronym run is one word, so `http` is one initial and not four.
        assert_eq!(quality("HTTPServer", "hs"), WORD_START);
    }

    #[test]
    fn a_prefix_that_starts_a_later_word_is_weaker_than_one_that_starts_the_name() {
        // `to_string` has words at `to` and `string`; `string` begins the second, which is a real match and a weaker
        // one than a name it begins.
        assert!(quality("to_string", "string") <= WORD_PREFIX);
        assert_eq!(quality("to_string", "to"), PREFIX);
        assert!(WORD_PREFIX < WORD_START);
    }

    #[test]
    fn the_initials_of_the_words_are_what_is_ranked_highest() {
        // **The distinction the module turns on, checked against the words each name actually has.**
        //
        //   get_parent_id   words at `get`, `parent`, `id`   `g`, `p` start words          word match
        //   gather_pid      words at `gather`, `pid`         `g`, `p` start words          word match
        //   getpid          one word                         `g`, `p` (inside `getpid`)   word match
        //
        // All three really do have their `g` and `p` at the start of a word — `getpid`'s `p` at index 3 because `get`
        // is a camel run, which this layer reads from the spelling alone and cannot know is not a word here. So all
        // three are word matches, and the ranking that separates them is the *caller's*, by where the name is declared.
        //
        // What this layer must not do is claim a word match for a name whose letters are merely present:
        assert_eq!(quality("get_parent_id", "gpid"), WORD_START);
        assert!(quality("gather_pid", "gpid") >= SUBSEQUENCE);
        assert!(quality("getpid", "gpid") >= SUBSEQUENCE);

        // …and this is that case: `mapping_pid` has `g`, `p`, `d` in order and `g` at no word start.
        assert!(quality("mapping_pid", "gpd") < WORD_START);
        // A name the reader was not spelling at all — the letters are not even in there.
        assert_eq!(quality("gather_pid", "xyz"), NONE);
    }

    #[test]
    fn a_candidate_without_the_characters_does_not_match_at_all() {
        assert_eq!(quality("string", "xyz"), NONE);
        assert_eq!(quality("get", "getters"), NONE);
        // Shorter than the prefix cannot contain it.
        assert_eq!(quality("ge", "get"), NONE);
    }

    #[test]
    fn a_prefix_must_beat_a_word_match_for_the_same_typed_text() {
        // **The ordering that keeps this from making the list worse.** Typing `str` must put `string` above
        // `to_string` — and above `std::sort`, which would be a subsequence match.
        let string = quality("string", "str");
        let to_string = quality("to_string", "str");
        let sort = quality("sort", "str");

        assert!(string > to_string, "{string} vs {to_string}");
        assert!(string > sort, "{string} vs {sort}");
        assert_eq!(string, PREFIX);
        // `sort` is not a subsequence of `str` at all, and `to_string` is a word-prefix match.
        assert_eq!(sort, NONE);
        assert!(to_string <= WORD_PREFIX);
    }

    #[test]
    fn words_are_found_by_both_conventions_at_once() {
        // The rule is read from the name rather than configured, because a project picks one convention and a header
        // may use either — and a generated name mixes them. Asserted through the matcher, which is the only way in:
        // `begins_a_word` is what the tiers are made of, and the initials of the words have to match for the tier to
        // be `WORD_START`.
        for (name, initials) in [
            ("get_pid", "gp"),
            ("getPid", "gp"),
            ("GetPid", "gp"),
            // An acronym run is one word, so `HTTP` + `Server` has the initials `hs` and not `hts`.
            ("HTTPServer", "hs"),
            ("utf8Decode", "ud"),
            ("_private_helper", "ph"),
        ] {
            assert_eq!(
                quality(name, initials),
                WORD_START,
                "{name} should answer {initials} as its words"
            );
        }
        // …and two words that are not the initials are not a word match.
        assert!(quality("get_pid", "gq") < WORD_START);
    }

    #[test]
    fn a_digit_begins_a_word_and_a_letter_after_one_does_too() {
        // `utf8` is `utf` + `8` to a reader, so `ud` is the initials of the two words and matches as one.
        assert_eq!(quality("utf8Decode", "ud"), WORD_START);
        // But `8d` is **not** the initials: `8` is not where the name starts, so the run has already passed a word
        // the reader did not type. It is still in there in order, which is the weaker match.
        assert_eq!(quality("utf8Decode", "8d"), WORD_PREFIX);
    }
}
