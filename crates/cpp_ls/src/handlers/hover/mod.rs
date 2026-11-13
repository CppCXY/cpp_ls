//! # `textDocument/hover` — what the name under the cursor is
//!
//! Three questions, in the order the preprocessor asks them:
//!
//! ```text
//! 1. is this a macro?        a name that a #define settles IS that macro at this point in the file
//! 2. otherwise, what does it name?  the index's declaration, rendered from what the fact records
//! 3. and if it names nothing, what is it?  the type of the expression the cursor is in — see below
//! ```
//!
//! Macros first because that is what the text means: `#define WIDGET_MAX 8` makes `WIDGET_MAX` eight, and a hover
//! that instead described some declaration of the same spelling would describe a name the compiler never saw. The
//! analysis answers that question the same way — `macro_across_files` walks the `#define`/`#undef` history before
//! the cursor's offset, and `#undef` counts, so a name that *was* a macro answers "not a macro here" rather than
//! pointing at the definition it no longer has.
//!
//! # What is shown, and what is deliberately not
//!
//! The declaration **as the file writes it** (the fact carries the declaration's range, and the text is read
//! through the session — the buffer for an open file), the qualified name, what kind of thing it is, its written
//! type or return type, the documentation comment above it, and the caveats the fact itself records: a declaration
//! inside a conditional block, or one the parser recovered around. That is the point of a fact-only hover:
//! everything shown is something the analysis *knows*, and nothing is a guess dressed as a signature.
//!
//! The third question is the one a cursor on `this`, `*p` or `f(1)` needs, and it is asked **only** when the
//! second one names nothing: a name is what a reader is asking about, and the type of the expression around it is
//! a fallback rather than a second answer competing with the first.
//!
//! No `range` is set on the answer: an LSP hover may carry one, and computing it would mean deciding where the
//! name under the cursor starts and ends — a second implementation of "what is a name" beside the one the analysis
//! already has (`sema::resolve::name_at`), which is free to disagree with it. The client positions the popup
//! itself when the answer carries none, which is what happens today.

use cpp_code_analysis::{
    DeclFact, DeclKind, DiskFiles, FactGuard, FileView, Known, ProjectDefinition, ProjectMacro,
    Session, UnknownReason,
};
use cpp_parser::CppDocComment;
use lsp_types::{
    ClientCapabilities, Hover, HoverContents, HoverParams, HoverProviderCapability, MarkedString,
    MarkupContent, MarkupKind, ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{file_offset_at_position, uri_to_file_path};

/// The two vocabularies a popup has an answer for and the analysis does not: keywords, and Doxygen's commands.
mod vocabulary;

/// How long an expression quoted in a hover may be before it is cut.
///
/// An expression is quoted *inside a line of markdown*, so it is collapsed to one line first; the cut is what
/// keeps a long call from filling the popup with code the reader is already looking at.
const MAX_INLINE_CHARS: usize = 80;

pub async fn on_hover(
    context: ServerContextSnapshot,
    params: HoverParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Hover> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    // **The read-only form of the step below**: hover is answered from the file's own text, so a wait for the write
    // lock — which during an index is the pump's — buys nothing. `AnalysisState::prepare_for_a_read` carries the
    // measurement: the first hover of an open file went from 1 103 ms to 12–34 ms.
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare_for_a_read(&path).await;
        crate::handlers::read_the_modules(&context, &path, false).await;

        // **And then the query, which no longer waits for the project's index to drain.**
        //
        // The defect this used to paper over was real and is worth keeping on the record: `std::endl` lives in
        // `__msvc_ostream.hpp`, inside a namespace opened by `_STD_BEGIN`, and it is filed under `std` only by that
        // header's **cooked** reading — so hovering it in a file just opened answered nothing at all, at once, at
        // 250 ms, at 1 s and at 3 s, and the report was "`endl` has no hover" rather than "it is slow". The wait was
        // `AnalysisState::settle`, which asked whether the *whole project's* indexing queue was empty.
        //
        // What the answer actually needs is this file, and it is read in above: `prepare` puts the
        // file's text in reach, `read_the_modules` catches the summary up and reads what it
        // imports. The global question was never a prerequisite for either.
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        struct Step(&'static str, std::time::Instant);
        impl Drop for Step {
            fn drop(&mut self) {
                let ms = self.1.elapsed().as_millis();
                if ms >= 15 {
                    log::warn!("hover: stage {} took {ms} ms", self.0);
                }
            }
        }
        let _whole = Step("query", std::time::Instant::now());
        let path = uri_to_file_path(&uri)?;

        // **The two questions about the text are asked of the file's own text.**
        //
        // A view is of the **rendering** — the file with its macros replaced and its directives resolved, which is
        // where declarations, types and members are resolved, and the right reading for those. Two of the questions
        // below are not about the program at all but about the line the reader wrote: `#include <vector>` declares
        // nothing, and a macro's name is *gone* from the rendering because it was replaced by what it stands for.
        // Asked of the rendering, both answer "nothing to say" — the reader sees an empty popup on the two things
        // they most obviously want a popup for.
        //
        // So the answers to those two are asked of the **file's own text** — which is what `Session::header_at` and
        // `Session::macro_definition` do by parsing the written text themselves — and the rendering answers the rest.
        // Neither is a fallback: each is the only reading that has the thing being asked about.
        //
        // **The block this replaces was dead**, and it is worth naming why: it began
        // `let written = session.view_of_the_file(&path)?;`, and `view_of_the_file` is an **ask** — it queues the
        // work and answers `None` ("a caller that needs meaning waits for the rendering"). So the `?` returned from
        // the whole handler and *every* hover answered `null`; the two queries below it never ran. What is left is one
        // reading and two of its offsets: the **file's** for the two text-shaped questions, the **rendering's** for
        // everything else.
        //
        // **The cursor is read from the file's own text, then mapped into the rendering.** The two
        // steps are done in the order that cannot drift.
        let view_step = Step("view_of_the_file", std::time::Instant::now());
        let written = session.view_of_the_file(&path)?;
        drop(view_step);
        let in_the_file = file_offset_at_position(&written, position)?;
        // **The hover answers from the file's own text, not from its rendering** — and that is a fix, not a
        // simplification. `session.view` asks for a cook when the rendering is not cached, and the cache holds eight,
        // so on a project whose closure is a hundred files hovering the file the reader is in **evicted it** and
        // answered `None` for that request while queuing the cook that would answer the next one. Reported from a real
        // session as "分离定义的 cpp 文件里面没有 hover 结果", together with a log of
        // `cook slice: asked 1, rendered 1, emptied the queue` on every hover.
        //
        // Everything below — `definitions`, `type_at`, `members_of`, the doc comment — is a name or type question, and
        // the scopes that answer those are the written text's.
        let view = written.clone();
        let offset = cpp_code_analysis::file::coordinates::ReadingOffset::new(in_the_file.get());
        if let Known::Yes(header) = session.header_at(&view, in_the_file) {
            return Some(markdown(header_markdown(&header)));
        }
        hover(session, &view, offset, in_the_file)
    })
    .await
}

/// **The Doxygen command at `offset`**, when the cursor is on one inside a documentation comment.
///
/// Read from the file's text and line by line, for the reason the completion's version is: a comment is trivia, so
/// no rendering contains it, and the cursor lands at a doc line's end after a keystroke.
///
/// Only a *documentation* comment answers: `// note to self: @param` is prose with an `@` in it.
fn doc_command_at(text: &str, offset: usize) -> Option<String> {
    let offset = offset.min(text.len());
    let before = text.get(..offset)?;
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);

    // **The whole line, not the text before the caret**: a cursor inside `@param` has only `@pa` in front of it,
    // and the word it is in is the whole one. The caret is a position in the line, tested against each word's span.
    let line_end = text[offset..]
        .find('\n')
        .map_or(text.len(), |at| offset + at);
    let line = text.get(line_start..line_end)?;
    let caret = offset - line_start;

    let trimmed = line.trim_start();
    let is_a_documentation_line = trimmed.starts_with("///")
        || trimmed.starts_with("//!")
        || trimmed.starts_with("/**")
        || trimmed.starts_with("/*!");
    let inside_a_documentation_block = if is_a_documentation_line {
        true
    } else {
        match (before.rfind("/*"), before.rfind("*/")) {
            (Some(opened), closed) => {
                let still_open = closed.is_none_or(|closed| closed < opened);
                still_open
                    && (before[opened..].starts_with("/**") || before[opened..].starts_with("/*!"))
            }
            _ => false,
        }
    };
    if !inside_a_documentation_block {
        return None;
    }

    // **The word under the cursor**, or the one it is just past: a cursor anywhere inside `@param` — including at
    // its end, where a keystroke leaves it — is a question about `param`.
    let bytes = line.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        if !matches!(bytes[at], b'@' | b'\\') {
            at += 1;
            continue;
        }
        let mut end = at + 1;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        let word_start = at + 1;
        // The caret must be **on** the word: inside it, or just past its last letter.
        if end > word_start && word_start <= caret && caret <= end {
            return Some(line[word_start..end].to_string());
        }
        at = end.max(at + 1);
    }

    None
}

/// **A symbol reference in a doc comment, when the cursor is on it** — Rust's ``[`Widget::grow`]``, or Doxygen's
/// `@see Widget::grow`.
///
/// Read from the file's own text, like [`doc_command_at`], and for the same reason: a comment is trivia, so the
/// rendering has none of it and every tree-based question would answer about the token *after* the comment.
///
/// The rule itself lives one layer down, in [`cpp_code_analysis::doc`], because a symbol reference is one question
/// with four consumers — the colour, this card, the jump and the completion — and a second copy of it here would be a
/// second answer to disagree with the first.
pub(crate) fn doc_target_at(
    text: &str,
    offset: usize,
) -> Option<cpp_code_analysis::doc::DocTarget> {
    cpp_code_analysis::doc::target_at(text, offset)
}

