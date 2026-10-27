//! **CUDA's own vocabulary**: the words a CUDA project reads as part of the language and a C++ compiler does not.
//!
//! There are three kinds, and the difference is *why* each is here rather than in the keyword table:
//!
//! ```text
//! the placement qualifiers   __global__ __device__ __shared__ …   a specifier the language does not have
//! the built-in variables     threadIdx blockIdx blockDim gridDim warpSize
//!                                                                  declared by nvcc's headers, not by the language
//! the vector types           dim3 char1..4 float4 double4 …        likewise: `vector_types.h` declares them
//! ```
//!
//! # Why this is a table of its own, and not the lexer's keywords
//!
//! A CUDA qualifier is an **identifier** to this lexer, deliberately: `__global__` is a legal variable name in C++,
//! and a lexer that made it a keyword would break every project that has one. What decides that `__global__` is a
//! specifier is the *family* the file is compiled for — `ParserConfig::cuda`, set from the toolchain (`__CUDACC__`,
//! `nvcc`) or from `[compile.cuda]` in `.cppls.toml` — which is a fact about the project, not about the token.
//!
//! The same reasoning is why nothing here is offered by default: a completion list is a claim about what may be
//! written, and offering `__shared__` in a file that no CUDA compiler will ever see is a claim that is false.
//!
//! # What is deliberately absent
//!
//! `__CUDA_ARCH__` is not a word here, and it is not a keyword anywhere: it is defined **only in the device
//! compilation pass**, so a server that defined it for a whole file would send host code down a branch a real
//! compiler never takes. The rest of nvcc's predefined macros (`__CUDACC__`, `__NVCC__`) are facts about the
//! *toolchain*, which is where they are read — see `crate::include::toolchain`.
//!
//! The vector types are generated: the CUDA headers declare `char1` through `char4`, and one of them differs from
//! its siblings only in the number of components. Writing out forty near-identical entries by hand is how a table
//! grows a hole — `ushort3` missing while `ushort2` and `ushort4` are there.

/// Which of the three kinds a word is.
///
/// The kind is what a completion list shows beside the name and what a popup leads with, so it is stated once here
/// rather than inferred from the spelling at each consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WordKind {
    /// A specifier: a placement qualifier (`__global__`), an inline hint (`__forceinline__`), or one of the
    /// qualifiers written with an argument list (`__launch_bounds__(…)`).
    Qualifier,
    /// A variable the toolkit declares before the file's first line: `threadIdx`, `blockDim`, `warpSize`.
    Builtin,
    /// A type the toolkit declares: `dim3`, and the vector families.
    VectorType,
}

impl WordKind {
    /// What a completion list says beside a word of this kind — the same sentence a reader sees in the popup's
    /// first line, so that the list and the card cannot describe the same word differently.
    pub fn detail(self) -> &'static str {
        match self {
            WordKind::Qualifier => "CUDA qualifier",
            WordKind::Builtin => "CUDA built-in variable",
            WordKind::VectorType => "CUDA vector type",
        }
    }
}

/// One word of CUDA's vocabulary.
#[derive(Debug, Clone, Copy)]
pub struct Word {
    /// The spelling, exactly as it must be written.
    pub name: &'static str,
    pub kind: WordKind,
    /// **The popup and the completion item's one line.** A sentence in the voice of the rest of the server's text,
    /// and the thing a translation may replace: [`Word::hover_key`] is looked up first, and this is what a locale
    /// that has no entry for the word falls back to.
    pub detail: &'static str,
    /// The `rust-i18n` key whose value is the long popup — `cuda.__global__`, `cuda.vector.float`, …
    ///
    /// A key rather than the text, because names of this vocabulary share a translation: the twelve `float`s of
    /// `float4` are one sentence about the family, not four sentences about the members. See the `cuda` section of
    /// `crates/cpp_ls/locales/en.yml`, which is where a translation lives beside the one it translates.
    pub hover_key: &'static str,
}

impl Word {
    /// Does this word take an argument list — `__launch_bounds__(1024)`, `__align__(16)`?
    ///
    /// The parser reads them as a specifier followed by a token group; a completion that inserts the name alone is
    /// the right thing to insert either way, so this is for the popup's syntax line rather than for behaviour.
    pub fn takes_an_argument_list(&self) -> bool {
        matches!(
            self.name,
            "__launch_bounds__" | "__maxnreg__" | "__align__" | "__cluster_dims__"
        )
    }
}

