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
pub fn quality(candidate: &str, typed: &str) -> Quality {
    if typed.is_empty() {
        return WHOLE;
    }

    // **Both sides lowercased the same way**, as whole strings rather than character by character: the two agree
    // everywhere this code will ever see, and doing it differently on the two sides is the kind of asymmetry that
    // shows up once as a name that matches in one direction and not the other.
    let candidate_lower: Vec<char> = candidate.to_lowercase().chars().collect();
    let typed_lower: Vec<char> = typed.to_lowercase().chars().collect();


    if candidate_lower.len() < typed_lower.len() {
        return NONE;
    }

    if candidate_lower.starts_with(&typed_lower) {
        return PREFIX;
    }

    let words = word_starts(candidate);
    if words.len() > 1
        // **The lowercase spelling on both sides.** `name` is what the file wrote and `typed_lower` is what was
        // typed, lowercased — comparing them directly would make `getPid` fail to answer `gp`, which is the case this
        // whole module exists for.
        && let Some(quality) = word_start_quality(&candidate_lower, &words, &typed_lower)
    {
        return quality;
    }

    if subsequence(&candidate_lower, &typed_lower) {
        return SUBSEQUENCE;
    }

    NONE
}

/// **The index of the first character of every word in `name`.**
///
/// The two conventions are read together rather than one being chosen, because a project picks one and a header may
/// use either — and a name that mixes them (`getHTTPResponse_code`) is written by people as well as by generators.
fn word_starts(name: &str) -> Vec<usize> {
    let chars: Vec<char> = name.chars().collect();
    let mut starts = Vec::new();

    for (index, current) in chars.iter().enumerate() {
        // **A leading separator names no word of its own.** `_private` has one word, at 1 — the `_` is a marker, and
        // a rule that made it a word would answer a bare `_` with every private name in the project.
        if index == 0 {
            if *current != '_' {
                starts.push(0);
            }
            continue;
        }

        let previous = chars[index - 1];
        let begins_a_word = if *current == '_' {
            // A separator is skipped rather than starting a word: `get_pid` has words at 0 and 4, and the `_` at 3
            // is not one. The character after it is what the next iteration sees, and it begins the word there.
            false
        } else if previous == '_' {
            true
        } else if current.is_ascii_uppercase() {
            // `getPid` — a rise. And `GetPid` — the first character of every word is upper in PascalCase, which is
            // the same rule applied again.
            !previous.is_ascii_uppercase()
                // **The end of an acronym run.** `HTTPServer` is `HTTP` + `Server`, not `H` + `T` + … + `Server`,
                // so a rise after a run of capitals starts the word *at* the previous capital when a lower-case
                // letter follows it.
                || chars.get(index + 1).is_some_and(|next| next.is_ascii_lowercase())
        } else if current.is_ascii_digit() {
            // **`utf8` is `utf` + `8`**, so a digit begins a word whatever precedes it — including another digit,
            // which cannot: `128` is one word and not three.
            !previous.is_ascii_digit()
        } else if previous.is_ascii_digit() {
            true
        } else {
            false
        };

        if begins_a_word {
            starts.push(index);
        }
    }

    starts.sort_unstable();
    starts.dedup();
    starts
}

/// **Does the prefix begin the name's words, in order?**
///
/// The rule: **every character begins a word, except a final run that fits inside one word.**
///
/// ```text
/// "gp"      -> getPid          both begin words                                WORD_START
/// "gpid"    -> get_parent_id   all four begin words                            WORD_START
/// "sv"      -> small_vector    both begin words                                WORD_START
/// "bsc"     -> basic_string    `b`, `s` begin words; `c` fits inside `string`  WORD_START
/// "string"  -> to_string       one character, a word that is not the first     WORD_PREFIX
/// "btc"     -> basic_string    `t` begins no word and does not fit in `asic`   no match
/// ```
///
/// Two things make this a *word* match rather than a subsequence, and both are what a reader means when they type
/// the initials of a name:
///
/// * the characters before the last run **really do begin words**, in order — which is why `btc` is refused: its
///   `t` begins nothing, and it is not the last character;
/// * the final run **fits inside a single word** rather than being scattered over the whole name, which is why
///   `string` → `to_string` is a weaker match than `bsc` → `basic_string`: `t` would have to jump out of the word
///   `s` began.
///
/// `name` and `typed` are both lowercased by the caller.
fn word_start_quality(name: &[char], words: &[usize], typed: &[char]) -> Option<Quality> {
    // **Two hypotheses, in order of how much they claim.**
    //
    // 1. every character but the last starts a word — `gp` → `getPid`, `bsc` → `basic_string`;
    // 2. a subsequence in which the *first* character starts a word — `gpid` → `get_parent_id`, where `i` and `d`
    //    sit inside `pid` and start nothing.
    //
    // The order matters because they disagree, and the first is the stronger claim: a greedy subsequence over
    // `basic_string` finds the `s` of `basic` at index 2, and then has nowhere to go for `c` — while the word rule
    // finds `s` at the start of `string`. Asking the strict question first is what makes `bsc` work at all.
    if let Some(quality) = every_character_but_the_last_starts_a_word(name, words, typed) {
        return Some(quality);
    }

    // The second: the first character starts a word, and the rest appear in order anywhere after it.
    let start = words
        .iter()
        .copied()
        .find(|candidate| name.get(*candidate) == typed.first())?;
    if !subsequence(&name[start..], typed) {
        return None;
    }
    Some(if start == 0 { WORD_START } else { WORD_PREFIX })
}