/// What the name at `offset` is — `None` when it does not name anything the analysis can answer about.
///
/// `None` is the answer for punctuation, for whitespace, and for a name the analysis cannot place: a client shows
/// nothing, and "there is nothing to say here" is true, where an empty popup would not be.
/// The **identifier the cursor is on**, read from the file's own text — the word a macro question is about.
///
/// The same walk `Session::macro_at_a_file_offset` makes; it is repeated here because the answer is needed *before*
/// that query, to ask the toolchain about a name that no file defines.
fn word_in_the_file(
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::FileOffset,
) -> String {
    // **The file's own text, from whichever view this is** — see `FileView::the_written_text`: the handler holds the
    // plain view, whose root is the file's own text, and `written_text()` alone is `None` for it.
    let Some(written) = view.the_written_text() else {
        return String::new();
    };
    let bytes = written.as_bytes();
    let is_a_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let at = offset.get().min(bytes.len());
    let mut start = at;
    while start > 0 && is_a_word(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end < bytes.len() && is_a_word(bytes[end]) {
        end += 1;
    }
    written.get(start..end).unwrap_or_default().to_string()
}
/// **The card's markdown**, for a caller that is not speaking the protocol — a tool, a test, a probe.
///
/// [`hover`] answers with `lsp_types::Hover`, which is the right type for the server and the wrong one for a caller that
/// wants the text: it would have to depend on `lsp_types` to read one field. See `tools/inspect`.
pub fn hover_text(
    session: &Session<DiskFiles>,
    view: &FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    in_the_file: cpp_code_analysis::file::coordinates::FileOffset,
) -> Option<String> {
    let hover = hover(session, view, offset, in_the_file)?;
    Some(match hover.contents {
        HoverContents::Markup(markup) => markup.value,
        HoverContents::Scalar(one) => match one {
            MarkedString::String(text) => text,
            MarkedString::LanguageString(string) => string.value,
        },
        HoverContents::Array(many) => many
            .into_iter()
            .map(|one| match one {
                MarkedString::String(text) => text,
                MarkedString::LanguageString(string) => string.value,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

/// **One call, timed, and said out loud when it is slow.** The stage names are the only way to tell "the query" from
/// what the query asked: a hover's own second has been attributed to the pump, the cook and the locks in earlier
/// rounds, and each time it was one of these calls — so the next attribution comes from a name, not a theory.
macro_rules! timed {
    ($name:expr, $call:expr) => {{
        let started = std::time::Instant::now();
        let value = $call;
        let ms = started.elapsed().as_millis();
        if ms >= 15 {
            log::warn!("hover: stage {} took {ms} ms", $name);
        }
        value
    }};
}

pub fn hover(
    session: &Session<DiskFiles>,
    view: &FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    in_the_file: cpp_code_analysis::file::coordinates::FileOffset,
) -> Option<Hover> {
    if let Some(found) = timed!(
        "program",
        hover_of_the_program(session, view, offset, in_the_file)
    ) {
        return Some(found);
    }

    // **The rendering, when the file's own text has nothing to say.** A declaration a *macro* wrote is not in the
    // file's text at all: `kvec_t(SignItem) signs = …` is `struct { … } signs = …` to a compiler, and the plain
    // reading files `signs` with no type — so hovering the declarator, or a member of it, answered nothing
    // (neovim's `src/nvim/decoration.c:1040`). The rendering is asked at the position mapped through its spans,
    // because the expansion makes the two layouts differ on that very line. `view()` answers `None` unless a cook
    // already read the rendering, so this costs nothing on a project that has not.
    if let Some(found) = hover_of_the_rendering(session, view, in_the_file) {
        return Some(found);
    }

    // **The two vocabularies are the last questions, not the first.**
    //
    // `this` is a keyword *and* a value with a type in this context, and `nullptr` is a literal with one; the analysis
    // has the better answer for both, and asking the vocabulary first shadowed it. Measured: hovering `this` inside
    // a member function showed the keyword's meaning instead of "a pointer to the object", which is the sentence a
    // reader on `this->size` is looking for.
    //
    // A keyword the analysis has nothing to say about — `constexpr`, `noexcept`, `friend` — reaches here, which is
    // the case that had no popup at all before.
    //
    // **Only when the cursor is on the word in the file.** The rendering drops blank lines and comments, so a cursor
    // on a blank line maps into the next token in it: measured, hovering the empty line above `void f()` was
    // answered with what `void` means. The token's own span, mapped back to the file, is what says whether the reader
    // is looking at it.
    if let Some(token) = cpp_parser::token_at(&view.root, offset.get()) {
        let in_the_rendering = cpp_parser::source_range(token.text_range());
        let start = view
            .file_offset_of(cpp_code_analysis::file::coordinates::ReadingOffset::new(
                in_the_rendering.start_offset,
            ))
            .map(|at| at.get());
        let end = view
            .file_offset_of_end(cpp_code_analysis::file::coordinates::ReadingOffset::new(
                in_the_rendering.end_offset(),
            ))
            .map(|at| at.get());
        let covers_the_cursor = match (start, end) {
            (Some(start), Some(end)) => start <= in_the_file.get() && in_the_file.get() <= end,
            _ => false,
        };

        if covers_the_cursor && let Some(text) = vocabulary::keyword_hover(&token) {
            return Some(markdown(text));
        }

        // **A C keyword *construct* — `_Static_assert`, `_Generic`, `typeof` — which the lexer does not call a
        // keyword.** Each is a contextual spelling (`_Static_assert` is an identifier, `typeof` is only a keyword where
        // the language allows it), so the table above cannot answer, and a reader hovering `_Static_assert(...)` got a
        // popup about a *function call* or nothing at all. The vocabulary is keyed by the spelling.
        if covers_the_cursor && let Some(text) = vocabulary::construct_hover(&token) {
            return Some(markdown(text));
        }

        // **…and CUDA's own vocabulary, which is not keywords at all.** `__global__` is an identifier to the lexer and
        // a specifier to a CUDA compiler, so the question is the project's *family* rather than the token's kind —
        // see `cpp_code_analysis::cuda`. It is asked after the keyword table and not before: a word that is both (none
        // is today, but `warpSize` is a name a project may also declare) belongs to the declaration if there is one,
        // and `hover_of_the_program` has already had its say by the time this runs.
        if covers_the_cursor
            && let Some(text) = vocabulary::cuda_hover(&token, session.config().cuda())
        {
            return Some(markdown(text));
        }
    }

    // **A symbol reference in a documentation comment** — Rust's ``[`Widget::grow`]``. Asked here, after the
    // program's own answer (a comment declares nothing, so the two cannot collide) and before the command table:
    // a link is the one place a reader expects a card about *another* declaration, and the target is resolved with
    // the same query a jump uses, so the card and the jump cannot disagree.
    // **A symbol reference in a documentation comment** — Rust's ``[`Widget::grow`]``, or Doxygen's
    // `@see Widget::grow`. Asked here, after the program's own answer (a comment declares nothing, so the two cannot
    // collide) and before the command table: a reference is the one place a reader expects a card about *another*
    // declaration, and the target is resolved with the same query a jump uses, so the card and the jump cannot
    // disagree.
    if let Some(target) = doc_target_at(
        view.written_text().unwrap_or(&view.source),
        in_the_file.get(),
    ) {
        // **The punctuation a reference may add.** ``[`f()`]`` and ``[`MAX!`]`` say what a reader means — that it is a
        // call, that it is a macro — and neither is part of the name.
        let spelling = target.spelled_for_a_lookup();
        if let Known::Yes(found) = session.index().definition(spelling, &view.path) {
            return Some(markdown(declaration_markdown(session, view, &found, None)));
        }
        // **A macro has no declaration to name**, so it is the second question rather than a spelling of the first —
        // the same one the parser asks for a `#define` this file writes.
        if let Known::Yes(found) = session.macro_at_a_file_offset(view, in_the_file) {
            return Some(markdown(macro_markdown(
                session,
                view,
                &found,
                Some(in_the_file),
            )));
        }
    }

    // **A Doxygen command**, asked of the file's own text: a comment is trivia, so the rendering has none of it —
    // the same reason the completion and the doc tokens read the written text. `@param` is a word the reader cannot
    // look up anywhere else in the editor.
    if let Some(name) = doc_command_at(
        view.written_text().unwrap_or(&view.source),
        in_the_file.get(),
    ) && let Some(text) = vocabulary::command_hover(&name)
    {
        return Some(markdown(text));
    }

    None
}

/// **The program's own answer** — declarations, types, expressions, headers, macros — and `None` when it has none.
/// **The same question, asked of the rendering** — see the note in [`hover`].
///
/// A macro that expands to a *type* is the shape this exists for: the file's text has `kvec_t(SignItem) signs`, whose
/// plain reading gives `signs` no type at all, and the rendering has `struct { … } signs`, whose members are real.
/// The position is mapped through the rendering's spans rather than reused, because the expansion is longer than the
/// invocation and every column after it moves.
fn hover_of_the_rendering(
    session: &Session<DiskFiles>,
    view: &FileView,
    in_the_file: cpp_code_analysis::file::coordinates::FileOffset,
) -> Option<Hover> {
    let traced = std::env::var("CPPLS_TRACE_HOVER").is_ok();
    let mapped = session.the_rendering_offset(&view.path, in_the_file.get());
    if traced {
        eprintln!(
            "[rendering] {} at {} maps to {mapped:?}",
            view.path.display(),
            in_the_file.get()
        );
    }
    let offset = mapped?;
    let rendered = session.view(&view.path)?;
    if traced {
        eprintln!(
            "[rendering] the rendering view is {} byte(s)",
            rendered.source.len()
        );
    }
    let offset = cpp_code_analysis::file::coordinates::ReadingOffset::new(offset);
    hover_of_the_program(session, &rendered, offset, in_the_file)
}

fn hover_of_the_program(
    session: &Session<DiskFiles>,
    view: &FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    in_the_file: cpp_code_analysis::file::coordinates::FileOffset,
) -> Option<Hover> {
    // A project can turn the popup off (`hover.enable = false` in `.cppls.toml`), which is a preference about the
    // editor rather than about the analysis — so it is checked here, at the edge, and the queries below do not know
    // it exists.
    if session.project_config().config.hover.enable == Some(false) {
        return None;
    }

    // **A header name before a declaration**, for the reason the jump asks it first: `#include <vector>` declares
    // nothing, so every question below answers "nothing to say" for a name whose answer is a file on disk. The
    // popup is the resolved path, which is the one thing a reader cannot see from the line they wrote — the header
    // is found by a search, and *which* of the four thousand files on this machine's search path it found is the
    // answer to the question the line asks.
    if let Known::Yes(header) = timed!("header_at", session.header_at(view, in_the_file)) {
        return Some(markdown(header_markdown(&header)));
    }

    // **And the format string**, which is the other thing a reader points at and the index cannot answer: `%zu` is a
    // `size_t` in decimal and `{:#x}` is hex with a prefix, and both are inside a literal that no declaration
    // describes. Asked here — after the header, before every name question — because a string literal is not a name
    // and the queries below would spend their time finding that out.
    if let Some(found) = timed!(
        "format_string",
        crate::handlers::format::format_string_hover(session, view, in_the_file)
    ) {
        return Some(found);
    }

    // **A member access is a question about the object's type, not about the name.** `z.append(…)` used to fall through
    // to the name query, which answers "every declaration called `append` in the project" — nine `basic_string::append`
    // overloads for a `std::vector`. The honest answers are two: the member the object's type actually has, or a popup
    // that says the type is not known here. Listing unrelated names is neither.
    if let Some(access) =
        cpp_code_analysis::sema::resolve::member_access_at(&view.root, offset.get())
    {
        return Some(markdown(member_markdown(session, view, &access)));
    }

    // **The macro question, asked in the file's coordinates.** A macro use has no footprint in the rendering — the cook
    // expanded it — so the query takes the file offset the client gave us. Measured: a hover on a file's own
    // `#define MAX(a, b)` answered nothing, because the reading offset named a position the file does not have.
    if let Known::Yes(found) = timed!(
        "macro_at_a_file_offset",
        session.macro_at_a_file_offset(view, in_the_file)
    ) {
        return Some(markdown(macro_markdown(
            session,
            view,
            &found,
            Some(in_the_file),
        )));
    }
    // …and the rendering's coordinates for a macro written where the cook kept it (a `#define`'s own body, a directive
    // argument), which is what the old path answered.
    // **A macro whose `#define` is inside a condition this reading cannot decide** is still a macro to the reader —
    // `<gtest/gtest.h>` writes `#define TEST(a, b) …` behind `#if !GTEST_DONT_DEFINE_TEST`, and the file that uses
    // `TEST(Format, LocalStatement)` plainly has it. The popup says what the name is and that its existence depends on
    // the compilation, rather than saying nothing at all (which is what an editor reported: no hover on `TEST`).
    match timed!("macro_definition", session.macro_definition(view, offset)) {
        Known::Yes(found) => {
            return Some(markdown(macro_markdown(
                session,
                view,
                &found,
                Some(in_the_file),
            )));
        }
        Known::Unknown(UnknownReason::ConditionalCompilation) => {
            let name = session
                .definitions(view, offset)
                .value()
                .and_then(|found| found.found.first().map(|one| one.fact.name.clone()));
            let _ = name;
            let written = cpp_parser::token_at(&view.root, offset.get())
                .map(|token| token.text().to_string())
                .unwrap_or_default();
            if !written.is_empty() {
                return Some(markdown(format!(
                    "`{written}` — a macro whose `#define` is inside a conditional block, so whether it exists here \
                     depends on the compilation. The analysis is reporting what the header says, not what a compiler \
                     concluded."
                )));
            }
        }
        Known::Unknown(_) | Known::No => {}
    }
    // **A macro the compiler itself defines** — `__STDC_VERSION__`, `__GNUC__`, `_WIN32`. It is in no file, so every
    // query above answers nothing for it, and a reader hovering `#ifdef __STDC_VERSION__` was told nothing at all —
    // which reads as "this name is unknown" for the one kind of name a compiler defines by itself. The value is the
    // standard's own where the standard defines one, and the toolchain's `-dM` table otherwise.
    let word = word_in_the_file(view, in_the_file);

    // **A directive this very file writes**, read from its text. The index's summaries are pushed by the work loop, so
    // a file opened a moment ago has a view and no summary — and a hover on its own `#define MAX(a, b)` answered nothing
    // at all. The card is the directive itself, which is what a reader pointing at a definition wants to see.
    if !word.is_empty()
        && let Some(directive) = session.macro_definition_text(&view.path, in_the_file.get(), &word)
    {
        return Some(markdown(format!("```c\n{directive}\n```")));
    }
    if !word.is_empty()
        && let Some(predefined) = session.predefined_macro(&word, &view.path)
    {
        return Some(markdown(if predefined.is_empty() {
            format!("`{word}` — predefined by the compiler this project builds with.")
        } else {
            format!(
                "`{word}` — predefined by the compiler this project builds with, as `{predefined}`."
            )
        }));
    }
    // **A macro the standard defines and no table holds.** `__LINE__`, `__FILE__`, `__DATE__`, `__TIME__` and
    // `__COUNTER__` have no value to show — the value is the line, the file, the date and the time the compiler was on —
    // and `-dM` does not print them (measured: `g++ -dM -E` answers `__cplusplus` and none of these). So the card says
    // what each one *is*, which is the only true thing it can say, and it says it in the same words the valued ones use.
    // Before this branch, `__cplusplus` had a card and `__LINE__` had none.
    if let Some(what) = a_standard_macro_meaning(&word) {
        return Some(markdown(format!(
            "`{word}` — predefined by the compiler: {what}."
        )));
    }

    // **The spelling the cursor sits on**, when it is a template-id use. Read once here rather than inside the card:
    // the card is also asked about declarations the cursor does *not* name (an overload list, a member resolved
    // through `->`), and those must not borrow this file's spelling.
    let written = written_template_id(view, offset);

    match session.definition(view, offset) {
        Known::Yes(found) => Some(markdown(declaration_markdown(
            session,
            view,
            &found,
            written.as_deref(),
        ))),
        // **A name with several declarations is a popup with several signatures**, not none. `std::format` is four
        // overloads, so the name question has always answered `Ambiguous` — and `Ambiguous` used to fall through to
        // the *expression* question, which has nothing to say about a name that is not in an expression: hovering
        // `std::format` showed an empty popup on the most ordinary call in modern C++. The list is what the reader
        // is asking for ("what can I call here"), and the index can now show each one's parameters.
        Known::Unknown(UnknownReason::Ambiguous(_)) => match session.definitions(view, offset) {
            Known::Yes(found) => {
                // **A member of one class is not a member of another.** The index answers every declaration of the
                // *spelling*, so hovering `_supportNonStandardSymbol` in `CodeFormat.cpp` listed `CodeFormat`'s and
                // `LuaLexer`'s and told the reader "which one a call means is decided by the arguments" — about a
                // **data member**, which has no arguments and no second reading. Find References and Go to Definition
                // narrow by the cursor's class; this is the same rule, at the same offset.
                let found = crate::handlers::references::narrowed_to_the_cursors_class(
                    view,
                    offset.get(),
                    found,
                );
                // **A class name is one declaration, not an overload set.** `LuaParser p(…)` hovers three facts — the
                // class and its two constructors, which are filed under the same name because a constructor's name is
                // its class's — and the answer listed the constructors as if a call were meant ("3 declarations …
                // decided by the arguments"), about a name being *declared* in that very line. When one of the facts is
                // a type, that is the one the reader named.
                // **Which of several types the reader named.** `std::vector<std::string>` has three type facts in
                // the index: a **forward declaration** in `<format>` (`template <typename, typename> class vector;`),
                // the **definition** in `stl_vector.h`, and the `vector<bool>` **specialization** in
                // `stl_bvector.h`. `find(kind == Type)` answered the first, so the card described a class with no
                // body, declared in a header the reader never included — reported with a picture ("the definition is
                // incomplete, it points at the forward declaration in <format>").
                //
                // A **definition** beats a forward declaration, and among definitions the one with the **most
                // template parameters** is the general one — a partial specialization is a narrower shape
                // (`vector<bool>`'s pattern has one parameter, the primary's two), so the general one is what a
                // spelling that did not write the specialization means.
                if std::env::var("CPPLS_TRACE_HOVER").is_ok() {
                    eprintln!(
                        "[types] {:?}",
                        found
                            .found
                            .iter()
                            .map(|definition| format!(
                                "{} definition {} pattern {:?}",
                                definition.file.display(),
                                definition.fact.definition,
                                definition.fact.pattern
                            ))
                            .collect::<Vec<_>>()
                    );
                }
                if let Some(the_type) = found
                    .found
                    .iter()
                    .filter(|definition| {
                        matches!(definition.fact.kind, cpp_code_analysis::DeclKind::Type)
                    })
                    .max_by_key(|definition| {
                        // **More template parameters is the more general declaration** — `fact.parameters` is the
                        // template head's own names, and it is what says which fact a partial specialization is:
                        // the primary's are `[_Tp, _Alloc]`, `vector<bool>`'s are `[_Alloc]`, and a forward
                        // declaration's are the empty names of `template <typename, typename>`. (`pattern` is `None`
                        // for all three here, so it cannot separate them — measured, not assumed.)
                        (definition.fact.definition, definition.fact.parameters.len())
                    })
                {
                    return Some(markdown(declaration_markdown(
                        session,
                        view,
                        the_type,
                        written.as_deref(),
                    )));
                }
                match found.found.len() {
                    1 => Some(markdown(declaration_markdown(
                        session,
                        view,
                        &found.found[0],
                        written.as_deref(),
                    ))),
                    2.. => Some(markdown(overloads_markdown(
                        session,
                        Some(view),
                        offset,
                        &found,
                    ))),
                    _ => None,
                }
            }
            _ => expression_markdown(session, view, offset).map(markdown),
        },
        // **A name inside a member function may be a member of the class** — C++ looks there before it gives up, and
        // that is the one place a bare name can name a declaration no scope in this file writes:
        // `shared_from_this()` in a class deriving `std::enable_shared_from_this<Node>`. The analysis answers with the
        // member list, substituted; without this the hover had **no content at all** for the call (reported, with a
        // screenshot of an empty popup).
        Known::No | Known::Unknown(_) => {
            match session.member_of_the_enclosing_class(view, offset) {
                Some(found) => Some(markdown(declaration_markdown(session, view, &found, None))),
                // The cursor may still be *in* something the analysis can type — `this`, `*p`, `f(1)` — which is a
                // different question about the same position, and the one a reader on a keyword is asking.
                None => expression_markdown(session, view, offset).map(markdown),
            }
        }
    }
}

/// **The member a `.` or `->` names, resolved through the object's type** — or an explanation when the type is unknown.
fn member_markdown(
    session: &Session<DiskFiles>,
    view: &FileView,
    access: &cpp_code_analysis::sema::resolve::MemberAccess,
) -> String {
    let object = access.object.text().to_string();
    let object = object.trim();
    // **The object's own node, not the byte at its end.**
    //
    // This used to read the type at the object's *last* byte, for a real reason: the first byte of `win->w_buffer` is
    // the `w` of `win`, and asking there answered `win_T*` for a `buf_T*` chain (measured on neovim's `arglist.c`,
    // where the popup said `` `win_T*` has no member `b_fnum` ``). That reason is a property of *offsets*, and the
    // caller here has the **node**: `access.object` is the whole chain. Reading it directly is the same answer for
    // `win->w_buffer` and the right one for objects that end in a delimiter — `Pair<int, int>{1, 2}`'s last byte is
    // inside the braced list, and the offset question answered `int` for it, which is the "`int` has no member `sum`"
    // in the report.
    let cpp_code_analysis::Known::Yes(found) = session.type_of_the_expression(view, &access.object)
    else {
        // **Saying "I do not know" is the answer.** A reader who sees this knows the popup is about a type the analysis
        // could not read, and the next thing to do — annotate the declaration, or read the header it comes from — is
        // theirs. Nine unrelated declarations would tell them the same thing at four times the size, and worse.
        return format!(
            "`{}`\n\nThe type of `{object}` is not known here, so `{}` cannot be resolved. That usually means the \
             declaration's type is `auto`, a template parameter, or comes from a header this project has not read.",
            access.member, access.member
        );
    };
    let written = found.type_of;
    // **An arrow reaches the pointee's members**, and the completion already says so (`the_class_after_the_operator`):
    // `ptr->c_str` on a `std::shared_ptr<std::string>` is `std::string`'s `c_str`, and asking `members_of` about the
    // *written* type instead is what made this popup say "`std::shared_ptr<std::string>` has no member `c_str`" while
    // the list at the same cursor offered 187 members of `std::string`.
    //
    // **But the handle's own members answer too.** `ptr->reset` is not in the list the completion offers (a smart
    // pointer's `reset`/`get` are the handle's, not the pointee's), and a reader who writes it gets a popup: the
    // pointee is asked first, then the type as written. The two readers stay one — the first step is
    // `the_class_after_the_operator`, the second is the class the type names.
    let reached = match access.arrow {
        true => session.the_class_after_the_operator(view, &written, true),
        false => None,
    };
    let mut list = None;
    for class in reached.iter().chain(std::iter::once(&written)) {
        if let cpp_code_analysis::Known::Yes(candidates) = session.members_of(view, class) {
            if candidates
                .members
                .iter()
                .any(|member| member.fact.name == access.member)
            {
                list = Some(candidates);
                break;
            }
            list = list.or(Some(candidates));
        }
    }
    let Some(list) = list else {
        return format!(
            "`{written}` has no member `{}`\n\n`{object}` is a `{written}`, and its members could not be read — the \
             class is out of the index, or a base is.",
            access.member
        );
    };
    // **Every declaration of the name, not the first.** A class that declares `void call(Fn &&fn)` and
    // `void call(Fn &&fn) const` has two members, and which one is callable depends on the object's constness — which the
    // type model does not carry yet (`type_of` is `Foo` for both `Foo foo;` and `const Foo cfoo;`).
    let candidates: Vec<&cpp_code_analysis::ProjectMember> = list
        .members
        .iter()
        .filter(|member| member.fact.name == access.member)
        .collect();

    // **Which of them this call can reach**, when the cursor is on one. Members are where a reader meets an overload set
    // most often, and printing all nine `basic_string::append` declarations for a call that can reach two answers a
    // question nobody asked. The question is the free-function path's, over the member facts: same resolver, same
    // `parameter_list` spellings.
    let call_selected: Option<Vec<usize>> = (candidates.len() > 1)
        .then(|| {
            let parameter_lists: Vec<Option<String>> = candidates
                .iter()
                .map(|candidate| candidate.fact.parameter_list.clone())
                .collect();
            let template_parameters: Vec<Vec<String>> = candidates
                .iter()
                .map(|candidate| {
                    session
                        .text(&candidate.file)
                        .map(|text| {
                            cpp_code_analysis::sema::deduction::template_parameters_before(
                                &text,
                                candidate.fact.name_range.start_offset,
                            )
                        })
                        .unwrap_or_default()
                })
                .collect();
            session.the_declarations_a_call_means(
                view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(
                    access.member_range.start_offset,
                ),
                &parameter_lists,
                &template_parameters,
            )
        })
        .flatten();

    // **Exactly one of them is the call's** — then the card is that member's, with the details and the location a member
    // that is not overloaded gets, rather than a list of one.
    if let Some(selected) = call_selected
        .as_ref()
        .filter(|selected| selected.len() == 1)
        && let Some(member) = candidates.get(selected[0])
    {
        let definition = cpp_code_analysis::ProjectDefinition {
            file: member.file.clone(),
            fact: member.fact.clone(),
        };
        return declaration_markdown(session, view, &definition, None);
    }

    // **Several can take the call, or the cursor is not on one** — print what the call can reach, or all of them.
    let shown: Vec<&cpp_code_analysis::ProjectMember> = match &call_selected {
        Some(selected) => selected
            .iter()
            .filter_map(|at| candidates.get(*at).copied())
            .collect(),
        None => candidates.clone(),
    };
    if shown.len() > 1 {
        let mut out = String::from("```cpp\n");
        for candidate in &shown {
            out.push_str(&a_line(&candidate.fact));
            out.push('\n');
        }
        out.push_str("```\n\n");
        let scope = shown
            .iter()
            .find_map(|candidate| candidate.fact.scope.clone())
            .unwrap_or_else(|| written.clone());
        let sentence = if call_selected.is_some() {
            format!(
                "`{scope}::{}` — {} of {} declarations can accept this call's arguments; the types do not decide \
                 between them, so they are all shown. Which one a call means depends on `{object}`: a `const` object can \
                 only call the `const` one.",
                access.member,
                shown.len(),
                candidates.len()
            )
        } else {
            format!(
                "`{scope}::{}` — {} declarations, told apart by their qualifiers.\n\nWhich one a call means depends \
                 on `{object}`: a `const` object can only call the `const` one.",
                access.member,
                candidates.len()
            )
        };
        out.push_str(&sentence);
        return out;
    }
    let Some(member) = candidates.first().copied().or_else(|| {
        list.members
            .iter()
            .find(|member| member.fact.name == access.member)
    }) else {
        // **A member written through a macro is not a missing member**, and this is C's ordinary way of renaming a
        // field: neovim writes `#define b_fnum handle` *inside* `struct file_buffer`, so `buf->b_fnum` is
        // `buf->handle` after the preprocessor while the file's own text has no member called `b_fnum` at all.
        //
        // Measured through the server on neovim's `arglist.c`: the popup said `` `buf_T*` has no member `b_fnum` `` —
        // true about this reading, and useless to the reader. The expansion is the answer, and it costs **one query
        // on the miss**, which is why it is asked here rather than for every member hover.
        if let Some(expansion) =
            session.macro_body_at(&view.path, access.member_range.start_offset, &access.member)
        {
            return format!(
                "`{}` is a macro here: it expands to `{expansion}`, and `{expansion}` is a member of `{written}`.\n\n\
                 This hover reads the file's own text, where the macro name is what was written.",
                access.member
            );
        }
        return format!(
            "`{written}` has no member `{}`\n\n`{object}` is a `{written}`.",
            access.member
        );
    };

    let definition = cpp_code_analysis::ProjectDefinition {
        file: member.file.clone(),
        fact: member.fact.clone(),
    };
    declaration_markdown(session, view, &definition, None)
}

/// **Every declaration of one name**, as one popup — what a hover says about an overloaded function.
///
/// # Why a signature per declaration rather than each declaration's text
///
/// Because the question is a choice: a reader hovering `std::format` wants to know what they can call, and a code
/// block holding four whole definitions (bodies and all) answers a different question at four times the size. The
/// line is built from the fact — the return type and the parameter list, both of which the index records — so this
/// costs no parsing, whoever's header it is.
///
/// A declaration whose parameters were never read (a fact written before the field existed) still gets a line, with
/// the `(…)` the old detail line used: less than the truth, and not more.
fn overloads_markdown(
    session: &Session<DiskFiles>,
    view: Option<&FileView>,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    found: &cpp_code_analysis::ProjectDefinitions,
) -> String {
    // **Which of them this call can reach, if the reader is on a call.**
    //
    // `the_declarations_a_call_means` is the same resolution the argument checks and the signature popup use, asked over
    // the declarations this hover holds: one index when it decided, the **viable set** when it did not. It is `None`
    // when the cursor is not on a call at all (a bare `std::format`), and then the list is all there is.
    //
    // The viable set is what a reader wants printed. Hovering `append` in `result_str.append(x)` used to draw **nine**
    // declarations of `basic_string::append`, several of which cannot accept one argument at all — a page about a call
    // that cannot be that call.
    let call_selected: Option<Vec<usize>> = view.and_then(|view| {
        let parameter_lists: Vec<Option<String>> = found
            .found
            .iter()
            .map(|declaration| declaration.fact.parameter_list.clone())
            .collect();
        // One `text` read per declaration, and the files are held afterwards.
        let template_parameters: Vec<Vec<String>> = found
            .found
            .iter()
            .map(|declaration| {
                session
                    .text(&declaration.file)
                    .map(|text| {
                        cpp_code_analysis::sema::deduction::template_parameters_before(
                            &text,
                            declaration.fact.name_range.start_offset,
                        )
                    })
                    .unwrap_or_default()
            })
            .collect();

        let selected = session.the_declarations_a_call_means(
            view,
            offset,
            &parameter_lists,
            &template_parameters,
        )?;
        (!selected.is_empty()).then_some(selected)
    });
    // **How many of them the popup prints.** A name with a dozen overloads — `std::format` has one per character
    // type and per allocator — drew a page: the reader asked what the name is, and the answer filled the screen and
    // pushed the sentence that explains it off the bottom (reported from an editor as "hover 结果很多").
    //
    // **On a call, the card prints exactly the declarations the call can reach** — one of them when the resolution
    // decides, several when the types do not. Off a call it prints the first few, and the count and the files are
    // said either way; `go to definition` lists them all.
    const MAX_SHOWN: usize = 6;
    let chosen_index = call_selected
        .as_ref()
        .filter(|selected| selected.len() == 1)
        .map(|selected| selected[0]);
    let mut shown: Vec<usize> = match &call_selected {
        Some(selected) => selected.clone(),
        None => (0..found.found.len()).collect(),
    };
    let hidden = found.found.len().saturating_sub(shown.len().min(MAX_SHOWN));
    shown.truncate(MAX_SHOWN);

    let mut lines = String::new();
    let mut shown_lines: Vec<String> = Vec::new();
    for index in &shown {
        let declaration = &found.found[*index];
        let is_the_one = chosen_index == Some(*index);
        // **A declaration the file's own text splits across a directive has no written type** — see
        // [`DeclFact::split_by_a_directive`]. The handler holds the session, so it asks the reading where the
        // directive is *resolved*: `bits/stl_bvector.h` writes `emplace_back` returning `reference` under
        // `#if __cplusplus > 201402L` and `void` under `#else`, and only the rendering knows which one this
        // compilation means.
        let mut fact = declaration.fact.clone();
        if fact.split_by_a_directive
            && let Some(read) = session.fact_from_the_rendering(
                &declaration.file,
                &fact.name,
                fact.scope.as_deref(),
            )
        {
            fact.returns = read.returns;
            fact.type_of = read.type_of;
        }
        let mut line = a_line(&fact);
        if is_the_one
            && let Some(view) = view
            && let Some(text) = session.text(&declaration.file)
        {
            // **The instantiated spelling, when the call fixes it.** `take(T* value)` is what the declaration says and
            // `take(int* value)` is what this call means; a reader who sees only the first cannot tell that the analysis
            // deduced anything. The substitution is whole-word (`types::substituted_spelling`), so `T` does not reach
            // into `Trait`.
            let names = cpp_code_analysis::sema::deduction::template_parameters_before(
                &text,
                declaration.fact.name_range.start_offset,
            );
            let (parameters, _) = declaration
                .fact
                .parameter_list
                .as_deref()
                .map(cpp_code_analysis::sema::overload::parameters_in)
                .unwrap_or_default();
            if let Some(arguments) =
                session.the_deduced_arguments(view, offset, &names, &parameters)
            {
                line =
                    cpp_code_analysis::sema::types::substituted_spelling(&line, &names, &arguments);
            }
        }
        // **One line per distinct signature.** A name declared twice with the same shape — `std::is_same_v` is
        // libstdc++'s `#if __cplusplus > …` branch and its `#else` — printed the same sentence twice and read as an
        // overload set with two identical members. The count of declarations is still said below; the block is what a
        // reader compares, and two identical lines compare to nothing.
        let marked = format!("{}{line}", if is_the_one { "→ " } else { "  " });
        let duplicate = shown_lines
            .iter()
            .any(|existing| existing.trim_start_matches("→ ").trim_start_matches("  ") == line);
        shown_lines.push(marked);
        if !duplicate {
            lines.push_str(&format!("{}{line}\n", if is_the_one { "→ " } else { "  " }));
        }
    }

    // **The file a reader is sent to** is the declaration the card is about: the one the call selects when it selects
    // one, and the first declaration otherwise.
    let first = &found.found[chosen_index.unwrap_or(0)];
    let qualified = first.fact.qualified_name();
    let distinct = shown_lines
        .iter()
        .map(|line| line.trim_start_matches("→ ").trim_start_matches("  "))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let mut out = code_block(lines.trim_end());

    out.push_str(&format!(
        "\n`{qualified}` — {} declarations. {}\n",
        found.found.len(),
        if let Some(selected) = &call_selected {
            if selected.len() == 1 {
                "The `→` one is what this call means: its parameter list accepts the arguments, and the others \\
                 cannot. `go to definition` lists them all."
            } else {
                "These are the declarations whose parameter list accepts this call's arguments; the types do not \\
                 decide between them, so they are all shown — `go to definition` lists the rest."
            }
        } else if hidden > 0 {
            "The ones printed here are the call's own (when the cursor is on one) and the first of the rest — the \
             count and the files below are complete, and `go to definition` lists them all."
        } else if distinct == 1 {
            // **One shape, several declarations.** A variable template written under `#if`/`#else`, a redeclaration in a
            // header it includes, an `inline constexpr` defined twice — none of them is an overload set, and the arguments
            // sentence is simply false about them.
            "The declarations spell the **same signature** — a redeclaration, or the two branches of a conditional \
             header — so which one is in effect depends on the compilation, not on a call."
        } else {
            "Which one a call means is decided by the arguments, so they are all here rather than one of them being \
             guessed at."
        }
    ));

    if found.conditional > 0 {
        out.push_str(&format!(
            "\n{} more declaration(s) of this name are reachable only through a conditional `#include`, so whether \
             they are in scope depends on the compilation.\n",
            found.conditional
        ));
    }

    let files: std::collections::BTreeSet<&std::path::Path> = found
        .found
        .iter()
        .map(|declaration| declaration.file.as_path())
        .collect();
    out.push_str(&format!(
        "\nDeclared in {}",
        where_clause(
            session,
            view,
            first.file.as_path(),
            first.fact.name_range.start_offset
        )
    ));
    if files.len() > 1 {
        out.push_str(&format!(" and {} other file(s)", files.len() - 1));
    }

    out
}

/// One declaration, as the line a hover shows: `string format(const _Fmt, _Args...)`.
/// Can the facts spell this declaration as a signature? A return type, a parameter list or a written type is what the
/// reconstruction needs; a class or a namespace has none of them, and keeps its raw text.
/// **The return type a specifier sequence has once the macros in it are dropped** — `WINBOOL` for
/// `WINBASEAPI WINBOOL WINAPI SetConsoleOutputCP(…)`.
///
/// The question is asked only when the card's return type **ends in a name the project defines as a macro**
/// ([`crate::ProjectIndex::files_defining_macro`], a name-table lookup). Then the written text before the declarator
/// is read out of the file, split into identifiers, and every identifier that is a macro somewhere is dropped — which
/// is what a compiler's expansion leaves — and what remains is the type.
///
/// `None` means "leave the fact's answer alone" — the ordinary case, and also every case the text cannot answer: a
/// declaration whose written text holds no specifiers (a constructor, a `using`), or one where dropping the macros
/// would leave nothing.
fn the_type_the_macros_leave<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    file: &std::path::Path,
    fact: &DeclFact,
    returns: &str,
) -> Option<String> {
    let last = returns.split_whitespace().next_back()?;
    if !last
        .chars()
        .all(|character| character.is_alphanumeric() || character == '_' || character == ':')
    {
        return None;
    }
    if session.index().files_defining_macro(last).is_empty() {
        return None;
    }

    let text = session.text(file)?;
    let before = text.get(..fact.name_range.start_offset)?;
    // The specifiers are what stands between the end of the previous declaration and this name: a `;`, `{`, `}`,
    // `#` or a blank line ends them. Read by line rather than by token, because the card is showing a spelling and
    // not parsing a declaration.
    let mut specifiers = String::new();
    for line in before.lines().rev() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.ends_with(';') || line.ends_with('{') {
            if !specifiers.is_empty() {
                break;
            }
            continue;
        }
        if line.ends_with('}') {
            break;
        }
        specifiers.insert_str(0, line);
        specifiers.insert(0, ' ');
        // Two lines is the most a return type is ever broken across in practice, and a third would be a guess.
        if specifiers.matches(' ').count() > 24 {
            break;
        }
    }
    if specifiers.trim().is_empty() {
        return None;
    }

    let mut kept: Vec<&str> = Vec::new();
    for word in specifiers.split_whitespace() {
        let trimmed = word.trim_matches(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == ':')
        });
        if trimmed.is_empty() {
            continue;
        }
        if session.index().files_defining_macro(trimmed).is_empty() {
            kept.push(word);
        }
    }
    if kept.is_empty() {
        return None;
    }

    let spelling = kept.join(" ");
    let spelling = spelling.trim().to_string();
    (!spelling.is_empty() && spelling != returns).then_some(spelling)
}
/// **The declaration's line, rebuilt** — the renderer's single-line form, for a card or an overload list.
///
/// The type writer and the parameter parser live in [`cpp_code_analysis::sema::render`], so a line printed in a list and
/// the same line printed above a card are the same line, generated the same way.
fn a_line(fact: &DeclFact) -> String {
    cpp_code_analysis::sema::render::a_line(fact, None, None)
}

pub(crate) fn markdown(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

/// **A header, as the file it resolved to** — what a hover on `#include <vector>` says.
///
/// One line and a path, because that is the whole answer: the reader wrote a name, a search turned it into a file,
/// and the file is the thing they cannot see. The **delimiters are the form the directive used**, put back on the
/// name so that the line reads as the reader wrote it — the two spellings search differently, and seeing which one
/// was applied is how a reader tells "found beside the file" from "found on the search path".
fn header_markdown(header: &cpp_code_analysis::HeaderTarget) -> String {
    let (open, close) = match header.form {
        cpp_code_analysis::IncludeForm::Quote => ('"', '"'),
        // A macro include never reaches here: its target is a macro, and the question about it is the macro
        // question, which is asked above. Both remaining forms are the angle one as far as a reader is concerned.
        cpp_code_analysis::IncludeForm::Angle | cpp_code_analysis::IncludeForm::Macro => ('<', '>'),
    };

    format!(
        "```cpp\n#include {open}{}{close}\n```\n\n`{}`\n",
        header.spelling,
        header.resolved.display()
    )
}

/// A macro, as the definition writes it.
fn macro_markdown(
    session: &Session<DiskFiles>,
    view: &FileView,
    found: &ProjectMacro,
    at: Option<cpp_code_analysis::file::coordinates::FileOffset>,
) -> String {
    let fact = &found.fact;

    if !fact.kind.is_definition() {
        return format!(
            "`#undef {}`{}\n\n`{}` is not a macro at this point in the file.",
            fact.name,
            where_clause(session, Some(view), &found.file, fact.range.start_offset),
            fact.name
        );
    }

    let mut out = String::new();
    let definition = definition_line(
        session,
        view,
        &found.file,
        fact.range.start_offset,
        fact.body_range,
    );
    out.push_str(&code_block(&definition.text));

    out.push_str(&format!(
        "\n`{}` — a {}{}, defined in {}\n",
        fact.name,
        if fact.function_like {
            "function-like macro"
        } else {
            "macro"
        },
        match &fact.value {
            Some(value) => format!(" whose value is `{value}`"),
            None => String::new(),
        },
        where_clause(session, Some(view), &found.file, fact.range.start_offset)
    ));

    // **What the invocation expands to**, when the cursor is on one. A macro's card otherwise says what the `#define`
    // wrote and leaves the substitution to the reader, which is the one thing a macro makes hard: `TEST_CPP_MAX(2, 5)`
    // is `((2) > (5) ? (2) : (5))` only after the arguments go in. Reported from an editor, with a screenshot of exactly
    // that card.
    //
    // The rules applied are textual and the sentence says which: `#x` and `a ## b` are not applied, and the result's own
    // macro names are not expanded again. A reader comparing this with a compiler must know the difference.
    if let Some(at) = at
        && let Some(expanded) = session.the_expansion_of_a_macro(view, at)
        && !expanded.trim().is_empty()
    {
        out.push_str(&format!(
            "\nExpands to `{}`\n\n*A textual expansion: `#` and `##` are not applied, and the result is not expanded again.*\n",
            expanded.trim()
        ));
    }

    if let Some(documentation) =
        documentation_of_a_fact(session, view, &found.file, fact.range.start_offset)
    {
        out.push_str(&format!("\n{documentation}\n"));
    }

    out
}

/// **The template-id the reader wrote at the cursor**, if that is where the cursor is: `std::map<std::string, int>`.
///
/// # Why the use is read and not the declaration
///
/// A class template's fact describes the **declaration** — the parameter names, in the order the header wrote them —
/// and a use says only which arguments were passed. Both are needed for the card a reader wants, and the index can
/// only give the first: `map<_Kty, _Ty, _Pr, _Alloc>` is the shape, `std::map<std::string, int>` is what the file in
/// front of them says. So the spelling is read from the file, where it is written exactly once.
///
/// The walk is [`cpp_code_analysis::sema::resolve::name_node_at`] — the innermost name node holding the offset —
/// and the parser folds a whole qualified name with its argument list into that one node (see `read_named`), so its
/// text *is* the spelling. `None` when the name has no arguments (`Widget`), which is the case the declaration's own
/// text already answers.
fn written_template_id(
    view: &FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
) -> Option<String> {
    let node = cpp_code_analysis::sema::resolve::name_node_at(&view.root, offset.get())?;
    let written = node.text().to_string();
    let written = written.trim();

    (written.contains('<') && !written.is_empty()).then(|| written.to_string())
}

fn declaration_markdown(
    session: &Session<DiskFiles>,
    view: &FileView,
    found: &ProjectDefinition,
    written: Option<&str>,
) -> String {
    // **A return type read as a macro's name is corrected here.** `wincon.h` writes
    //
    // ```c
    // WINBASEAPI WINBOOL WINAPI SetConsoleOutputCP(UINT wCodePageID);
    // ```
    //
    // and its own text defines neither `WINBASEAPI` nor `WINAPI` — they arrive from the includer (`windows.h`).
    // The grammar's rule for a sequence of names is "the last one wins" (it is what fixes
    // `_NODISCARD _CONSTEXPR20 reference back()`), which picks `WINAPI`, so the card read "a function returning
    // `WINAPI`" — reported from an editor, with a screenshot. The index knows better: the name table says which
    // names are macros anywhere in the project (`files_defining_macro`), and the written specifier sequence minus
    // those names is the type the compiler would have seen. See `the_type_the_macros_leave`.
    // **A binding is a place to jump to; the card wants the declaration's facts.** `ProjectDefinition::from_binding`
    // records no type and no return type, deliberately — they are facts about the file's text, and a binding carries
    // none of them. A hover on a function's own definition therefore printed `make_sized()` and "a function", while the
    // index held `std::array<int, Size>` (reported, fig 5; `facts` and the card disagreed about the same declaration).
    //
    // The index is asked only when the binding's fact has **nothing to compose from** — no type, no return type, no
    // parameter list — so a declaration the file spells keeps its own answer, and one the index does not hold falls back
    // to the binding rather than to an empty card.
    // **A binding is a place to jump to; the card wants the declaration's facts.** `ProjectDefinition::from_binding`
    // records no type and no return type, deliberately — they are facts about the file's text, and a binding carries
    // none of them. A hover on a function's own definition therefore printed `make_sized()` and "a function", while the
    // index held `std::array<int, Size>`; and a **member alias** hovered at its declaration printed the bare name
    // (`value_type`) where the index holds `T` — measured with `inspect hover` on a class template.
    //
    // The index is asked only when the binding's fact has **nothing to render from** — no type, no return type, no
    // parameter list — so a declaration the file spells keeps its own answer, and one the index does not hold falls back
    // to the binding rather than to an empty card.
    // **A local is never looked up by name in the index.** A binding carries no type, so the condition below was
    // always true for one — and the index lookup it triggered was a *bare name*, because a local's
    // `qualified_name()` is just its spelling (`scope` is `None`: a function body contributes no segment). Measured
    // on `EmmyLuaCodeStyle`'s `LSPHandle.cpp:139`, `auto result = std::make_shared<lsp::DocumentFormattingResult>();`:
    // the card for the variable `result` was `` `codecvt_base::result` — a type of type `int`, in `codecvt_base`,
    // declared in `xlocale:581:11` `` — a real declaration of the same spelling from the standard library, inside a
    // conditional block. `definitions` and `type_at` had the local right; only the card re-asked. `LSP.cpp:12`'s
    // `auto object = nlohmann::json::object();` was the same shape: a variable shown as the static function
    // `basic_json::object(initializer_list_t init = ...)`. See `docs/buglist.md` section 7.
    // **A type is never re-asked either.** A class fact has no type, no return type and no parameter list — the
    // fields the re-ask exists to recover from a *binding* — so the condition below was true for every class, and
    // the name lookup replaced the declaration the caller had chosen with whatever the bare spelling answered:
    // `std::vector<std::string>`'s card was a forward declaration in `<format>` and then the `vector<bool>`
    // specialization, never the definition the hover had just selected (`CPPLS_TRACE_HOVER=1` shows
    // `[card] heard vector … => indexed stl_bvector.h:vector`). See `docs/buglist.md` section 10.
    let indexed = if !found.fact.local
        && !matches!(
            found.fact.kind,
            cpp_code_analysis::DeclKind::Type | cpp_code_analysis::DeclKind::Namespace
        )
        // **Nothing to render at all**, which is what a *binding* leaves behind: a binding is a place to jump to and
        // carries no type, no return type, no parameter list and no annotation.
        //
        // **`||` here was a defect, and this is the line the whole `std::optional<V>` report ended at.** The
        // annotation clause was the last one written, and as a disjunction it made the re-ask true for **every** fact
        // without a reflection annotation — which is every ordinary declaration — so a fact that already had
        // everything was thrown away and the *index's* copy asked for. The copy is unsubstituted by construction:
        // `ProjectIndex::definition` answers the spelling the class wrote, while the caller here had a member list
        // whose `Box<int>` arguments had already been substituted through it. Measured: `members_of("Box<int>")`
        // answered `returns: Some("std::optional<int>")`, and the card printed `std::optional<V>` because of this
        // `||`. A conjunction asks exactly the question the comment asks — "does this fact have nothing at all".
        && found.fact.type_of.is_none()
        && found.fact.returns.is_none()
        && found.fact.parameter_list.is_none()
        && found.fact.reflection.is_none()
    {
        session
            .index()
            .definition(&found.fact.qualified_name(), &found.file)
            .value()
    } else {
        None
    };
    if std::env::var("CPPLS_TRACE_HOVER").is_ok() {
        eprintln!(
            "[card] heard {} local {} type_of {:?} => indexed {}",
            found.fact.name,
            found.fact.local,
            found.fact.type_of,
            indexed
                .as_ref()
                .map(|seen| format!("{}:{}", seen.file.display(), seen.fact.name))
                .unwrap_or_else(|| "no".to_string())
        );
    }
    let found = match &indexed {
        Some(indexed) => indexed,
        None => found,
    };

    let mut corrected = found.fact.clone();
    // **A deduced type is shown as the type it deduced to.** The fact records what the file *wrote* — `auto&` — which
    // is the spelling a lookup wants and no answer at all to "what is this". The engine has the answer (`type_at`, the
    // same one the inlay hint draws), so the card shows it: measured on a user's file,
    // `auto &syntaxTree = opSyntaxTree.value();` hovers as "`syntaxTree` — a variable, declared in a function body"
    // with no type, while the inlay hint on the same line draws `LuaSyntaxTree&`.
    //
    // The spelling replaces the written one **whole**: the deduced type already carries the `&` (or `&&`) that the
    // declarator writes, so nothing is appended beside it and nothing of `auto` is kept.
    if matches!(corrected.kind, DeclKind::Variable)
        && corrected
            .type_of
            .as_deref()
            .is_none_or(cpp_code_analysis::sema::types::is_a_deduced_type)
        && is_the_viewed_file(view, &found.file)
        && let Known::Yes(deduced) = session.type_at(
            view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(
                corrected.name_range.start_offset,
            ),
        )
    {
        corrected.type_of = Some(deduced.type_of.to_string());
    }
    if let Some(returns) = corrected.returns.clone()
        && let Some(better) = the_type_the_macros_leave(session, &found.file, &corrected, &returns)
    {
        corrected.returns = Some(better);
    }
    // **The annotation the file writes, when the index has not recorded it yet.** `UCLASS(Blueprintable)` is read into
    // the fact from the file's text, and the summary that holds it is built by the pump — a hover can arrive first.
    // The declaring file's own view is right here (the node below is taken from it), and its facts are the same facts
    // the summary would hold, so the card reads them rather than showing a class with no annotation.
    if corrected.reflection.is_none()
        && let Some(facts) = session
            .view_of_the_file(&found.file)
            .map(|view| session.facts_of_the_view(&view))
        && let Some(the_files) = facts
            .iter()
            .find(|theirs| theirs.name_range.start_offset == corrected.name_range.start_offset)
    {
        corrected.reflection = the_files.reflection.clone();
    }
    let fact = &corrected;
    if std::env::var_os("CPPLS_DEBUG_CARD").is_some() {
        eprintln!(
            "[card] name={:?} kind={:?} scope={:?} returns={:?} parameter_list={:?} range={:?}",
            fact.name, fact.kind, fact.scope, fact.returns, fact.parameter_list, fact.range
        );
    }
    let mut out = String::new();

    // **The instantiation the reader wrote is the signature they asked about.** A class template's fact describes
    // the *declaration* — `pattern` is `map<_Kty, _Ty, _Pr, _Alloc>`, which is what the header says — while the
    // cursor is on `std::map<std::string, LuaTokenKind, std::less<>>` and the reader is asking what *that* is. The
    // card showed only the declaration's parameter names, which names nothing the reader can see (reported from an
    // editor, on `LuaLexer.cpp`'s `LuaReserved`). So the written spelling becomes the signature line and the
    // subject of the sentence, and the face the fact describes follows it.
    let subject = match written {
        Some(written) if matches!(fact.kind, DeclKind::Type) && !fact.parameters.is_empty() => {
            written
        }
        _ => "",
    };

    // **Reconstructed from the facts when they can spell it, and copied only when they cannot.** The raw text of a
    // declaration is what a *reader* cannot use in a popup: it carries the vendor's macro noise (`& _CLR_OR_THIS_CALL
    // read(…)` for `basic_istream::read`), the lines the file happened to break it across, and the spelling of the one
    // declaration that was found rather than the shape of the thing. The signature line is the same reconstruction the
    // overload popup uses, so the two answers cannot disagree — and a declaration whose facts are too thin to spell (a
    // class, a namespace) keeps the raw text rather than a worse guess.
    // **An enumerator is drawn as one, with the value the compilation gives it.** `OPR_MUL` is a `Variable` in the
    // fact — that is the kind a name in a scope has, and `DeclKind` has no enumerator — so a card that only asks the
    // fact says "`BinOpr::OPR_MUL` — a variable", which is true and useless: what a reader hovering an enumerator
    // wants is which set it belongs to and **what number it is** (reported from an editor: "the member has no hover,
    // and if it did it should show the value"). The value is not in the fact: it is computed from the enumerators
    // written before it, which is what the language does and what the raw text of the file holds.
    // **The value, folded the way a compiler folds it** — `TK_AND = FIRST_RESERVED` and `const int FIRST_RESERVED =
    // UCHAR_MAX + 1;` are the two shapes an editor reported as "no value shown". The layer is
    // `sema::constant`, which also says what it refuses; `enumerator_value` stays the question "is this an
    // enumerator at all", which decides the sentence below.
    let enumerator = enumerator_value(view, fact.name_range.start_offset);
    // **The value is folded in the unit where the declaration is read**, not in the file the cursor happens to be
    // in: a header's `UCHAR_MAX` comes from `<climits>`, which the header itself need not include — every `.cpp` that
    // includes it does. See [`cpp_code_analysis::Session::constant_value_of_a_declaration`].
    let constant_value =
        session.constant_value_of_a_declaration(&found.file, fact.name_range, &view.path);

    // **The code block is generated from the model** — see [`cpp_code_analysis::sema::render`]. Not the declaration's
    // text, not the fact's strings joined together, and no `template <…>` cut out of the file: a type is a `Type`
    // printed by its own writer, a parameter list is parsed and rebuilt, and the head comes from the declaration's
    // template parameters. The renderer is the one place that decides this, so the card, the overload list and the
    // member list cannot disagree.
    let node = session.the_declaration_node(&found.file, fact.range.start_offset);
    let declaration = cpp_code_analysis::sema::render::an_item(fact, node.as_ref(), constant_value);
    out.push_str(&code_block(&declaration.join("\n")));
    // **The instantiation the reader wrote**, when it is not what the declaration says. A class template's fact
    // describes the declaration; the cursor may be on `std::map<std::string, LuaTokenKind>` and the signature that
    // matters is the class's. Parsed by the type writer like everything else.
    if !subject.is_empty() {
        out.push_str(&format!(
            "\nAs written here: `{}`\n",
            cpp_code_analysis::sema::render::a_type(subject)
        ));
    }
    let qualified = if subject.is_empty() {
        fact.qualified_name()
    } else {
        subject.to_string()
    };
    match enumerator {
        Some(_) => out.push_str(&format!(
            "\n`{}` — an enumerator of `{}`\n",
            qualified,
            fact.scope.as_deref().unwrap_or("(an anonymous enum)")
        )),
        None => out.push_str(&format!("\n`{}` — {}\n", qualified, kind_words(fact))),
    }

    // **A variable whose type is deduced from an initializer that could not be typed** — say why, rather than
    // leaving the reader with a popup that mentions no type at all. `auto enenn = std::format(…)` was reported for
    // exactly this: the card said "a variable" and nothing else, and the reason the analysis had — `std::format` is
    // declared more than once in what this file can see, with nothing choosing between the declarations — was never
    // shown. The sentence is the analysis's own (`UnknownReason::describe`), not a second opinion.
    if matches!(fact.kind, DeclKind::Variable)
        && fact
            .type_of
            .as_deref()
            .is_none_or(|written| written == "auto")
        && is_the_viewed_file(view, &found.file)
        && let Known::Unknown(reason) = session.type_at(
            view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(fact.name_range.start_offset),
        )
    {
        out.push_str(&format!(
            "\nThe type is deduced from the initializer, and that could not be typed: {}.\n",
            reason.describe()
        ));
    }

    if let Some(value) = constant_value {
        let how = if enumerator.is_some() {
            "counted from the enumerators written before it, as a compiler does."
        } else {
            "folded from its initializer, as a compiler does."
        };
        out.push_str(&format!("\nValue **{value}** — {how}\n"));
    }

    // The comment the file writes above the declaration, in the place clangd and the C++ extension put it: after
    // the signature, before where it was declared from.
    if let Some(documentation) =
        documentation_of_a_fact(session, view, &found.file, fact.range.start_offset)
    {
        out.push_str(&format!("\n{documentation}\n"));
    }

    out.push_str(&format!(
        "\nDeclared in {}",
        where_clause(
            session,
            Some(view),
            &found.file,
            fact.name_range.start_offset
        )
    ));

    // **The reflection annotation the declaration was written under.** `UCLASS(Blueprintable)` and
    // `UPROPERTY(EditAnywhere, Category="Game")` are empty macros: the tree has nothing of them, and the card a reader
    // gets for a member said only its type. The specifiers are what the engine's build tool acts on — the reason the
    // declaration is what it is to the editor — so the card shows them exactly as the file wrote them.
    if let Some(reflection) = &fact.reflection {
        out.push_str(&format!(
            "\n\n`{}({})`",
            reflection.macro_name,
            reflection.specifiers.join(", ")
        ));
        out.push_str(match reflection.specifiers.len() {
            0 => "\n\nThe declaration is annotated for Unreal's reflection system, with no specifiers.",
            _ => "\n\nThe specifiers Unreal's reflection system acts on, as the declaration wrote them.",
        });
    }

    if let FactGuard::Region(_) = fact.guard {
        out.push_str(
            "\n\nThis declaration is inside a conditional block, so whether it exists depends on the compilation \
             — the analysis is reporting what the file says, not what a compiler concluded.",
        );
    }

    if !fact.clean {
        out.push_str(
            "\n\nThe parser recovered from an error inside this declaration, so its text may not mean what it \
             looks like.",
        );
    }

    out
}

/// The comment a declaration is documented by, as markdown — `None` when the file writes none.
///
/// # Why only a comment written *as* documentation
///
/// The parser answers "which comment documents this declaration" for any comment above it, and it also answers
/// whether that comment is written as documentation ([`cpp_parser::CppDocComment::is_documentation`]): `///`, `//!`, `/**`,
/// `/*!`. This asks the second question as well, because of what a plain `//` comment above a declaration usually
/// is in a real codebase — `// TODO`, `// NOLINT`, a banner of slashes — and a popup that showed every one of them
/// would dress a note to the reader as an interface description. A codebase that documents with plain `//` gets no
/// hover text: a deliberate miss rather than a silent wrong answer.
///
/// # Why the comment is shown as it was written
///
/// Doxygen's `@brief` and `@param` are left as the file spells them rather than turned into a markdown list. That
/// is the same choice clangd and the C++ extension make, and the alternative is a *rendering policy* the file did
/// not write: the structure is available (`CppDocComment::get_commands`) for the feature that needs it, and
/// signature help — which has to line a parameter up with its documentation — is where that will be.
fn documentation_markdown(
    session: &Session<DiskFiles>,
    view: &FileView,
    file: &std::path::Path,
    offset: usize,
) -> Option<String> {
    documentation_text(&session.documentation(view, file, offset)?)
}

/// The comment as markdown — `None` when it is not written as documentation, or says nothing.
///
/// Shared with signature help, which shows the same comment beside the parameter being typed: one rendering, so
/// that two popups about one declaration cannot disagree about what it says.
pub(crate) fn documentation_text(comment: &CppDocComment) -> Option<String> {
    if !comment.is_documentation() {
        return None;
    }

    // Delimiters, line markers and the blank lines around them are the parser's reading; an empty comment — `///`
    // with nothing after it — has nothing to show.
    let text = comment.get_comment_text();
    let text = text.trim();

    (!text.is_empty()).then(|| render_documentation(text))
}

/// **A documentation comment as markdown** — its Doxygen commands rendered rather than dumped.
///
/// # Why this is not "show the comment as it was written"
///
/// That was the rule, and a screenshot of the result is why it changed. Markdown joins a paragraph's lines, so
///
/// ```text
/// /// @brief Prints a message with a preceding newline.
/// /// @param message The message to print after a newline.
/// /// @param count The number of times to print the message after a newline.
/// ```
///
/// — the ordinary shape of a documented C++ function — arrived in the popup as **one run-on paragraph** with the tags
/// still in it, and `@param` read as a word in a sentence rather than as the name of a parameter. The rule's own
/// argument ("a rendering policy the file did not write") is about *inventing* structure; the file wrote this
/// structure, and Doxygen's tags are its spelling of it.
///
/// # What is rendered, and what is left alone
///
/// A **command is a block**. `@brief`/`@details` become their own paragraph; `@param`/`@tparam`/`@return`/`@note`/
/// `@warning`/`@see`/`@deprecated` become one list, with the parameter's name or the label in bold; a continuation
/// line belongs to the block above it; a blank line ends the block. `\brief` is the same command as `@brief` — both
/// spellings are Doxygen's — and a command this table does not know is shown **as it was written**, in prose, because
/// a policy that silently dropped the parts it did not recognise would be worse than the run-on paragraph.
fn render_documentation(text: &str) -> String {
    // The blocks of the answer, in the order the file wrote them, and the **one list** they share: Doxygen's
    // parameter and label commands are one list even when a `@note` sits between two `@param`s.
    let mut blocks: Vec<String> = Vec::new();
    let mut list: Vec<String> = Vec::new();
    let mut prose: Option<String> = None;

    fn end_prose(blocks: &mut Vec<String>, prose: &mut Option<String>) {
        if let Some(text) = prose.take()
            && !text.trim().is_empty()
        {
            blocks.push(text);
        }
    }
    fn end_list(blocks: &mut Vec<String>, list: &mut Vec<String>) {
        if !list.is_empty() {
            blocks.push(list.join("\n"));
            list.clear();
        }
    }
    fn say(prose: &mut Option<String>, line: &str) {
        match prose {
            Some(text) => {
                text.push(' ');
                text.push_str(line);
            }
            None => *prose = Some(line.to_string()),
        }
    }

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            end_prose(&mut blocks, &mut prose);
            end_list(&mut blocks, &mut list);
            continue;
        }

        let Some((command, rest)) = command_of(trimmed) else {
            // **A line with no command continues the block above it** — a parameter's description that ran onto the
            // next line, or a paragraph that did. With nothing open it starts a paragraph of its own.
            if let Some(prose) = &mut prose {
                prose.push(' ');
                prose.push_str(trimmed);
            } else if let Some(item) = list.last_mut() {
                item.push(' ');
                item.push_str(trimmed);
            } else {
                say(&mut prose, trimmed);
            }
            continue;
        };

        match command {
            "param" | "tparam" | "arg" | "return" | "returns" | "result" | "note" | "warning"
            | "attention" | "see" | "deprecated" | "todo" | "pre" | "post" | "remark" => {
                end_prose(&mut blocks, &mut prose);
                let item = match command {
                    // `@param[in] name description` — the direction is part of the command, not of the name, and it
                    // is worth keeping: "this parameter is read" is the first thing the description has to be read
                    // against.
                    "param" | "tparam" | "arg" => {
                        let (direction, rest) = direction_of(rest);
                        match rest.split_once(char::is_whitespace) {
                            Some((name, description)) => match direction {
                                Some(direction) => {
                                    format!("- **`{name}`** ({direction}) — {}", description.trim())
                                }
                                None => format!("- **`{name}`** — {}", description.trim()),
                            },
                            None => format!("- **{}** — {rest}", t!("doc.parameter")),
                        }
                    }
                    "return" | "returns" | "result" => {
                        format!("- **{}** — {rest}", t!("doc.returns"))
                    }
                    "note" => format!("- **{}** — {rest}", t!("doc.note")),
                    "warning" => format!("- **{}** — {rest}", t!("doc.warning")),
                    "attention" => format!("- **{}** — {rest}", t!("doc.attention")),
                    "see" => format!("- **{}** — {rest}", t!("doc.see")),
                    "deprecated" => format!("- **{}** — {rest}", t!("doc.deprecated")),
                    "todo" => format!("- **{}** — {rest}", t!("doc.todo")),
                    // A label this table has no translation for keeps the command's own spelling, capitalised:
                    // `@pre` reads as **Pre**, which is what the file wrote and what Doxygen calls it.
                    other => format!("- **{}** — {rest}", capitalize(other)),
                };
                list.push(item);
            }
            // `@brief` and `@details` are what a paragraph says, so they open one.
            "brief" | "details" | "short" => {
                end_list(&mut blocks, &mut list);
                say(&mut prose, rest);
            }
            // **A command this table does not know is shown as the file wrote it**, in a block of its own — dropping
            // it would be a policy nobody asked for, and merging it into the paragraph above hides where it starts.
            _ => {
                end_list(&mut blocks, &mut list);
                end_prose(&mut blocks, &mut prose);
                say(&mut prose, trimmed);
            }
        }
    }

    end_prose(&mut blocks, &mut prose);
    end_list(&mut blocks, &mut list);
    blocks.join("\n\n")
}

/// The direction a parameter command carries, and the rest — `@param[in] p` is `(Some("in"), "p")`.
fn direction_of(rest: &str) -> (Option<String>, &str) {
    let Some(after) = rest.strip_prefix('[') else {
        return (None, rest);
    };
    let Some(end) = after.find(']') else {
        return (None, rest);
    };
    (
        Some(after[..end].trim().to_string()),
        after[end + 1..].trim_start(),
    )
}

/// The Doxygen command a comment line starts with, and the text after it — `None` for prose.
///
/// Both spellings are Doxygen's: `@brief` and `\brief` are one command, and a real codebase uses both.
fn command_of(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('@').or_else(|| line.strip_prefix('\\'))?;
    let end = rest
        .find(|character: char| !character.is_ascii_alphanumeric())
        .unwrap_or(rest.len());
    let command = &rest[..end];
    (!command.is_empty()).then(|| (command, rest[end..].trim_start()))
}

/// A command's own spelling, capitalised — the label for one this crate has no translation for.
fn capitalize(word: &str) -> String {
    let mut characters = word.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}
/// What the expression at the cursor is, when no declaration is at it.
///
/// The fallback for `this`, `*p`, `f(1)`, `w.size`: positions where there is no *name* to look up and the analysis
/// still knows the type. `None` when it does not — which is the ordinary answer, because most positions are in no
/// expression at all and the ones that are may be a call through a function pointer or a template parameter that
/// nothing here instantiates.
fn expression_markdown(
    session: &Session<DiskFiles>,
    view: &FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
) -> Option<String> {
    let Known::Yes(found) = session.type_at(view, offset) else {
        return None;
    };

    // `this` is the one expression whose type is not what the word means: the analysis answers with the enclosing
    // *class*, and calling that the type of `this` would be wrong about a pointer.
    if found.expression == "this" {
        return Some(format!(
            "`this` — a pointer to the enclosing class `{}`",
            found.type_of
        ));
    }

    Some(format!(
        "`{}` — has type `{}`",
        inline(&found.expression),
        found.type_of
    ))
}

/// An expression's text for a line of markdown: one line, and short enough to read.
fn inline(text: &str) -> String {
    let mut collapsed = String::new();
    let mut after_space = false;

    for character in text.chars() {
        if character.is_whitespace() {
            after_space = true;
            continue;
        }

        if after_space && !collapsed.is_empty() {
            collapsed.push(' ');
        }
        after_space = false;
        collapsed.push(character);
    }

    if collapsed.chars().count() <= MAX_INLINE_CHARS {
        return collapsed;
    }

    let cut: String = collapsed.chars().take(MAX_INLINE_CHARS - 1).collect();
    format!("{cut}…")
}

/// What kind of declaration this is, as a sentence fragment.
/// **The value of an enumerator, counted from the text** — `None` when the cursor is not on one.
///
/// An enumerator's value is not stored in its fact and should not be: it is a function of the enumerators written
/// before it in the same body (an explicit `= 1` sets it, anything after it counts on), and the fact is a fact about
/// one name. The file's own tree is where the sequence is, so this walks it:
///
/// ```text
/// enum class E { A = 1, B, C = 0x10, D };    A 1, B 2, C 16, D 17
/// ```
///
/// `None` when an explicit initialiser is not an integer literal — `1 << 4` or `Other` are values this layer does not
/// evaluate, and a card that guessed at them would be worse than one that says the number it knows.
fn enumerator_value(view: &FileView, offset: usize) -> Option<i64> {
    let token = cpp_parser::token_at(&view.root, offset)?;
    let declaration = token.parent_ancestors().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::EnumeratorDecl
    })?;
    let body = declaration.parent()?;

    let mut value: i64 = -1;
    for enumerator in body.children() {
        if cpp_parser::CppSyntaxKind::from(enumerator.kind())
            != cpp_parser::CppSyntaxKind::EnumeratorDecl
        {
            continue;
        }
        // **The significant tokens of the whole enumerator, nested ones included**: `A = 1 << 4` holds three of them
        // after the name, and an initialiser that is not exactly one integer literal is not a value this layer
        // evaluates — `None` rather than the first number it happened to see.
        let mut tokens: Vec<cpp_parser::CppSyntaxToken> = enumerator
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| !cpp_parser::is_trivia(cpp_parser::CppTokenKind::from(token.kind())))
            .collect();
        tokens.retain(|token| !token.text().trim().is_empty());
        match tokens.get(1) {
            Some(token)
                if cpp_parser::CppTokenKind::from(token.kind())
                    == cpp_parser::CppTokenKind::Assign =>
            {
                let literal = tokens.get(2)?;
                if tokens.len() != 3
                    || cpp_parser::CppTokenKind::from(literal.kind())
                        != cpp_parser::CppTokenKind::IntegerLiteral
                {
                    return None;
                }
                value = an_integer(literal.text())?;
            }
            _ => value += 1,
        }
        if enumerator.text_range() == declaration.text_range() {
            return Some(value);
        }
    }

    None
}

