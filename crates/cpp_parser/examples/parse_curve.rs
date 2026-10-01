//! **Whether a parse's cost is linear in its input, and if not, where it stops being.** One file, many prefixes.
//!
//! ```text
//! cargo run --release -p cpp_parser --example parse_curve -- <file> [slices]
//! ```
//!
//! # Why this exists, and how it differs from `parse_scale`
//!
//! `parse_scale` prints a size, a time and a ratio, and a rising ratio is the reader's to notice. That was enough to
//! establish that something *is* super-linear and not enough to act on, for two reasons:
//!
//! * **It spawns a process per size.** Measuring eight prefixes of a 3 MB file costs eight `cargo run`s plus eight
//!   full parses of every *smaller* prefix inside each, so the wall time is minutes and the thing being watched is
//!   buried in process startup.
//! * **It has no baseline.** The question is not "is the ratio big" but "is the ratio bigger than the one a linear
//!   reader would show on this same file" — and only the second is answerable, because an ordinary C++ file's ratio
//!   drifts a little as its mix of declarations changes.
//!
//! So this walks a ladder of prefixes **in one process**, and for each step prints the two numbers that decide it:
//! the per-byte and per-token cost, and the **growth of each against the growth of the input**. A step that added
//! 1.15x the bytes and 1.35x the time is the step to look at, and it says so.
//!
//! # What it was built to answer
//!
//! A rendered standard-library program (`unit_program.cpp`, 3.3 MB, 1.2 M tokens) parses at **0.98 ms/KB** while a
//! 1.4 MB raw header parses at 0.28 and a flat statement file of the same size at 0.27. The cost per byte rises with
//! the file, which no linear reader does — and equal-sized windows taken anywhere in that file each cost 0.20–0.30
//! ms/KB, so it is not one expensive region. It is something that accumulates.
//!
//! # Reading the table
//!
//! ```text
//!      bytes     events   parse ms   us/byte   us/event   input    time   per-token
//! ```
//!
//! `input` and `time` are the steps' own growth against the previous row, and `per-token` is their ratio. A linear
//! reader holds `per-token` near 1.00; a term proportional to the input's *square* makes it rise without bound. The
//! `VERDICT` line names the first step where it leaves the band, because "where does it start" is the question that
//! turns a curve into a bisection.

use std::time::Instant;

