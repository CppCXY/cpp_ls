//! 找引用：一个宏的名字在项目里出现在哪些位置。
//!
//! `macro_across_files` 回答"游标下的名字定义在哪"，这一层回答反过来的那个问题——**哪些位置上的这个名字
//! 指的是它**。重命名需要的正是这个列表，而它是第一个需要**读文件正文**的查询。
//!
//! # 为什么它需要 provider，而别的查询不需要
//!
//! 摘要里存的是**声明与指令**，没有标识符位置：`DeclFact` 说"某处声明了 `Widget`"，却不说"第 812 个字节
//! 处写着 `Widget`"。所以"名字在哪儿出现过"这件事，索引答不了——它只能从文件正文里读出来。
//!
//! 这就带出这一层真正的设计问题：**一个项目里找一次引用要读多少东西。** 答案是一段阶梯，每一级都比上一级
//! 便宜，而且每一级都是**可靠**的（不是启发式）：
//!
//! ```text
//! 1. 候选集    只有能看见某个定义的文件才可能是用户：定义所在文件 + 它们的传递反向 include 闭包
//! 2. 文本预筛  名字不是这个文件正文的子串，就一个标识符也不可能叫这个名字 → 连词法都不用做
//! 3. 词法      CppLexer 一遍：注释和字符串是**一个 token**，所以它们里的名字根本不会出现成 Identifier
//! 4. 精确判定  每个命中问一次**宏环境**（每个文件算一次）：它是不是宏、是哪一条 #define
//! ```
//!
//! 第 3 级是这一层能成立的关键：**注释与字符串不需要解析就能排除**，而"用文本子串找引用"最大的假阳性
//! 正是它们。第 4 级是**唯一**不靠形状而是靠语义的一步，也正因为有它，第 2、3 级可以宽松。
//!
//! 要不要在摘要里存一张"标识符位置表"（那样就不必读正文）是一个**量出来的问题**，不是设计偏好：
//! `examples/find_references.rs` 把上面四级各自的代价打出来，`docs/index-design.md` 记着结论。
//!
//! # 它刻意不做的事
//!
//! * **不找普通名字的引用**。一个宏的名字是**文本**层面的东西：预处理器只有一张名字表，出现即替换。普通
//!   名字（`Widget`、`w`）的引用要问"这个偏移处的这个名字解析到那条声明吗"，那需要每个候选文件的**作用域**，
//!   也就是每个候选文件一次解析——比这一层贵一个数量级。位置表那件事正是为它准备的。
//! * **不判断引用所在区域是否被编译**。一次引用是"这个名字在这里指的是这个宏"，与那段代码在 `#if` 的哪一支
//!   无关：重命名两边都要改。
//! * **不猜**。命中只有三种归宿：确定是它（`Use`）、确定不是（计数进 `rejected`）、可能但说不准
//!   （`Uncertain` 带 `UnknownReason`）。

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

use cpp_parser::{CppLexer, CppTokenKind, LexerConfig, SourceRange};

use crate::include::paths::{FileProvider, normalize_path};
use crate::index::ProjectIndex;
use crate::summary::MacroKind;
use crate::symbol::{Known, UnknownReason};

/// How many candidate files one find-references will read.
///
/// The same shape as [`crate::IncludeBudget`] and for the same reason: a bound that is **reported** rather than
/// enforced quietly. A standard-library closure is a few hundred files and a project index is rarely past a few
/// thousand, so the default is far past anything measured — and a caller that hits it is told through
/// [`MacroReferences::not_looked_at`], because "there are no more references" and "I stopped looking" are two
/// different answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceBudget {
    pub max_files: usize,
}

impl Default for ReferenceBudget {
    fn default() -> Self {
        ReferenceBudget { max_files: 4096 }
    }
}

/// Every place a macro's name is written, and what each one turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroReferences {
    /// The name asked about.
    pub name: String,
    /// The files with at least one reference, in path order — so that two runs over one project answer the same
    /// thing in the same order.
    pub files: Vec<FileReferences>,
    /// Candidate files that were read and lexed.
    pub looked_at: usize,
    /// Candidate files whose text does not contain the name at all, which is the cheap filter's work: reading
    /// them is one `read` and no lexing.
    pub without_the_name: usize,
    /// Identifier occurrences of the name that are **not** this macro: a variable of the same name, a function, or
    /// the name after an `#undef`. Counted rather than dropped silently, because "we looked and rejected these" is
    /// a different statement from "there was nothing to look at".
    pub rejected: usize,
    /// Candidate files the budget stopped before.
    pub not_looked_at: usize,
    /// Candidate files that could not be read: deleted between the index and the query.
    pub unreadable: Vec<PathBuf>,
}

/// The references written in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReferences {
    pub file: PathBuf,
    /// In source order.
    pub references: Vec<Reference>,
}

/// One place the name is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub range: SourceRange,
    pub kind: ReferenceKind,
}

/// What a place turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceKind {
    /// The `#define` itself. Renaming edits it.
    Definition,
    /// An `#undef`: the name's history, and part of what a rename has to keep in step.
    Undefinition,
    /// An identifier the preprocessor replaces. `resolved_to` is the file whose `#define` is in force **at that
    /// offset**, which is not always the one the query started from: the same name can be defined twice, in two
    /// branches, and a rename edits both because the name is one name.
    Use { resolved_to: PathBuf },
    /// A place the name is a macro's, but not certainly *this* one: it is reached through a conditional
    /// `#include`, or through an `#if`. A caller that shows these must label them; a caller that edits them is
    /// guessing, which is why [`MacroReferences::rename`] leaves them alone.
    Uncertain(UnknownReason),
}