/// An integer literal's value — decimal, hexadecimal, binary, with a `u`/`l` suffix if one is written.
fn an_integer(text: &str) -> Option<i64> {
    let text = text.trim_end_matches(|character: char| {
        character.is_ascii_alphabetic() && !matches!(character, 'x' | 'X' | 'b' | 'B')
    });
    let text = text.replace('\'', "");
    let (digits, radix) = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(digits) => (digits, 16),
        None => match text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
            Some(digits) => (digits, 2),
            None => (text.as_str(), 10),
        },
    };
    i64::from_str_radix(digits, radix).ok()
}

fn kind_words(fact: &DeclFact) -> String {
    let kind = match fact.kind {
        DeclKind::Type => "a type",
        DeclKind::Function => "a function",
        DeclKind::Variable => "a variable",
        DeclKind::Namespace => "a namespace",
        DeclKind::MacroLike => "a macro-like declaration",
        DeclKind::Other => "a declaration",
    };

    let mut words = kind.to_string();

    // **The template parameters, which the head cannot show.** `DeclFact::range` is the *declarator*, so a class
    // template's `template <class T>` is not in it: the head of `Box` comes out as `class Box`, and a reader of
    // `Box<int> b;` has no way to see that the class takes anything at all. The names are in the fact, and they are
    // shown as the fact gives them.
    //
    // The one word *not* shown is `class` or `typename`: the fact records the parameter names and not their keywords,
    // and printing `class` for a `typename` would be a guess dressed as a fact. An **unnamed** parameter is an empty
    // entry (its slot is kept so arguments pair with names by position — see `template_parameters_in`), and a name
    // the reader cannot write is not shown at all.
    let named: Vec<&str> = fact
        .parameters
        .iter()
        .filter(|name| !name.is_empty())
        .map(String::as_str)
        .collect();
    // **Only when the declaration is the template.** A *member* of a class template carries the class's parameters in
    // this field — the member-list builder fills them so a use's arguments can be paired — so printing them as the
    // member's own said "`std::pair::second` — a variable template over `_T1`, `_T2`" (reported, with a screenshot): a
    // member of a template is not a template, and the field cannot tell the two apart. A fact with a scope therefore
    // stops claiming and lets the `in `` ` clause below say where it is.
    if !named.is_empty() && fact.scope.is_none() {
        words.push_str(&format!(" template over `{}`", named.join("`, `")));
    }

    // **The prose's types go through the same renderer as the card's** — `docs/hover-render.md`'s contract, and the
    // reason it is not a style choice: a fact records the spelling the *file* wrote (`std::vector<std::string> &`, with
    // the space the declarator needed), and the prose printed it as-is while the card below it printed
    // `std::vector<std::string>&`. Two spellings of one type in one popup is a reader asking which is the type.
    //
    // `a_type` parses the spelling and prints it from the model, so the qualifiers, the references and the arguments
    // come out the way every other type in the card does. `const char * const` → `const char*` is the same normalization
    // the card has had since the renderer landed.
    match (&fact.type_of, &fact.returns) {
        (Some(written), _) => words.push_str(&format!(
            " of type `{}`",
            cpp_code_analysis::sema::render::a_type(written)
        )),
        (None, Some(written)) => words.push_str(&format!(
            " returning `{}`",
            cpp_code_analysis::sema::render::a_type(written)
        )),
        (None, None) => {}
    }

    if !fact.bases.is_empty() {
        words.push_str(&format!(" derived from `{}`", fact.bases.join("`, `")));
    }

    if fact.local {
        words.push_str(", declared in a function body");
    }

    if let Some(scope) = &fact.scope {
        words.push_str(&format!(", in `{scope}`"));
    }

    words
}