/// **The qualifiers.** A specifier in CUDA and an identifier in C++.
///
/// The order is the one a reader meets them in: the ones that decide *where* code runs, then the memory space, then
/// the function attributes. It is also the order a completion list falls back to when nothing has been typed.
pub const QUALIFIERS: &[Word] = &[
    Word {
        name: "__global__",
        kind: WordKind::Qualifier,
        detail: "a kernel: called from host code, run on the device",
        hover_key: "cuda.__global__",
    },
    Word {
        name: "__device__",
        kind: WordKind::Qualifier,
        detail: "runs on the device: callable from a kernel or from another device function",
        hover_key: "cuda.__device__",
    },
    Word {
        name: "__host__",
        kind: WordKind::Qualifier,
        detail: "runs on the host: a function compiled for the CPU",
        hover_key: "cuda.__host__",
    },
    Word {
        name: "__shared__",
        kind: WordKind::Qualifier,
        detail: "a block-wide scratch array in shared memory",
        hover_key: "cuda.__shared__",
    },
    Word {
        name: "__constant__",
        kind: WordKind::Qualifier,
        detail: "read-only device memory written by the host",
        hover_key: "cuda.__constant__",
    },
    Word {
        name: "__managed__",
        kind: WordKind::Qualifier,
        detail: "memory accessible from both the host and the device",
        hover_key: "cuda.__managed__",
    },
    Word {
        name: "__grid_constant__",
        kind: WordKind::Qualifier,
        detail: "a kernel parameter read-only on the device, not copied per thread",
        hover_key: "cuda.__grid_constant__",
    },
    Word {
        name: "__restrict__",
        kind: WordKind::Qualifier,
        detail: "no aliasing through this pointer, as `restrict` is to C",
        hover_key: "cuda.__restrict__",
    },
    Word {
        name: "__launch_bounds__",
        kind: WordKind::Qualifier,
        detail: "the largest block, and the smallest block count, a kernel is launched with",
        hover_key: "cuda.__launch_bounds__",
    },
    Word {
        name: "__maxnreg__",
        kind: WordKind::Qualifier,
        detail: "the maximum number of registers a kernel's threads may use",
        hover_key: "cuda.__maxnreg__",
    },
    Word {
        name: "__align__",
        kind: WordKind::Qualifier,
        detail: "the alignment of a declaration, as `alignas` is to C++",
        hover_key: "cuda.__align__",
    },
    Word {
        name: "__cluster_dims__",
        kind: WordKind::Qualifier,
        detail: "the shape of the thread-block cluster this kernel is launched in",
        hover_key: "cuda.__cluster_dims__",
    },
    Word {
        name: "__forceinline__",
        kind: WordKind::Qualifier,
        detail: "always inline this function",
        hover_key: "cuda.__forceinline__",
    },
    Word {
        name: "__noinline__",
        kind: WordKind::Qualifier,
        detail: "never inline this function",
        hover_key: "cuda.__noinline__",
    },
];

/// **The variables the toolkit defines before the first line of the file.**
///
/// They are `extern const` in `cuda_runtime.h` — which is why a project without a toolkit has them *unresolved* and
/// this table is what answers for them: a reader whose machine has no CUDA install still needs to know what
/// `threadIdx` is. When the header is on the include path the declaration wins, and the popup shows the real type.
pub const BUILTINS: &[Word] = &[
    Word {
        name: "threadIdx",
        kind: WordKind::Builtin,
        detail: "the index of this thread within its block, a `dim3`",
        hover_key: "cuda.threadIdx",
    },
    Word {
        name: "blockIdx",
        kind: WordKind::Builtin,
        detail: "the index of this block within the grid, a `dim3`",
        hover_key: "cuda.blockIdx",
    },
    Word {
        name: "blockDim",
        kind: WordKind::Builtin,
        detail: "the number of threads in this block, a `dim3`",
        hover_key: "cuda.blockDim",
    },
    Word {
        name: "gridDim",
        kind: WordKind::Builtin,
        detail: "the number of blocks in this grid, a `dim3`",
        hover_key: "cuda.gridDim",
    },
    Word {
        name: "warpSize",
        kind: WordKind::Builtin,
        detail: "the number of threads in a warp: 32 on every NVIDIA GPU so far",
        hover_key: "cuda.warpSize",
    },
];

