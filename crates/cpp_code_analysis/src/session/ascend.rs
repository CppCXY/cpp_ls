//! **Ascend C's vocabulary** — the words a Huawei AICore project reads as part of its kernel language rather than as
//! ordinary C++ names.
//!
//! The lexer deliberately leaves all of them as identifiers: `__aicore__` and `__gm__` are legal C++ names in a
//! project that never sees `ccec`. The project family (`ProjectLanguage::AscendC`, from `[compile.ascend]`) is what
//! makes them reserved spellings; this table is what a completion list and a hover popup answer from when the
//! toolkit headers are not installed.

/// Which part of the vocabulary a word belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WordKind {
    /// A placement/memory-space specifier: `__aicore__`, `__global__`, `__gm__`, `__ubuf__`.
    Qualifier,
    /// A function the AICore runtime declares (`GetBlockIdx`, `DataCopy`, …).
    Builtin,
    /// A type the toolkit declares (`GlobalTensor`, `LocalTensor`, `TPipe`, …).
    Type,
}

impl WordKind {
    /// The one line a completion list puts beside the word.
    pub fn detail(self) -> &'static str {
        match self {
            WordKind::Qualifier => "Ascend C qualifier",
            WordKind::Builtin => "Ascend C built-in function",
            WordKind::Type => "Ascend C toolkit type",
        }
    }
}

/// One word of Ascend C's vocabulary.
#[derive(Debug, Clone, Copy)]
pub struct Word {
    pub name: &'static str,
    pub kind: WordKind,
    /// The short description the completion list and a missing locale fall back to.
    pub detail: &'static str,
    /// The locale key whose value is the long hover popup — `ascend.__aicore__`, …
    pub hover_key: &'static str,
}

/// The kernel and memory-space qualifiers. Order is the order a kernel is usually written: function placement
/// first, then the memory spaces.
pub const QUALIFIERS: &[Word] = &[
    Word {
        name: "__aicore__",
        kind: WordKind::Qualifier,
        detail: "an AICore kernel or device function; runs on the AI Core",
        hover_key: "ascend.__aicore__",
    },
    Word {
        name: "__global__",
        kind: WordKind::Qualifier,
        detail: "the kernel entry point, called from the host",
        hover_key: "ascend.__global__",
    },
    Word {
        name: "__host__",
        kind: WordKind::Qualifier,
        detail: "runs on the host, not on the AI Core",
        hover_key: "ascend.__host__",
    },
    Word {
        name: "__device__",
        kind: WordKind::Qualifier,
        detail: "runs on the AI Core device",
        hover_key: "ascend.__device__",
    },
    Word {
        name: "__gm__",
        kind: WordKind::Qualifier,
        detail: "the global memory space: visible to the host and every AI Core",
        hover_key: "ascend.__gm__",
    },
    Word {
        name: "__ubuf__",
        kind: WordKind::Qualifier,
        detail: "the Unified Buffer: on-chip scratch memory for vector operations",
        hover_key: "ascend.__ubuf__",
    },
    Word {
        name: "__cbuf__",
        kind: WordKind::Qualifier,
        detail: "the Cube Buffer: on-chip scratch memory for matrix operations",
        hover_key: "ascend.__cbuf__",
    },
    Word {
        name: "__fbuf__",
        kind: WordKind::Qualifier,
        detail: "the Fixpipe Buffer: the matrix multiply result's on-chip buffer",
        hover_key: "ascend.__fbuf__",
    },
    Word {
        name: "__ca__",
        kind: WordKind::Qualifier,
        detail: "the L1/L0A cache space of an AI Core",
        hover_key: "ascend.__ca__",
    },
    Word {
        name: "__cb__",
        kind: WordKind::Qualifier,
        detail: "the L1/L0B cache space of an AI Core",
        hover_key: "ascend.__cb__",
    },
    Word {
        name: "__cc__",
        kind: WordKind::Qualifier,
        detail: "the L0C cache space: the matrix accumulator",
        hover_key: "ascend.__cc__",
    },
];

/// The runtime functions a kernel calls, and the toolkit types a kernel is built from. The types live in the
/// toolkit's headers; the table exists for the same reason CUDA's does — a completion and a popup must work before
/// those headers are read.
pub const BUILTINS: &[Word] = &[
    Word {
        name: "GetBlockIdx",
        kind: WordKind::Builtin,
        detail: "the index of this AI Core block",
        hover_key: "ascend.GetBlockIdx",
    },
    Word {
        name: "GetBlockNum",
        kind: WordKind::Builtin,
        detail: "the number of AI Core blocks in the grid",
        hover_key: "ascend.GetBlockNum",
    },
    Word {
        name: "GetSubBlockIdx",
        kind: WordKind::Builtin,
        detail: "the sub-block index inside a block, for split vector/cube work",
        hover_key: "ascend.GetSubBlockIdx",
    },
    Word {
        name: "DataCopy",
        kind: WordKind::Builtin,
        detail: "copy between global and on-chip memory",
        hover_key: "ascend.DataCopy",
    },
    Word {
        name: "PipeBarrier",
        kind: WordKind::Builtin,
        detail: "synchronise the pipeline stages before continuing",
        hover_key: "ascend.PipeBarrier",
    },
    Word {
        name: "InitGlobalMemory",
        kind: WordKind::Builtin,
        detail: "fill global memory with a value",
        hover_key: "ascend.InitGlobalMemory",
    },
    Word {
        name: "TPipe",
        kind: WordKind::Type,
        detail: "the per-kernel pipeline that owns queues and buffers",
        hover_key: "ascend.TPipe",
    },
    Word {
        name: "TQue",
        kind: WordKind::Type,
        detail: "a typed queue for passing tensors between pipeline stages",
        hover_key: "ascend.TQue",
    },
    Word {
        name: "TQueSync",
        kind: WordKind::Type,
        detail: "a queue synchronisation point",
        hover_key: "ascend.TQueSync",
    },
    Word {
        name: "GlobalTensor",
        kind: WordKind::Type,
        detail: "a tensor in global memory",
        hover_key: "ascend.GlobalTensor",
    },
    Word {
        name: "LocalTensor",
        kind: WordKind::Type,
        detail: "a tensor in on-chip memory",
        hover_key: "ascend.LocalTensor",
    },
    Word {
        name: "TBuf",
        kind: WordKind::Type,
        detail: "a raw on-chip buffer",
        hover_key: "ascend.TBuf",
    },
];

/// Every word of Ascend C's vocabulary.
pub fn words() -> impl Iterator<Item = &'static Word> {
    QUALIFIERS.iter().chain(BUILTINS)
}

/// One word by spelling, or `None` when the spelling is not part of this vocabulary.
pub fn word(name: &str) -> Option<Word> {
    words().find(|word| word.name == name).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

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
                word.hover_key.starts_with("ascend."),
                "{} has a key outside the ascend section: {}",
                word.name,
                word.hover_key
            );
        }
    }
}