/// **The text a fact's offsets are offsets into** — which is not always the file's own text.
///
/// A fact about a declaration in **another file** carries that file's offsets: the index maps a cooked reading back
/// into the file it stands in before it stores anything, so the VFS copy is the right ruler. A fact about a
/// declaration in **the file being viewed** does not — it comes from the view's own tree, whose offsets are the
/// **rendering's**, because the rendering is what the parser was handed.
///
/// So the rendering's own text answers for the viewed file, and that is not a workaround: it is what the declaration
/// looked like to the reader that produced the fact.
fn the_text_a_facts_offsets_are_in<'a>(
    session: &'a Session<DiskFiles>,
    view: Option<&'a FileView>,
    file: &std::path::Path,
) -> Option<&'a str> {
    match view {
        Some(view) if is_the_viewed_file(view, file) => Some(&view.source),
        _ => session.files().held(file).map(|held| &*held.text),
    }
}

/// Is this the file the view is of?
///
/// **Compared through the crate's one normaliser, and on both sides.** The two spellings reach here from different
/// journeys — one from the session's own file list, one out of an index that stored it — and on Windows they differ
/// in the drive letter's case and the separator: `d:/…/main.cpp` against `D:\…\main.cpp`. Comparing a normalised
/// path against a raw one is never equal, and the branch that depends on this then answers as if the fact were about
/// some other file. Measured: a popup read `#include <vector>` where the declaration was.
fn is_the_viewed_file(view: &FileView, file: &std::path::Path) -> bool {
    cpp_code_analysis::normalize_path(&view.path, cfg!(windows))
        == cpp_code_analysis::normalize_path(file, cfg!(windows))
}