/// **The vector types that are not one of the numbered families** — plus `dim3`, which is what a launch is
/// configured with.
pub const VECTOR_OTHERS: &[Word] = &[
    Word {
        name: "dim3",
        kind: WordKind::VectorType,
        detail: "three unsigned components, `.x .y .z`, used for a grid's and a block's shape",
        hover_key: "cuda.dim3",
    },
    Word {
        name: "__half",
        kind: WordKind::VectorType,
        detail: "a 16-bit floating-point value (`cuda_fp16.h`)",
        hover_key: "cuda.__half",
    },
    Word {
        name: "__half2",
        kind: WordKind::VectorType,
        detail: "two 16-bit floating-point values (`cuda_fp16.h`)",
        hover_key: "cuda.__half2",
    },
    Word {
        name: "__nv_bfloat16",
        kind: WordKind::VectorType,
        detail: "a 16-bit brain float (`cuda_bf16.h`)",
        hover_key: "cuda.__nv_bfloat16",
    },
];

/// **One numbered family, written out four times at compile time.**
///
/// The bodies are `concat!`s of literals, so the table is `const` and nothing is allocated: a completion list is built
/// on every keystroke, and a table that leaked a `String` per word per call would be a slow leak in the editor's own
/// process. The alternative — writing forty near-identical `Word`s by hand — is how a table grows a hole (`ushort3`
/// missing while `ushort2` and `ushort4` are there), which is what the test below exists to catch.
macro_rules! vector_family {
    ($base:literal, $key:literal, $element:literal) => {
        [
            Word {
                name: concat!($base, "1"),
                kind: WordKind::VectorType,
                detail: concat!("1 × ", $element, ", `.x`"),
                hover_key: $key,
            },
            Word {
                name: concat!($base, "2"),
                kind: WordKind::VectorType,
                detail: concat!("2 × ", $element, ", `.x .y`"),
                hover_key: $key,
            },
            Word {
                name: concat!($base, "3"),
                kind: WordKind::VectorType,
                detail: concat!("3 × ", $element, ", `.x .y .z`"),
                hover_key: $key,
            },
            Word {
                name: concat!($base, "4"),
                kind: WordKind::VectorType,
                detail: concat!("4 × ", $element, ", `.x .y .z .w`"),
                hover_key: $key,
            },
        ]
    };
}

/// The bases of the numbered families: `char1` … `double4`.
///
/// `long` is here and `longlong` too because `long1` and `longlong1` are different types in the CUDA headers — a
/// 32-bit integer on Windows and a 64-bit one on Linux in the first case, and always 64-bit in the second.
const VECTOR_FAMILIES: &[[Word; 4]] = &[
    vector_family!("char", "cuda.vector_char", "a signed 8-bit integer"),
    vector_family!("uchar", "cuda.vector_uchar", "an unsigned 8-bit integer"),
    vector_family!("short", "cuda.vector_short", "a signed 16-bit integer"),
    vector_family!("ushort", "cuda.vector_ushort", "an unsigned 16-bit integer"),
    vector_family!("int", "cuda.vector_int", "a signed 32-bit integer"),
    vector_family!("uint", "cuda.vector_uint", "an unsigned 32-bit integer"),
    vector_family!("long", "cuda.vector_long", "a signed long"),
    vector_family!("ulong", "cuda.vector_ulong", "an unsigned long"),
    vector_family!(
        "longlong",
        "cuda.vector_longlong",
        "a signed 64-bit integer"
    ),
    vector_family!(
        "ulonglong",
        "cuda.vector_ulonglong",
        "an unsigned 64-bit integer"
    ),
    vector_family!("float", "cuda.vector_float", "a 32-bit float"),
    vector_family!("double", "cuda.vector_double", "a 64-bit float"),
];

