//! **Ascend C** — Huawei's AICore kernel language over C++.
//!
//! ```toml
//! [compile.ascend]
//! enable = true
//! toolkit = "C:/Ascend/ascend-toolkit/latest"
//! include = ["src/kernels/include"]
//! args = ["-DASCENDC_CPU_DEBUG"]
//! ```
//!
//! Ascend C is C++ with a kernel vocabulary (`__aicore__`, `__global__`, `__gm__`, `__ubuf__`, `AscendC::TPipe`,
//! `GlobalTensor`, `LocalTensor`, …). The grammar switch is [`cpp_parser::ParserConfig::ascend_c`]; this section is
//! what a project says when no compile database names `ccec`, plus the toolkit directories that make
//! `#include "kernel_operator.h"` resolve.
//!
//! **The family is exclusive.** A project is ordinary C++, CUDA, Unreal, or Ascend C — never two; the configuration
//! validator rejects a file that enables more than one.

use crate::include;
use std::path::PathBuf;

use serde::Deserialize;

/// `[compile.ascend]` — an Ascend C project's extra configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AscendSection {
    /// Read this project as Ascend C even when the toolchain is only a host compiler.
    #[serde(default)]
    pub enable: bool,
    /// The Ascend toolkit installation directory — `…/ascend-toolkit/latest`.
    #[serde(default)]
    pub toolkit: Option<PathBuf>,
    /// Extra include directories, relative to the workspace root (or absolute).
    #[serde(default)]
    pub include: Vec<PathBuf>,
    /// Flags appended to whichever set won.
    #[serde(default)]
    pub args: Vec<String>,
}

impl AscendSection {
    /// The configuration this section describes, applied after the toolchain and the compilation database.
    pub fn applied_to(
        &self,
        mut config: include::config::CompilerConfig,
        root: &std::path::Path,
    ) -> include::config::CompilerConfig {
        if self.enable {
            config = config.with_language_family(crate::ProjectLanguage::AscendC);
        }
        if let Some(toolkit) = &self.toolkit {
            let toolkit = if toolkit.is_absolute() {
                toolkit.clone()
            } else {
                root.join(toolkit)
            };
            // The layout of an Ascend installation, nearest first. The two `tikcfw` directories are where
            // `kernel_operator.h` and its interface headers live; `include` is the toolkit's public header root.
            for relative in [
                "compiler/tikcpp/tikcfw",
                "compiler/tikcpp/tikcfw/interface",
                "include",
            ] {
                let directory = toolkit.join(relative);
                if directory.is_dir() {
                    config
                        .include_paths
                        .push(include::config::IncludePath::system(directory));
                }
            }
        }
        for directory in &self.include {
            let directory = if directory.is_absolute() {
                directory.clone()
            } else {
                root.join(directory)
            };
            config
                .include_paths
                .push(include::config::IncludePath::system(directory));
        }
        config.extra_args.extend(self.args.iter().cloned());
        config
    }
}