/// The `#define` line a macro fact points at, body included.
fn definition_line(
    session: &Session<DiskFiles>,
    view: &FileView,
    file: &std::path::Path,
    name_offset: usize,
    body_range: Option<cpp_parser::SourceRange>,
) -> RenderedText {
    // **The text the offsets are into**, not simply the file's — see [`the_text_a_facts_offsets_are_in`]: a macro
    // fact about the viewed file carries the rendering's offsets, and its `#define` line is the rendering's line.
    // **A macro fact's offsets are the FILE's, so this is the one place that must slice the file's own text.**
    //
    // The declaration path wants the rendering (a `DeclFact` about the viewed file is translated into it), but a
    // macro has no rendering: `#define STD std::` exists only in the file, and the cook is what removes it. Slicing
    // the rendering with the file's offsets landed in the middle of `main` — measured, hovering `STD` answered with a
    // screenful of the function body instead of the directive.
    let text = if is_the_viewed_file(view, file) {
        view.written_text().unwrap_or(&view.source)
    } else {
        match the_text_a_facts_offsets_are_in(session, Some(view), file) {
            Some(text) => text,
            None => {
                return RenderedText {
                    text: format!(
                        "<the macro is defined in an unreadable file: {}>",
                        file.display()
                    ),
                };
            }
        }
    };
    let name_offset = name_offset.min(text.len());

    // The name's range is the name; the body's range (when there is one) is everything after the parameters, and
    // it is stored precisely so that a consumer does not have to search for the end of the directive.
    let end = body_range.map_or(name_offset, |body| body.end_offset());
    let line_end = text[name_offset..]
        .find('\n')
        .map_or(text.len(), |offset| name_offset + offset);
    let end = end.max(line_end.min(text.len()));

    // The directive's own `#define` keyword is before the name, and the fact does not record it: taking the line's
    // start is honest — it is the directive — where inventing the keyword would not be.
    let line_start = text[..name_offset]
        .rfind('\n')
        .map_or(0, |offset| offset + 1);

    RenderedText {
        text: text[line_start..end].trim_end().to_string(),
    }
}