/// **Every word**, in the order a completion list offers them: the qualifiers, then the built-ins, then the types.
///
/// An iterator rather than a `Vec`: the table is `const` (see [`vector_family`]), so borrowing it costs nothing and a
/// keystroke allocates nothing. `QUALIFIERS`, `BUILTINS`, `VECTOR_OTHERS` and `VECTOR_FAMILIES` are walked in that
/// order, and a caller that wants a count asks for one.
pub fn words() -> impl Iterator<Item = &'static Word> {
    QUALIFIERS
        .iter()
        .chain(BUILTINS)
        .chain(VECTOR_OTHERS)
        .chain(VECTOR_FAMILIES.iter().flatten())
}
/// One word by spelling, or `None` for anything that is not part of this vocabulary.
///
/// A linear scan of eighty short strings, for the reason [`crate::completion::table`] gives about its own tables: a
/// completion is a keystroke, and a `HashMap` here would be one more thing to keep in step with a `const`.
pub fn word(name: &str) -> Option<Word> {
    words().find(|word| word.name == name).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The table is a table**: nothing is listed twice, and every entry says something.
    #[test]
    fn every_word_is_listed_once_and_says_something() {
        let words: Vec<&Word> = words().collect();
        let mut names: Vec<&str> = words.iter().map(|word| word.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "a spelling is listed twice");

        for word in words {
            assert!(!word.detail.is_empty(), "{} has no description", word.name);
            assert!(
                word.hover_key.starts_with("cuda."),
                "{} has a key outside the cuda section: {}",
                word.name,
                word.hover_key
            );
        }
    }

    /// **The vocabulary is what a CUDA project may write, and the count is the measure of it.**
    ///
    /// The numbers are here so that a word added without a thought shows up as a diff rather than as a list that grew
    /// quietly: fourteen qualifiers, five built-in variables, four types that are not a numbered family, and twelve
    /// families of four.
    #[test]
    fn the_vocabulary_is_the_one_a_cuda_project_writes() {
        let words: Vec<&Word> = words().collect();
        let of = |kind: WordKind| words.iter().filter(|word| word.kind == kind).count();

        assert_eq!(of(WordKind::Qualifier), 14);
        assert_eq!(of(WordKind::Builtin), 5);
        assert_eq!(of(WordKind::VectorType), 4 + VECTOR_FAMILIES.len() * 4);
        assert_eq!(words.len(), 14 + 5 + 4 + 12 * 4);
    }

    /// **The numbered families are complete**, because a hole in one is invisible until someone types the name.
    ///
    /// This is the test that would catch `ushort3` missing while `ushort2` and `ushort4` are there — which is how a
    /// list of forty near-identical entries fails when it is written by hand.
    #[test]
    fn every_vector_family_has_one_to_four_components() {
        for family in VECTOR_FAMILIES {
            let base = family[0].name.trim_end_matches('1');
            for components in 1..=4 {
                let name = format!("{base}{components}");
                let found = word(&name);
                assert!(found.is_some(), "{name} is missing from the table");
                assert_eq!(found.expect("just checked").kind, WordKind::VectorType);
            }
            assert!(
                word(&format!("{base}5")).is_none(),
                "{base}5 is not a type a toolkit declares"
            );
        }
    }

    /// **A spelling that is not in the vocabulary is not a word**, and case matters: `ThreadIdx` is a name.
    #[test]
    fn the_lookup_is_exact() {
        assert!(word("threadIdx").is_some());
        assert!(word("ThreadIdx").is_none());
        assert!(word("threadidx").is_none());
        assert!(word("").is_none());
        assert!(
            word("__CUDA_ARCH__").is_none(),
            "device-only, deliberately absent"
        );
        assert!(
            word("float").is_none(),
            "a C++ keyword is not CUDA's vocabulary"
        );
    }

    /// **The parser and this table agree about the qualifiers** — the cross-check that keeps two lists honest.
    ///
    /// The grammar's own list is in `cpp_parser` (a `matches!` over spellings, because a parser matches on tokens);
    /// this is the same fact said as data, and a qualifier added to one and not the other would show up as a
    /// completion that inserts something the parser does not read. Each is written where a specifier may stand.
    #[test]
    fn the_parser_reads_every_qualifier_this_table_offers() {
        for word in QUALIFIERS {
            let source = if word.takes_an_argument_list() {
                format!(
                    "__global__ void __global__x() {{ }}\n{} void f() {{ }}\n",
                    word.name
                )
            } else {
                format!("{} void f() {{ }}\n", word.name)
            };
            let tree = cpp_parser::CppParser::parse(
                &source,
                cpp_parser::ParserConfig::default().with_cuda(true),
            );
            assert!(
                tree.get_errors().is_empty(),
                "`{}` is offered but does not parse as a specifier: {:?}\n{source}",
                word.name,
                tree.get_errors()
            );
        }
    }
}