/// One edit a rename would make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub file: PathBuf,
    pub range: SourceRange,
    /// What replaces the range — the new name.
    pub replacement: String,
}

impl MacroReferences {
    /// How many references were found, across every file.
    pub fn total(&self) -> usize {
        self.files.iter().map(|file| file.references.len()).sum()
    }

    /// How many files the candidate rule named.
    ///
    /// An accessor rather than arithmetic at each call site, because the three fields it adds up mean three
    /// different things and a caller that forgets one of them gets a number that looks plausible: the first
    /// version of this module's own probe computed "candidates" as `looked_at + without_the_name` — which counts the
    /// skipped files twice and leaves the budget's files out.
    pub fn candidates(&self) -> usize {
        self.looked_at + self.not_looked_at + self.unreadable.len()
    }

    /// How many candidate files were **lexed**: the ones that got past the text filter.
    pub fn lexed(&self) -> usize {
        self.looked_at - self.without_the_name
    }

    /// How many of them a rename would **not** touch, because they might be something else.
    pub fn uncertain(&self) -> usize {
        self.files
            .iter()
            .flat_map(|file| &file.references)
            .filter(|reference| matches!(reference.kind, ReferenceKind::Uncertain(_)))
            .count()
    }

    /// The edits that rename this macro to `to`.
    ///
    /// The definition, the `#undef`s and the certain uses — and **not** the uncertain ones, which is a decision
    /// rather than an omission: an uncertain reference is a place where the name might be a variable, or might be
    /// another macro's, and an edit there is a change to code the user did not ask about. A caller that wants to
    /// show them can, through [`MacroReferences::files`]; a caller that renames cannot do it safely.
    ///
    /// A rename that would edit nothing is not an error: a macro only its own `#define` mentions is a macro with
    /// one reference, and that is the answer.
    pub fn rename(&self, to: &str) -> Vec<Rename> {
        let mut edits = Vec::new();

        for file in &self.files {
            for reference in &file.references {
                let certain = matches!(
                    reference.kind,
                    ReferenceKind::Definition | ReferenceKind::Undefinition | ReferenceKind::Use { .. }
                );

                if certain {
                    edits.push(Rename {
                        file: file.file.clone(),
                        range: reference.range,
                        replacement: to.to_string(),
                    });
                }
            }
        }

        edits
    }
}