/// `file.h:12` — where something is, as a client can follow.
fn where_clause(
    session: &Session<DiskFiles>,
    view: Option<&FileView>,
    file: &std::path::Path,
    offset: usize,
) -> String {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.display().to_string());

    // **The file's ruler, after the view's mapping.** A declaration's offsets are the **file's** — the summary is
    // mapped into the file, and a probe on `test_cpp/src/templates.cpp` shows `name_range` slicing the file's own text.
    // What a view can add is a *mapping*: a rendering's offsets are not the file's, so they go through
    // `file_offset_of` first (the identity for a view of the written text). The line index must then be the **declaring
    // file's**: a plain view of the written text carries `written_text()` of `None` and no line index of its own, and
    // measuring a file offset against that answered `templates.cpp:6:12` for `Pair::sum`, which is written on line 18.
    let in_the_file = view
        .and_then(|view| {
            view.file_offset_of(cpp_code_analysis::file::coordinates::ReadingOffset::new(
                offset,
            ))
        })
        .map(|at| at.get())
        .unwrap_or(offset);
    let asked = session
        .files()
        .held(file)
        .and_then(|declaring| crate::util::position_in_file(declaring, in_the_file));

    match asked {
        Some(position) => format!("`{name}:{}:{}`", position.line + 1, position.character + 1),
        None => format!("`{name}`"),
    }
}