/// **The ceiling** a step's per-token growth may reach and still be called flat. Wide, because a file's mix
/// changes across a slice and a rule that fires a few more times per byte is not a defect.
///
/// A **ceiling and not a band**, and the measurement that settled it: with `Vec::insert` in the token split the
/// curve rose 244 → 847 µs/KB; once the split became `O(1)` the same file gave 0.78, 1.08, 0.92, 0.95, 1.00, 1.00,
/// 1.02 — all flat — and the two-sided band still fired, on the **0.78**. A step that got *cheaper* per token is
/// not super-linear behaviour by any reading; it is the first row's cold allocations or a slice that happened to
/// hold fewer constructs. A verdict that names it sends the reader to the wrong step, which is worse than no
/// verdict at all.
const FLAT_CEILING: f64 = 1.25;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(path) = arguments.next() else {
        eprintln!("usage: parse_curve <file> [slices]");
        return;
    };
    let slices: usize = arguments
        .next()
        .and_then(|count| count.parse().ok())
        .unwrap_or(8)
        .max(2);
    let text = std::fs::read_to_string(&path).expect("the file is readable text");

    println!("{path}");
    println!(
        "{} bytes, sliced into {slices} prefixes at statement boundaries\n",
        text.len()
    );
    // **A warm-up parse that is not reported.** The first parse in a process pays for cold allocation paths that every
    // later one does not, and a ladder read from the top needs its *steps* to be comparable: measured without this,
    // the first slice of a run came out 1.9x per token above the second, and the verdict named it every time.
    // Discarded rather than reported, because it is a property of the process and not of the file.
    let _ = cpp_parser::CppParser::parse(
        &text[..4096.min(text.len())],
        cpp_parser::ParserConfig::default(),
    );

    println!(
        "{:>10}  {:>9}  {:>9}  {:>8}  {:>9}  {:>6}  {:>6}  {:>9}",
        "bytes", "events", "parse ms", "us/byte", "us/event", "input", "time", "per-token"
    );

    let mut previous: Option<(usize, usize, f64)> = None;
    let mut first_leaving: Option<(f64, f64, f64, usize)> = None;

    for step in 1..=slices {
        let target = text.len() * step / slices;
        let slice = prefix_at_a_boundary(&text, target);
        if slice.is_empty() {
            continue;
        }

        // **The timer covers the whole call**, lexing included, because that is what a caller pays, and the **best of
        // three** is what is reported: one run on a busy machine varies by tens of percent, which is the same size as
        // the effect being looked for.
        let mut best: Option<(f64, usize)> = None;
        for _ in 0..3 {
            let started = Instant::now();
            let tree = cpp_parser::CppParser::parse(slice, cpp_parser::ParserConfig::default());
            let millis = started.elapsed().as_secs_f64() * 1000.0;
            let tokens = tree.get_tokens().len();
            if best.is_none_or(|(fastest, _)| millis < fastest) {
                best = Some((millis, tokens));
            }
        }
        let (millis, tokens) = best.expect("three runs happened");

        let per_byte = micros(millis, slice.len());
        let per_event = micros(millis, tokens);

        let (input_growth, time_growth, per_token_growth) = match previous {
            Some((bytes, events, previous_millis)) => {
                let input = slice.len() as f64 / bytes.max(1) as f64;
                let events_growth = tokens as f64 / events.max(1) as f64;
                let time = millis / previous_millis.max(f64::MIN_POSITIVE);
                (input, time, time / events_growth.max(f64::MIN_POSITIVE))
            }
            None => (f64::NAN, f64::NAN, f64::NAN),
        };

        let flat = !(per_token_growth.is_finite()) || per_token_growth <= FLAT_CEILING;
        if !flat && first_leaving.is_none() {
            first_leaving = Some((per_byte, per_event, per_token_growth, slice.len()));
        }

        println!(
            "{:>10}  {:>9}  {:>9.1}  {:>8.3}  {:>9.3}  {:>6}  {:>6}  {:>8.2}x{}",
            slice.len(),
            tokens,
            millis,
            per_byte,
            per_event,
            if input_growth.is_finite() {
                format!("{input_growth:.2}")
            } else {
                "  -".to_string()
            },
            if time_growth.is_finite() {
                format!("{time_growth:.2}")
            } else {
                "  -".to_string()
            },
            per_token_growth,
            if flat { "" } else { "  <-" },
        );

        previous = Some((slice.len(), tokens, millis));
    }

    match first_leaving {
        Some((per_byte, per_event, growth, bytes)) => println!(
            "\nVERDICT super-linear from {bytes} bytes: that step cost {growth:.2}x per token, \
             at {per_byte:.3} us/byte and {per_event:.3} us/event."
        ),
        None => println!(
            "\nVERDICT linear: no step cost more than {FLAT_CEILING:.2}x per token."
        ),
    }
}

/// The file's first `target` bytes, cut back to a `;` or `}` so no slice splits a token in half.
///
/// A slice that ends mid-token would be measured with a lexer error and a recovery path the whole file never takes,
/// which is a different program from the prefix a caller means.
fn prefix_at_a_boundary(text: &str, target: usize) -> &str {
    if target >= text.len() {
        return text;
    }
    let end = text[..target]
        .rfind([';', '}'])
        .map(|at| at + 1)
        .unwrap_or(target);
    &text[..end]
}

fn micros(millis: f64, count: usize) -> f64 {
    millis * 1000.0 / count.max(1) as f64
}