/// Every place `name` is written as a macro, across the files the index holds.
///
/// # The candidate rule, and what it is worth
///
/// A file can only use a macro it can **see**: the name has to be `#define`d before the use, either in the file or
/// in something it includes. So the candidates are the files that define the name and everything that
/// **transitively includes** one of them. That is a sound narrowing rather than a guess — a file outside it has a
/// use of the name only if the name reaches it some other way, and the include graph is the only way there is.
///
/// What it costs is stated where it is paid: a file that writes an `#include` that did not resolve is not an
/// includer of anything, so it is not a candidate — correctly, because a compiler reading it would not see the
/// macro either.
///
/// # What it answers
///
/// * `Yes(references)` — the name is a macro somewhere in the index. The list may be short: see below.
/// * `Unknown(NotDeclaredHere)` — no file in the index defines the name. **Not** "no such macro": the index holds
///   the files it has been asked about, and under lazy indexing that is the difference between "not here" and
///   "not read yet" ([`crate::Session::pending`] is how a caller tells them apart).
pub fn macro_references<F: FileProvider>(
    index: &ProjectIndex,
    files: &F,
    name: &str,
    budget: ReferenceBudget,
) -> Known<MacroReferences> {
    let definers = files_defining(index, name);

    if definers.is_empty() {
        return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(name)));
    }

    let mut answer = MacroReferences {
        name: name.to_string(),
        files: Vec::new(),
        looked_at: 0,
        without_the_name: 0,
        rejected: 0,
        not_looked_at: 0,
        unreadable: Vec::new(),
    };

    for path in candidates(index, &definers, budget, &mut answer) {
        // The facts first, and they do not depend on the text: a `#define` is a reference to its own name by
        // definition, and a file that writes one necessarily contains the name — but reading the facts first keeps
        // the two halves of the answer independent of the order they are discovered in.
        let mut references: Vec<Reference> = index
            .summary(&path)
            .map(|summary| {
                summary
                    .macros
                    .iter()
                    .filter(|fact| fact.name == name)
                    .map(|fact| Reference {
                        range: fact.range,
                        kind: match fact.kind {
                            MacroKind::Definition => ReferenceKind::Definition,
                            MacroKind::Undefinition => ReferenceKind::Undefinition,
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();

        let Some(text) = files.read(&path) else {
            // Readable when it was indexed, gone now. One deleted file does not stop the answer; the caller is told.
            answer.unreadable.push(path);
            continue;
        };

        answer.looked_at += 1;

        // The cheap filter, and it is exact rather than heuristic: an identifier token's text is a slice of the
        // file's text, so a file that does not contain the name as a substring cannot contain it as an identifier.
        if !text.contains(name) {
            answer.without_the_name += 1;
        } else {
            // A `#define`'s fact carries the **name's** range, so the identifier token for it is the same range and
            // has to be skipped — otherwise every definition would be reported twice.
            let claimed: HashSet<(usize, usize)> = references
                .iter()
                .filter(|reference| reference.kind == ReferenceKind::Definition)
                .map(|reference| {
                    (
                        reference.range.start_offset,
                        reference.range.end_offset(),
                    )
                })
                .collect();

            // An `#undef`'s fact carries the **whole directive**, because the directive reader does not track where
            // the name is inside it — and the whole directive is the wrong thing for a rename to replace: it would
            // delete the `#undef`. The lexer is the thing that does know, so a hit inside one of these ranges is not
            // a use and not a rejection: it is the position that fact was missing.
            let undefinitions: Vec<usize> = references
                .iter()
                .enumerate()
                .filter(|(_, reference)| reference.kind == ReferenceKind::Undefinition)
                .map(|(index, _)| index)
                .collect();

            // The environment is computed **once per file** and then asked about every hit. Asking
            // `macro_definition` per hit instead was the first version of this function, and the measurement is what
            // killed it: 4 079 hits in one file, each walking the include graph, took **3.8 s** — for an answer
            // whose every input was the same walk. Lexing those files took 15 ms.
            let environment = index.macro_environment(name, &path);

            for range in identifiers(&text, name) {
                if claimed.contains(&(range.start_offset, range.end_offset())) {
                    continue;
                }

                if let Some(named) = undefinitions
                    .iter()
                    .copied()
                    .find(|index| covers(references[*index].range, range))
                {
                    references[named].range = range;
                    continue;
                }

                match environment.is_a_macro_at(range.start_offset) {
                    // It is a macro here, and it is a *definition* that settles it: this is a use.
                    Known::Yes(found) => references.push(Reference {
                        range,
                        kind: ReferenceKind::Use {
                            resolved_to: found.file().to_path_buf(),
                        },
                    }),
                    // A macro, but reached through an `#if`: renaming it is a guess.
                    Known::Unknown(reason @ UnknownReason::ConditionalCompilation) => {
                        references.push(Reference {
                            range,
                            kind: ReferenceKind::Uncertain(reason),
                        });
                    }
                    // Everything else is "not this macro here": a variable of the same name, a name `#undef`ed
                    // above this point, or a name whose only definition is behind an include that did not resolve.
                    // The most interesting rejection there is, because the text looks exactly like a use.
                    Known::Unknown(_) | Known::No => answer.rejected += 1,
                }
            }
        }

        if !references.is_empty() {
            references.sort_by_key(|reference| reference.range.start_offset);
            answer.files.push(FileReferences {
                file: path,
                references,
            });
        }
    }

    answer.files.sort_by(|one, other| one.file.cmp(&other.file));
    Known::Yes(answer)
}

/// The files whose own `#define`s are what the name means somewhere.
fn files_defining(index: &ProjectIndex, name: &str) -> Vec<PathBuf> {
    index
        .summaries()
        .filter(|summary| {
            summary
                .macros
                .iter()
                .any(|fact| fact.name == name && fact.kind.is_definition())
        })
        .map(|summary| summary.path.clone())
        .collect()
}

/// The files that could use the name: the definers, and everything that transitively includes one.
///
/// The walk is over **reverse** include edges — the ones the index derives — because the question is the
/// opposite of the one an include answers: not "what does this file see" but "who sees this file". A file reached
/// twice is one file, which is what makes a diamond cheap and a cycle terminate.
fn candidates(
    index: &ProjectIndex,
    definers: &[PathBuf],
    budget: ReferenceBudget,
    answer: &mut MacroReferences,
) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pending: VecDeque<PathBuf> = definers.iter().cloned().collect();

    while let Some(path) = pending.pop_front() {
        if !seen.insert(normalize_path(&path, cfg!(windows))) {
            continue;
        }

        if found.len() >= budget.max_files {
            answer.not_looked_at += 1;
            continue;
        }

        found.push(path.clone());
        pending.extend(index.includers_of(&path));
    }

    // Path order, so that a second run answers in the same order: `includers_of` is a `BTreeSet` and the queue is
    // a `VecDeque`, and neither is a promise about the order two diamond paths arrive in.
    found.sort();
    found
}

/// Is `inner` inside `outer`?
fn covers(outer: SourceRange, inner: SourceRange) -> bool {
    outer.start_offset <= inner.start_offset && inner.end_offset() <= outer.end_offset()
}

/// Every identifier token of `text` whose spelling is exactly `name`.
///
/// The lexer rather than a text scan, and that is the whole reason this layer is affordable: a comment is **one
/// token** and a string literal is one token, so the two commonest false positives of "search the text for the
/// name" cannot reach this function at all. A text scan plus a hand-written "am I inside a comment" state machine
/// would be a second implementation of the lexer, and it would disagree with the parser about raw strings,
/// continuations, and line comments ending at a `\`-continued newline.
fn identifiers(text: &str, name: &str) -> Vec<SourceRange> {
    let mut errors = Vec::new();
    let mut lexer = CppLexer::new(text, LexerConfig::default(), &mut errors);

    lexer
        .tokenize()
        .into_iter()
        .filter(|token| token.kind == CppTokenKind::Identifier)
        .filter(|token| text.get(token.range.start_offset..token.range.end_offset()) == Some(name))
        .map(|token| token.range)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ReferenceBudget, ReferenceKind, macro_references};
    use crate::cache::SummaryKey;
    use crate::include::config::CompilerConfig;
    use crate::include::graph::Marked;
    use crate::include::paths::{FileProvider, MemoryFiles};
    use crate::index::{FileIndexer, ProjectIndex};
    use crate::symbol::{Known, UnknownReason};
    use std::path::Path;

    /// An index over a set of in-memory files, with the include edges resolved.
    ///
    /// A real index built the real way — [`FileIndexer`] per file — because the whole query depends on the reverse
    /// edges, and an index built by hand would be an index whose graph is whatever the test believed it was.
    fn index_of(files: &MemoryFiles, paths: &[&str]) -> ProjectIndex {
        let config = CompilerConfig::default();
        let indexer = FileIndexer::new(files, &config);
        let mut index = ProjectIndex::new();

        for path in paths {
            let path = Path::new(path);
            let source = files.read(path).expect("the fixture has the file");
            index.insert(indexer.index(path, &source, SummaryKey::new(0, 0)));
        }

        index
    }

    /// The macros a compilation starts with, for the tests that ask what a *decided* condition does: what a
    /// compiler predefines (`-dM`) and what the command line says (`-D`) arrive in the same shape, and only the
    /// names matter here.
    fn defined(names: &[&str]) -> Marked {
        let mut marked = Marked::default();

        for name in names {
            marked.define_on_the_command_line(name, None);
        }

        marked
    }

    /// Every reference of `name`, as `file:kind:text` so that an assertion says *where* as well as *what*.
    fn shown(files: &MemoryFiles, index: &ProjectIndex, name: &str) -> Vec<String> {
        match macro_references(index, files, name, ReferenceBudget::default()) {
            Known::Yes(found) => found
                .files
                .iter()
                .flat_map(|file| {
                    let text = files.read(&file.file).unwrap_or_default();
                    file.references.iter().map(move |reference| {
                        let kind = match &reference.kind {
                            ReferenceKind::Definition => "define".to_string(),
                            ReferenceKind::Undefinition => "undef".to_string(),
                            ReferenceKind::Use { .. } => "use".to_string(),
                            ReferenceKind::Uncertain(_) => "maybe".to_string(),
                        };
                        format!(
                            "{}:{kind}:{}",
                            file.file.file_name().unwrap().to_string_lossy(),
                            &text[reference.range.start_offset..reference.range.end_offset()]
                        )
                    })
                })
                .collect(),
            Known::Unknown(reason) => vec![format!("unknown: {}", reason.describe())],
            Known::No => vec!["no".to_string()],
        }
    }

    /// One project, reused by most of the tests below:
    ///
    /// ```text
    /// api.h      #define API int
    /// main.cpp   #include "api.h"   API f();        <- a definition and a certain use
    /// guarded.h  #define GUARDED int
    /// cond.cpp   #if defined(X) #include "guarded.h" #endif   GUARDED g();
    /// ```
    fn fixture() -> MemoryFiles {
        MemoryFiles::new()
            .with_file("/p/api.h", "#define API int\n")
            .with_file("/p/main.cpp", "#include \"api.h\"\nAPI f() { return 0; }\n")
            .with_file("/p/guarded.h", "#define GUARDED int\n")
            .with_file(
                "/p/cond.cpp",
                "#if defined(X)\n#include \"guarded.h\"\n#endif\nGUARDED g() { return 0; }\n",
            )
    }

    const FIXTURE_FILES: &[&str] = &["/p/api.h", "/p/main.cpp", "/p/guarded.h", "/p/cond.cpp"];

    #[test]
    fn a_use_in_the_file_that_includes_the_definition_is_found() {
        let files = fixture();
        let index = index_of(&files, FIXTURE_FILES);

        assert_eq!(
            shown(&files, &index, "API"),
            ["api.h:define:API", "main.cpp:use:API"],
            "the definition and the one use, in path order"
        );
    }

    #[test]
    fn a_name_in_a_comment_or_a_string_is_not_a_reference() {
        // The two commonest false positives of a text search, and the reason this layer lexes rather than greps:
        // a comment is one token and a string is one token, so neither can reach the identifier filter.
        let files = MemoryFiles::new()
            .with_file("/p/api.h", "#define API int\n")
            .with_file(
                "/p/main.cpp",
                "#include \"api.h\"\n// API is a macro\nconst char* s = \"API\";\nAPI f() { return 0; }\n",
            );
        let index = index_of(&files, &["/p/api.h", "/p/main.cpp"]);

        assert_eq!(shown(&files, &index, "API"), ["api.h:define:API", "main.cpp:use:API"]);

        match macro_references(&index, &files, "API", ReferenceBudget::default()) {
            Known::Yes(found) => assert_eq!(
                found.rejected, 0,
                "a comment and a string are not even candidates for rejection: they are not identifiers"
            ),
            other => panic!("the macro is indexed, got {other:?}"),
        }
    }

    #[test]
    fn a_name_after_an_undef_is_rejected_rather_than_reported() {
        // Text that looks exactly like a use and is not one. The `#undef` itself *is* a reference — a rename has to
        // keep it in step — and the name inside the directive is covered by that fact rather than reported twice.
        let files = MemoryFiles::new()
            .with_file("/p/api.h", "#define API int\n")
            .with_file(
                "/p/main.cpp",
                "#include \"api.h\"\n#undef API\nint API = 1;\n",
            );
        let index = index_of(&files, &["/p/api.h", "/p/main.cpp"]);

        assert_eq!(
            shown(&files, &index, "API"),
            ["api.h:define:API", "main.cpp:undef:API"],
            "the `#undef` is a reference, and it points at the *name* — not at the whole line, which is what the \
             directive reader recorded and what a rename must not replace"
        );

        match macro_references(&index, &files, "API", ReferenceBudget::default()) {
            Known::Yes(found) => assert_eq!(found.rejected, 1, "and the rejection is counted"),
            other => panic!("the macro is indexed, got {other:?}"),
        }
    }

    #[test]
    fn a_condition_is_decided_by_a_define_the_closure_wrote_earlier() {
        // The shape libstdc++ is built on: `bits/c++config.h` defines `_GLIBCXX_USE_CXX11_ABI` as `1`, and headers
        // written after it test `#if _GLIBCXX_USE_CXX11_ABI`. The condition is about a macro **no file in the
        // translation unit's environment defines** and the closure does — so it is the walk that decides it, by
        // reading the unit in order: `config.h`'s facts are in force by the time `widget.h` is read.
        //
        // Both halves are here and they are different answers from the same machinery: `ABI == 1` needs the
        // *value* (`MacroFact::value`, one integer literal) and `FEATURE` needs only the name.
        let files = MemoryFiles::new()
            .with_file("/p/config.h", "#define ABI 1\n#define FEATURE\n")
            .with_file(
                "/p/widget.h",
                "#include \"config.h\"\n#if ABI == 1\n#include \"modern.h\"\n#endif\n\
                 #ifdef FEATURE\n#include \"feature.h\"\n#endif\n",
            )
            .with_file("/p/modern.h", "#define MODERN int\n")
            .with_file("/p/feature.h", "#define FEATURE_ONLY int\n")
            .with_file("/p/main.cpp", "#include \"widget.h\"\nMODERN a;\nFEATURE_ONLY b;\n");

        let index = index_of(
            &files,
            &[
                "/p/config.h",
                "/p/widget.h",
                "/p/modern.h",
                "/p/feature.h",
                "/p/main.cpp",
            ],
        );

        assert_eq!(
            shown(&files, &index, "MODERN"),
            ["main.cpp:use:MODERN", "modern.h:define:MODERN"],
            "the value is read out of `config.h`, so the `#if` is taken and the include is certain"
        );
        assert_eq!(
            shown(&files, &index, "FEATURE_ONLY"),
            ["feature.h:define:FEATURE_ONLY", "main.cpp:use:FEATURE_ONLY"],
            "and a name defined with no body at all is enough for an `#ifdef`"
        );
    }

    #[test]
    fn a_condition_about_a_define_that_comes_later_after_the_include_is_not_decided() {
        // The order is the whole answer, so the *same two files* the other way round must not decide anything: a
        // `#define` below the `#include` is not in force when the include is read. A walk that collected every fact
        // of every reachable file and then evaluated would get this wrong, which is why the state is built as the
        // walk passes each fact rather than in one pass at the end.
        let files = MemoryFiles::new()
            .with_file("/p/config.h", "#define ABI 1\n")
            .with_file(
                "/p/widget.h",
                "#if ABI == 1\n#include \"modern.h\"\n#endif\n#include \"config.h\"\n",
            )
            .with_file("/p/modern.h", "#define MODERN int\n")
            .with_file("/p/main.cpp", "#include \"widget.h\"\nMODERN a;\n");

        let index = index_of(
            &files,
            &["/p/config.h", "/p/widget.h", "/p/modern.h", "/p/main.cpp"],
        );

        assert_eq!(
            shown(&files, &index, "MODERN"),
            ["main.cpp:maybe:MODERN", "modern.h:define:MODERN"],
            "at the `#if` the name was not defined yet: the include may or may not have happened"
        );
    }

    #[test]
    fn a_define_inside_a_region_the_walk_cannot_decide_does_not_decide_a_later_condition() {
        // The soundness rule for the state: a fact whose own region is undecidable is **not** in force, so it cannot
        // make a later condition decided. `#ifdef SETTING / #define WIDE 1 / #endif` says nothing about `WIDE` when
        // `SETTING` is unknown — and a state that took it anyway would answer the second `#if` with evidence that
        // depends on the first one's unknown answer.
        let files = MemoryFiles::new()
            .with_file(
                "/p/widget.h",
                "#ifdef SETTING\n#define WIDE 1\n#endif\n#if WIDE == 1\n#include \"wide.h\"\n#endif\n",
            )
            .with_file("/p/wide.h", "#define WIDE_ONLY int\n")
            .with_file("/p/main.cpp", "#include \"widget.h\"\nWIDE_ONLY a;\n");

        let index = index_of(&files, &["/p/widget.h", "/p/wide.h", "/p/main.cpp"]);

        assert_eq!(
            shown(&files, &index, "WIDE_ONLY"),
            ["main.cpp:maybe:WIDE_ONLY", "wide.h:define:WIDE_ONLY"],
            "the value came from a branch nobody can decide"
        );
    }

    #[test]
    fn a_define_inside_a_settled_region_is_in_force_without_its_value() {
        // `#ifndef NAME / #define NAME 1 / #endif` — the idiom `MacroFact::settles_the_name` exists for. The name
        // *is* a macro after the block whichever branch ran, so a later `#ifdef` is decided; and the *value* is not
        // certain (the other branch may have defined it differently), so a later `#if NAME == 1` is not.
        let files = MemoryFiles::new()
            .with_file(
                "/p/widget.h",
                "int early;\n#ifndef WIDE\n#define WIDE 1\n#endif\n\
                 #ifdef WIDE\n#include \"named.h\"\n#endif\n#if WIDE == 1\n#include \"valued.h\"\n#endif\n",
            )
            .with_file("/p/named.h", "#define NAMED int\n")
            .with_file("/p/valued.h", "#define VALUED int\n")
            .with_file("/p/main.cpp", "#include \"widget.h\"\nNAMED a;\nVALUED b;\n");

        let index = index_of(
            &files,
            &["/p/widget.h", "/p/named.h", "/p/valued.h", "/p/main.cpp"],
        );

        assert_eq!(
            shown(&files, &index, "NAMED"),
            ["main.cpp:use:NAMED", "named.h:define:NAMED"],
            "definedness is settled by the shape"
        );
        assert_eq!(
            shown(&files, &index, "VALUED"),
            ["main.cpp:maybe:VALUED", "valued.h:define:VALUED"],
            "the value is not: the other branch could have defined it as anything"
        );
    }

    #[test]
    fn a_guard_on_a_name_nothing_defines_is_taken_when_the_caller_declares_the_inputs_complete() {
        // The same fixture as the test above, with the *other* environment answer — the one `Session::open` gives a
        // project whose compile database it read. `NT_INCLUDED` is defined nowhere, and "nowhere" is a conclusion
        // the caller is entitled to draw when it has told us how the project is compiled: the guard is taken, the
        // include is certain, and the use behind it is a use.
        //
        // This is the MinGW `windef.h` shape, and the reason every one of `STDMETHODCALLTYPE`'s four thousand
        // references was a "maybe" (`docs/std-library.md`, round 14).
        let files = MemoryFiles::new()
            .with_file("/p/winnt.h", "#define WIN_ONLY int\n")
            .with_file(
                "/p/windef.cpp",
                "int early;\n#ifndef NT_INCLUDED\n#include \"winnt.h\"\n#endif\nWIN_ONLY x;\n",
            );

        // `Marked::default()` is a *complete* environment with nothing in it: the compiler predefines nothing and
        // the command line said nothing, which is the whole of what this compilation defines.
        let index = index_of(&files, &["/p/winnt.h", "/p/windef.cpp"]).with_macros(Marked::default());

        assert_eq!(
            shown(&files, &index, "WIN_ONLY"),
            ["windef.cpp:use:WIN_ONLY", "winnt.h:define:WIN_ONLY"],
            "the guard is not taken, so the include is in the translation unit"
        );
    }

    #[test]
    fn an_include_that_did_not_resolve_takes_the_completeness_claim_back() {
        // The other half of that bargain, and the reason it is safe to make: a walk that runs into an `#include`
        // it cannot read has a hole in what it knows — the file it names may define *anything* — so every later
        // "nothing defines this" answer is off again. Without this, a missing header would quietly turn into
        // negative answers about names it might have defined.
        let files = MemoryFiles::new()
            .with_file(
                "/p/windef.cpp",
                "#include \"nowhere_to_be_found.h\"\nint early;\n\
                 #ifndef NT_INCLUDED\n#include \"winnt.h\"\n#endif\nWIN_ONLY x;\n",
            )
            .with_file("/p/winnt.h", "#define WIN_ONLY int\n");

        let index = index_of(&files, &["/p/winnt.h", "/p/windef.cpp"]).with_macros(Marked::default());

        assert_eq!(
            shown(&files, &index, "WIN_ONLY"),
            ["windef.cpp:maybe:WIN_ONLY", "winnt.h:define:WIN_ONLY"],
            "the unresolved include could have defined `NT_INCLUDED`, so the guard is undecided"
        );
    }

    #[test]
    fn a_use_reached_through_a_conditional_include_is_certain_when_the_environment_decides_it() {
        // The same text as the test above, and the other answer. `-DX` is a fact about the *compilation*, not a
        // doubt about it: a compiler that was given `-DX` read `guarded.h`, so the use is a use. This is the
        // whole point of storing each region's condition in the summary and evaluating it at query time — the
        // summary is keyed without the macro environment, so the answer cannot be baked into it.
        let files = fixture();
        let index = index_of(&files, FIXTURE_FILES).with_macros(defined(&["X"]));

        assert_eq!(
            shown(&files, &index, "GUARDED"),
            ["cond.cpp:use:GUARDED", "guarded.h:define:GUARDED"],
            "no `maybe`: the include is in the translation unit"
        );
    }

    #[test]
    fn a_use_reached_through_an_include_the_environment_rules_out_is_not_a_use() {
        // The other decided answer, and it is the stronger claim: `#if 0` is not "I do not know", it is "not
        // compiled", so the include is not followed at all and the name in `never.cpp` is not a macro there.
        // Before the condition was evaluated this was a *maybe*, which is a use a rename would have been shown
        // and could have edited by mistake.
        let files = MemoryFiles::new()
            .with_file("/p/skipped.h", "#define SKIPPED int\n")
            .with_file(
                "/p/never.cpp",
                "#if 0\n#include \"skipped.h\"\n#endif\nSKIPPED x;\n",
            );
        let index = index_of(&files, &["/p/skipped.h", "/p/never.cpp"]);

        assert_eq!(
            shown(&files, &index, "SKIPPED"),
            ["skipped.h:define:SKIPPED"],
            "only the definition: the include is not in the translation unit"
        );
    }

    #[test]
    fn a_guard_on_a_name_nothing_defines_is_still_unknown_rather_than_untaken() {
        // `NT_INCLUDED` is the case this whole rule is careful about, measured in the MinGW headers:
        // `#ifndef NT_INCLUDED / #include <winnt.h> / #endif` in `windef.h`, where nothing in the closure defines
        // `NT_INCLUDED` at all. A closed-world reading says the guard is taken *or* not taken; the truth is that
        // the index cannot tell, and the include has to be followed with doubt rather than dropped.
        //
        // The declaration before the conditional keeps it from being the file's own guard, which is the one
        // conditional that is *not* a condition (see `deguard_the_files_own_guard`).
        let files = MemoryFiles::new()
            .with_file("/p/winnt.h", "#define WIN_ONLY int\n")
            .with_file(
                "/p/windef.cpp",
                "int early;\n#ifndef NT_INCLUDED\n#include \"winnt.h\"\n#endif\nWIN_ONLY x;\n",
            );
        let index = index_of(&files, &["/p/winnt.h", "/p/windef.cpp"]);

        assert_eq!(
            shown(&files, &index, "WIN_ONLY"),
            ["windef.cpp:maybe:WIN_ONLY", "winnt.h:define:WIN_ONLY"],
            "the include may have happened, so the use may be a use"
        );
    }

    #[test]
    fn a_definition_the_environment_rules_out_is_not_a_candidate() {
        // `#if 0` around a `#define`: the fact is in the file's text and a compiler never reads it, so the name is
        // not a macro in any file that includes this one. The region needs no environment to decide — `0` is `0` —
        // and the *use* in `main.cpp` is therefore not a use. Before the fact's own region was evaluated this was a
        // "maybe", which is a reference a rename would have been offered.
        let files = MemoryFiles::new()
            .with_file("/p/api.h", "#if 0\n#define API int\n#endif\n")
            .with_file("/p/main.cpp", "#include \"api.h\"\nAPI f();\n");
        let index = index_of(&files, &["/p/api.h", "/p/main.cpp"]);

        assert_eq!(
            shown(&files, &index, "API"),
            ["api.h:define:API"],
            "the `#define` is a fact about the text, and the use is not a use"
        );
    }

    #[test]
    fn a_use_reached_through_a_conditional_include_is_uncertain() {
        // Whether the include happened depends on a macro the analysis does not have, so the use *may* be a use.
        // `Uncertain` rather than `Use`, because everything downstream treats the two differently: one is edited,
        // the other is shown.
        let files = fixture();
        let index = index_of(&files, FIXTURE_FILES);

        assert_eq!(
            shown(&files, &index, "GUARDED"),
            ["cond.cpp:maybe:GUARDED", "guarded.h:define:GUARDED"],
            "path order, so `cond.cpp` comes first"
        );

        match macro_references(&index, &files, "GUARDED", ReferenceBudget::default()) {
            Known::Yes(found) => {
                assert_eq!(found.uncertain(), 1);
                assert_eq!(
                    found.rename("OTHER").len(),
                    1,
                    "only the definition is certain"
                );
            }
            other => panic!("the macro is indexed, got {other:?}"),
        }
    }

    #[test]
    fn a_file_that_cannot_see_the_macro_is_not_even_read() {
        // The candidate rule, and the money it saves: `other.cpp` uses the name but includes nothing, so no
        // preprocessor reading it would expand anything. It is not a candidate, and the provider proves it was not
        // read — an assertion about the *result* could not tell "not read" from "read and rejected".
        let files = fixture().with_file("/p/other.cpp", "API f() { return 0; }\n");
        let index = index_of(
            &files,
            &["/p/api.h", "/p/main.cpp", "/p/other.cpp", "/p/guarded.h", "/p/cond.cpp"],
        );

        assert_eq!(shown(&files, &index, "API"), ["api.h:define:API", "main.cpp:use:API"]);
        assert_eq!(
            files.reads_of("/p/other.cpp"),
            1,
            "read once, by the indexer — and never by the query"
        );
    }

    #[test]
    fn a_candidate_whose_text_has_no_such_name_is_filtered_before_the_lexer() {
        let files = fixture().with_file("/p/quiet.cpp", "#include \"api.h\"\nint x;\n");
        let index = index_of(&files, &["/p/api.h", "/p/main.cpp", "/p/quiet.cpp"]);

        match macro_references(&index, &files, "API", ReferenceBudget::default()) {
            Known::Yes(found) => {
                assert_eq!(found.looked_at, 3, "three candidates: both files and the definition");
                assert_eq!(found.without_the_name, 1, "and one of them has no `API` in it");
            }
            other => panic!("the macro is indexed, got {other:?}"),
        }
    }

    #[test]
    fn a_name_no_file_defines_is_unknown_rather_than_empty() {
        // The distinction the whole crate is built on: "nothing in the index defines it" is not "there are no
        // references". Under lazy indexing it also covers "the file that defines it has not been read yet".
        let files = fixture();
        let index = index_of(&files, FIXTURE_FILES);

        assert_eq!(
            macro_references(&index, &files, "NOTHING", ReferenceBudget::default()),
            Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("NOTHING")))
        );
    }

    #[test]
    fn a_candidate_that_cannot_be_read_is_reported_rather_than_dropped() {
        // The file was in the index a moment ago and is gone now. One deleted file does not stop the answer, and it
        // is not silently missing from it either.
        let files = fixture();
        let index = index_of(&files, FIXTURE_FILES);
        let nothing = MemoryFiles::new();

        match macro_references(&index, &nothing, "API", ReferenceBudget::default()) {
            Known::Yes(found) => {
                assert_eq!(found.looked_at, 0);
                assert_eq!(
                    found.unreadable.len(),
                    2,
                    "the definer and the file that includes it — the closure comes from the index, not from reading"
                );
                assert_eq!(found.candidates(), 2, "and they are still the candidates");
                assert_eq!(found.total(), 0);
            }
            other => panic!("the macro is indexed, got {other:?}"),
        }
    }

    #[test]
    fn the_counters_add_up_to_the_ladder_they_describe() {
        // Every number in this struct is one rung of the same ladder, and a caller reading them has to be able to
        // trust that. `_inline`'s first version of the probe did not: it added the skipped files to the read ones
        // and reported a candidate count that was 132 too high, which is a number nobody would have questioned.
        let files = fixture().with_file("/p/quiet.cpp", "#include \"api.h\"\nint x;\n");
        let index = index_of(&files, &["/p/api.h", "/p/main.cpp", "/p/quiet.cpp"]);

        let Known::Yes(found) = macro_references(&index, &files, "API", ReferenceBudget::default())
        else {
            panic!("the macro is indexed");
        };

        assert_eq!(found.candidates(), 3, "the definer and the two files that include it");
        assert_eq!(found.looked_at, 3, "all three were read");
        assert_eq!(found.without_the_name, 1, "one of them has no `API` in it");
        assert_eq!(found.lexed(), 2, "so two were lexed");
        assert_eq!(found.not_looked_at, 0);
        assert!(found.unreadable.is_empty());
    }

    #[test]
    fn the_budget_stops_the_answer_and_says_how_much_it_missed() {
        let files = fixture();
        let index = index_of(&files, FIXTURE_FILES);

        match macro_references(&index, &files, "API", ReferenceBudget { max_files: 1 }) {
            Known::Yes(found) => {
                assert_eq!(found.looked_at, 1);
                assert_eq!(found.not_looked_at, 1, "one candidate was left unread");
                assert_eq!(found.candidates(), 2, "and it is still counted as a candidate");
                assert!(
                    found.total() < 2,
                    "the answer is short because of it: {} references",
                    found.total()
                );
            }
            other => panic!("the macro is indexed, got {other:?}"),
        }
    }

    #[test]
    fn renaming_edits_exactly_the_ranges_that_hold_the_name() {
        // The test that makes "find references" a rename rather than a list: apply the edits backwards and the file
        // that comes out is the file with the new name. A range that is off by one character fails here, and it
        // would fail silently in a client that only compared counts.
        let files = fixture();
        let index = index_of(&files, FIXTURE_FILES);

        let Known::Yes(found) = macro_references(&index, &files, "API", ReferenceBudget::default())
        else {
            panic!("the macro is indexed");
        };

        let edits = found.rename("INT_TYPE");
        assert_eq!(edits.len(), 2, "the definition and the one use");

        let mut text = files.read(Path::new("/p/main.cpp")).expect("the file reads");
        for edit in edits.iter().filter(|edit| edit.file == Path::new("/p/main.cpp")) {
            text.replace_range(
                edit.range.start_offset..edit.range.end_offset(),
                &edit.replacement,
            );
        }

        assert_eq!(text, "#include \"api.h\"\nINT_TYPE f() { return 0; }\n");
    }

    #[test]
    fn the_answer_is_in_path_order_whatever_order_the_graph_reaches_files_in() {
        // Determinism, asserted because a query whose output order depends on a `HashMap` is a bug that only shows
        // up in a test that runs twice. The files are given to the index in an order that is *not* path order.
        let files = fixture();
        let index = index_of(
            &files,
            &["/p/cond.cpp", "/p/main.cpp", "/p/guarded.h", "/p/api.h"],
        );

        match macro_references(&index, &files, "API", ReferenceBudget::default()) {
            Known::Yes(found) => {
                let names: Vec<String> = found
                    .files
                    .iter()
                    .map(|file| file.file.file_name().unwrap().to_string_lossy().to_string())
                    .collect();
                assert_eq!(names, ["api.h", "main.cpp"]);
            }
            other => panic!("the macro is indexed, got {other:?}"),
        }
    }
}