fn code_block(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    // A fence longer than any run inside the declaration: a C++ file can hold a raw string containing three
    // backticks, and a hover that ended early would show the rest of the answer as prose.
    let fence = "`".repeat(3.max(longest_backtick_run(text) + 1));
    format!("{fence}cpp\n{text}\n{fence}\n")
}

/// The longest run of backticks in `text`, which is what a fence has to be longer than.
fn longest_backtick_run(text: &str) -> usize {
    text.split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
}

struct RenderedText {
    text: String,
}

pub struct HoverCapabilities;

impl RegisterCapabilities for HoverCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.hover_provider = Some(HoverProviderCapability::Simple(true));
    }
}

/// **What a position-dependent standard macro means**, for the card that cannot show a value.
///
/// `__LINE__` expands to the line it stands on, so there is nothing to print beside it the way `__cplusplus` prints
/// `202302L` — but "predefined by the compiler" with no explanation is a card that says nothing, and no card at all
/// is what a reader reported for `__LINE__`. The five names are [cpp.predefined]'s, and the wording is the standard's.
fn a_standard_macro_meaning(word: &str) -> Option<&'static str> {
    Some(match word {
        "__LINE__" => "the line number of the line it stands on",
        "__FILE__" => "the name of the file it stands in",
        "__DATE__" => "the date this translation unit was compiled",
        "__TIME__" => "the time this translation unit was compiled",
        "__COUNTER__" => "a number one higher each time it is written",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    /// **A position-dependent standard macro says what it is**, because it has no value to show.
    ///
    /// `__cplusplus` had a card ("predefined by the compiler … as `202302L`") and `__LINE__` had none: the valued
    /// table is the standard's own, and these five are not in it. Measured with `inspect hover` on
    /// `int f() { return __LINE__; }`.
    #[test]
    fn a_position_dependent_standard_macro_explains_itself() {
        for (word, expected) in [
            ("__LINE__", "the line number of the line it stands on"),
            ("__FILE__", "the name of the file it stands in"),
            ("__DATE__", "the date this translation unit was compiled"),
            ("__TIME__", "the time this translation unit was compiled"),
            ("__COUNTER__", "a number one higher each time it is written"),
        ] {
            assert_eq!(
                a_standard_macro_meaning(word),
                Some(expected),
                "`{word}` is explained rather than left as a bare `predefined`"
            );
        }
        assert_eq!(
            a_standard_macro_meaning("__cplusplus"),
            None,
            "the valued one is not here"
        );
        assert_eq!(
            a_standard_macro_meaning("Widget"),
            None,
            "and neither is an ordinary name"
        );
    }

    /// **An enumerator's value, counted the way a compiler counts it.**
    ///
    /// Reported from an editor: hovering `OPR_MUL` in an `enum class BinOpr { OPR_ADD = 1, OPR_MUL, … }` showed
    /// nothing, and the report asked for the number beside it. The number is not in the fact — it is a function of the
    /// enumerators written before it — so this reads the sequence out of the file's own tree.
    #[test]
    fn an_enumerators_value_is_counted_from_the_ones_before_it() {
        let text = "enum class Kind { A = 1, B, C = 0x10, D, E = 100, F };\n";
        let tree = cpp_parser::CppParser::parse(text, cpp_parser::ParserConfig::default());
        let files = cpp_code_analysis::MemoryFiles::new().with_file("/p/a.cpp", text);
        let mut session = cpp_code_analysis::Session::with_config(
            "/p",
            cpp_code_analysis::SessionFiles::new(cpp_code_analysis::OpenDocuments::new(), files),
            cpp_code_analysis::WatchFilter::new("/p"),
            cpp_code_analysis::CompilerConfig::default(),
        );
        session.load("/p/a.cpp");
        let view = session
            .view_of_the_file(std::path::Path::new("/p/a.cpp"))
            .expect("a view");
        let _ = tree;

        for (name, expected) in [
            ("A", 1),
            ("B", 2),
            ("C", 16),
            ("D", 17),
            ("E", 100),
            ("F", 101),
        ] {
            let offset = text.find(&format!(" {name}")).expect("the fixture") + 1;
            assert_eq!(
                enumerator_value(&view, offset),
                Some(expected),
                "`{name}` counts from the enumerators before it"
            );
        }

        // **A non-literal initialiser is not guessed at**: `1 << 4` is a value this layer does not evaluate, and the
        // answer is `None` rather than a number nobody computed.
        let shifted = "enum class S { A = 1 << 4, B };\n";
        let files = cpp_code_analysis::MemoryFiles::new().with_file("/p/s.cpp", shifted);
        let mut session = cpp_code_analysis::Session::with_config(
            "/p",
            cpp_code_analysis::SessionFiles::new(cpp_code_analysis::OpenDocuments::new(), files),
            cpp_code_analysis::WatchFilter::new("/p"),
            cpp_code_analysis::CompilerConfig::default(),
        );
        session.load("/p/s.cpp");
        let view = session
            .view_of_the_file(std::path::Path::new("/p/s.cpp"))
            .expect("a view");
        let offset = shifted.find(" A").expect("the fixture") + 1;
        assert_eq!(enumerator_value(&view, offset), None);
    }

    /// **A return type read as a macro's name is corrected from the names that are not macros.**
    ///
    /// The shape is `wincon.h`'s: `WINBASEAPI WINBOOL WINAPI SetConsoleOutputCP(UINT)`, where the two macro names
    /// are defined by a *different* header (the includer's), so the file's own text cannot expand them. The grammar
    /// takes the last name of the specifier sequence — the rule that fixes `_NODISCARD _CONSTEXPR20 reference
    /// back()` — which is `WINAPI`, and the card said "a function returning `WINAPI`" (reported from an editor).
    /// The index knows both names are macros, so the type left after dropping them is `WINBOOL`.
    #[test]
    fn a_return_type_that_is_a_macros_name_is_read_without_the_macros() {
        let definition = "#define WINBASEAPI __declspec(dllimport)\n#define WINAPI __attribute__((__stdcall__))\n";
        let declaration = "#include \"defs.h\"\nWINBASEAPI WINBOOL WINAPI SetConsoleOutputCP(UINT wCodePageID);\n";
        let files = cpp_code_analysis::MemoryFiles::new()
            .with_file("/p/defs.h", definition)
            .with_file("/p/wincon.h", declaration)
            .with_file("/p/main.cpp", "#include \"wincon.h\"\n");
        let mut session = cpp_code_analysis::Session::with_config(
            "/p",
            cpp_code_analysis::SessionFiles::new(cpp_code_analysis::OpenDocuments::new(), files),
            cpp_code_analysis::WatchFilter::new("/p"),
            cpp_code_analysis::CompilerConfig::default(),
        );
        session.add_project_files([std::path::PathBuf::from("/p/main.cpp")]);
        session.load("/p/wincon.h");
        session.index_everything();
        while session.pending_work() > 0 {
            session.advance(64);
        }

        let fact = session
            .index()
            .summary(std::path::Path::new("/p/wincon.h"))
            .and_then(|summary| {
                summary
                    .declarations
                    .iter()
                    .find(|declaration| &*declaration.name == "SetConsoleOutputCP")
            })
            .expect("the declaration")
            .clone();
        let corrected = the_type_the_macros_leave(
            &session,
            std::path::Path::new("/p/wincon.h"),
            &fact,
            fact.returns.as_deref().unwrap_or_default(),
        );
        assert_eq!(
            corrected.as_deref(),
            Some("WINBOOL"),
            "the two macro names drop out and the type that is left is `WINBOOL` (the fact said {:?})",
            fact.returns
        );

        // **And a name that is not a macro is left alone.** `std::size_t` has no definers, so the card keeps the
        // fact's spelling rather than reconstructing anything.
        let other = DeclFact {
            enumerator: false,
            returns: Some("std::size_t".to_string()),
            ..fact.clone()
        };
        assert_eq!(
            the_type_the_macros_leave(
                &session,
                std::path::Path::new("/p/wincon.h"),
                &other,
                "std::size_t"
            ),
            None
        );
    }
    /// **中文表加载成功，而且和英文表用的是同一组 key。**
    ///
    /// 一张译文表最容易出的错不是翻译错，而是 key 拼错或漏了 —— 那样 rust-i18n 会安静地回落成英文，看起来像
    /// "翻译没生效"。所以这条测试直接比较两种语言下同一个 key 的结果：不同、且都不等于 key 本身。
    #[test]
    fn the_chinese_table_translates_the_same_keys() {
        let english = (t!("keyword.constexpr"), t!("command.param"));
        rust_i18n::set_locale("zh-CN");
        let chinese = (t!("keyword.constexpr"), t!("command.param"));
        rust_i18n::set_locale("en");

        assert_ne!(
            english.0, chinese.0,
            "`keyword.constexpr` has a translation"
        );
        assert_ne!(english.1, chinese.1, "`command.param` has a translation");
        assert!(chinese.0.starts_with("**`constexpr`**"), "{chinese:?}");
        assert!(chinese.1.contains("@param"), "{chinese:?}");
        println!("en: {english:?}\nzh: {chinese:?}");
    }

    /// **The two vocabularies answer, and the file they live in is the one that was embedded.**
    ///
    /// `rust-i18n` answers a missing key with the key itself, so "the popup says `keyword.constexpr`" and "the popup
    /// is empty" are the two failure modes this test is written to catch. Both tables are read from
    /// `locales/en.yml` at build time — the point of the whole arrangement is that the text is *data*, so a test
    /// that only called `t!` directly would not prove the file was found.
    #[test]
    fn a_keyword_and_a_doc_command_explain_themselves() {
        let tree = cpp_parser::CppParser::parse(
            "constexpr int f(int value);\n",
            cpp_parser::ParserConfig::default(),
        );
        let root = tree.get_red_root();
        let token = root
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| token.text() == "constexpr")
            .expect("the fixture writes it");

        let keyword =
            vocabulary::keyword_hover(&token).expect("`constexpr` is in the keyword table");
        assert!(
            keyword.contains("constexpr"),
            "the popup names the word it is about: {keyword}"
        );
        assert!(
            !keyword.starts_with("keyword."),
            "and it is text, not the key that was not found: {keyword}"
        );

        let command = vocabulary::command_hover("param").expect("`@param` is in the command table");
        assert!(command.contains("@param"), "{command}");

        // **A command is a word in a comment**, and only in a *documentation* comment.
        let text = "/// @param value the value\n// @param not documentation\nint f(int value);\n";
        let at = text.find("@param").expect("the fixture") + 3;
        assert_eq!(
            doc_command_at(text, at).as_deref(),
            Some("param"),
            "a cursor inside `@param` in a `///` line"
        );
        let end = text.find("@param").expect("the fixture") + "@param".len();
        assert_eq!(
            doc_command_at(text, end).as_deref(),
            Some("param"),
            "and a cursor just past it, which is where a keystroke leaves it"
        );

        // `rfind`, because `//` occurs inside the `///` line as well — a `find` would land there and pass for the
        // wrong reason.
        let plain = text.rfind("// @param").expect("the fixture") + "// @param".len();
        assert_eq!(
            doc_command_at(text, plain),
            None,
            "a `//` comment is prose with an `@` in it"
        );
    }
    #[test]
    fn a_fence_cannot_be_closed_by_the_declaration_it_quotes() {
        // A declaration holding a raw string with three backticks in it: the block has to be fenced with more.
        let block = code_block("const char* s = R\"(```)\";");
        assert!(block.starts_with("````cpp\n"));
        assert!(block.ends_with("````\n"));
    }

    /// An expression quoted in a line of markdown: one line, and bounded.
    ///
    /// Both halves matter in a popup: a newline inside a single pair of backticks ends the markdown *span*, so the
    /// rest of the answer would be rendered as prose, and an unbounded quote would push the declaration itself out
    /// of the popup the reader opened.
    #[test]
    fn an_expression_is_collapsed_to_one_bounded_line() {
        assert_eq!(inline("scale(1,\n     2)"), "scale(1, 2)");
        assert_eq!(inline("  spaced   out  "), "spaced out");

        let long = "a".repeat(MAX_INLINE_CHARS + 20);
        let shown = inline(&long);
        assert_eq!(shown.chars().count(), MAX_INLINE_CHARS);
        assert!(shown.ends_with('…'), "the cut is visible: {shown}");

        // Exactly at the bound nothing is cut: an elision of nothing would be a lie.
        let exact = "b".repeat(MAX_INLINE_CHARS);
        assert_eq!(inline(&exact), exact);
    }

    #[test]
    fn nothing_is_shown_for_an_empty_declaration() {
        assert_eq!(code_block(""), "");
    }
}
/// **The documentation above a declaration an index fact names** — with the coordinate seam crossed.
///
/// Two coordinate systems meet here and only one of them is obvious. A `DeclFact` comes from the index, and the index
/// is built from a file's **cooked reading** — the program a compiler sees, with comments removed. So
/// `fact.range.start_offset` is an offset in the *rendering*, while the comment it documents is in the text the
/// **reader wrote**, and the two differ by exactly what the cook deleted. `Session::documentation` parses the written
/// text and looks the offset up there, so it has to be given a file offset: the signature path crosses that seam
/// (see `Session::signatures_at`, which says the same thing about `declared_at`), and this path did not — the
/// documented declaration's hover carried no comment at all, on a file whose comment sits right above it.
fn documentation_of_a_fact(
    session: &Session<DiskFiles>,
    view: &FileView,
    file: &std::path::Path,
    offset: usize,
) -> Option<String> {
    // **The offset is the file's**, because the view the caller holds is the file's own text — `hover` answers from
    // `view_of_the_file` now (see the note at its call). This used to ask `session.view(file)` for the rendering and map
    // the offset through `file_offset_of`, and that was the *only* place a hover queued a cook: on a file whose
    // rendering had been evicted the documentation disappeared, and the next hover paid for the render.
    documentation_markdown(session, view, file, offset)
}