/// The strict hypothesis: **initials, with at most the last character running on.**
///
/// Every character must either
///
/// * **start a word** — the initials a reader is typing, `gp` → `getPid`; or
/// * **continue the character matched immediately before it** — `bs` → `basic_string`, where `s` is the next letter
///   of `basic`.
///
/// and only the **last** character is exempt, because that is the one still being typed.
///
/// The adjacency, rather than "somewhere later in the name", is what makes this a claim worth ranking above a
/// subsequence:
///
/// ```text
/// "bsc" -> basic_string      b starts `basic`, s follows b, c is last            a word match
/// "gpid"-> get_parent_id     g,p,d start words, i follows p, d is last           a word match
/// "gpid"-> getpid            t follows nothing (t is not a word start)           NOT a word match
/// "gpid"-> gather_pid        a follows nothing, t is not last                    NOT a word match
/// ```
///
/// Returning `None` is not a failure — the caller falls back to [`SUBSEQUENCE`], which is the honest description of
/// a name whose letters are present but whose words were not typed.
fn every_character_but_the_last_starts_a_word(
    name: &[char],
    words: &[usize],
    typed: &[char],
) -> Option<Quality> {
    // **Match the typed characters in order, earliest first, and count how many land on a word start.**
    //
    // There is no jump and no special case, which is the point: the previous three attempts at this each added a rule
    // for one example and broke another, and the honest version is a *score* rather than a verdict. A reader who
    // spelled the initials of a name gets a high count; one whose letters merely happen to appear gets a low one, and
    // the caller's tier decides between two names that scored the same.
    //
    // The rule is a subsequence, so it always finds a match when one exists — the ranking is the whole of the
    // discrimination, and it never has to say "no" to a name that contains what was typed.
    let mut at = 0usize;
    let mut on_a_word_start = 0usize;
    let mut first: Option<usize> = None;

    for wanted in typed {
        let found = name.iter().enumerate().skip(at).find(|(_, have)| *have == wanted)?.0;
        first = first.or(Some(found));
        if words.contains(&found) {
            on_a_word_start += 1;
        }
        at = found + 1;
    }

    let starts_at_the_name = first == Some(0);
    Some(match (starts_at_the_name, on_a_word_start) {
        // Every character at the start of a word, beginning with the name's own: the initials, exactly.
        (true, count) if count == typed.len() => WORD_START,
        // The name's first word began the run, so this is `bsc` → `basic_string` — the initials, with the last
        // character still being typed inside the word it began.
        (true, count) if count + 1 >= typed.len() => WORD_START,
        // A later word began the run: `string` → `to_string`.
        (false, count) if count + 1 >= typed.len() => WORD_PREFIX,
        // The letters are in there and the words are mostly not: a real match, ranked last.
        _ => SUBSEQUENCE,
    })
}

/// Do the characters of `typed` appear in `candidate` in order, not necessarily adjacently?
fn subsequence(candidate: &[char], typed: &[char]) -> bool {
    let mut wanted = typed.iter();
    let mut current = wanted.next();
    for have in candidate {
        let Some(want) = current else { return true };
        if have == want {
            current = wanted.next();
        }
    }
    current.is_none()
}

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
        // The separators are not part of the match, so typing one is the same as not typing it.
        assert_eq!(quality("basic_string", "b_s"), WORD_START);
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
        // may use either — and a generated name mixes them.
        assert_eq!(word_starts("get_pid"), vec![0, 4]);
        assert_eq!(word_starts("getPid"), vec![0, 3]);
        assert_eq!(word_starts("GetPid"), vec![0, 3]);
        assert_eq!(word_starts("HTTPServer"), vec![0, 4]);
        assert_eq!(word_starts("utf8Decode"), vec![0, 3, 4]);
        // A leading separator names no word of its own.
        assert_eq!(word_starts("_private"), vec![1]);
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