/// **The comment a project writes above a function, rendered as the popup shows it.**
///
/// The fixture is the shape from the screenshot that started this: a `@brief` and two `@param`s, one of them
/// running onto a second line as real code does. Before this, markdown joined every line and the popup read as one
/// paragraph with the tags still in it.
#[test]
fn a_documented_function_is_rendered_as_blocks() {
    let rendered = render_documentation(
        "@brief Prints a message with a preceding newline.\n\
             @param message The message to print after a newline.\n\
             @param count The number of times to print the message\n\
             after a newline.\n\
             @return whether anything was written\n\
             @note the stream is flushed",
    );

    assert_eq!(
        rendered,
        "Prints a message with a preceding newline.\n\n\
             - **`message`** — The message to print after a newline.\n\
             - **`count`** — The number of times to print the message after a newline.\n\
             - **Returns** — whether anything was written\n\
             - **Note** — the stream is flushed",
        "brief is prose, the commands are one list, and a continuation line belongs to its item"
    );
}

/// **A command this crate does not know is kept as written**, and `\brief` is the same command as `@brief`.
///
/// The alternative — dropping what a table does not name — silently removes an author's structure, which is a
/// worse popup than the run-on paragraph this renderer replaced.
#[test]
fn an_unknown_command_stays_in_the_text() {
    let rendered = render_documentation(
        "\\brief One line.\n@precondition the file is open\n@param[in] p a pointer",
    );
    assert!(rendered.contains("One line."), "{rendered}");
    assert!(
        !rendered.contains("@brief") && !rendered.contains("\\brief"),
        "a command this table knows is not shown as a tag: {rendered}"
    );
    assert!(
        rendered.contains("@precondition the file is open"),
        "an unknown one is kept, tag and text together: {rendered}"
    );
    assert!(
        rendered.contains("- **`p`** (in) — a pointer"),
        "and a parameter's direction is kept beside its name: {rendered}"
    );
}
