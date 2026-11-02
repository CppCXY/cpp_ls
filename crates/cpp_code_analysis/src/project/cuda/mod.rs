//! **CUDA** — everything the analysis knows about it, in one module.
//!
//! ```text
//! .cppls.toml                 [compile.cuda] enable = true
//! <toolkit>/include           cuda_runtime.h, the vector types, the CCCL include from 12.8 on
//! .cu / .cuh                  the files the grammar reads with the CUDA rules
//! ```
//!
//! # What is here today
//!
//! [`CudaSection`] — the `[compile.cuda]` section of `.cppls.toml` — and the one function that turns it into a
//! [`CompilerConfig`](crate::include::config::CompilerConfig): the toolkit's include directories, the host compiler's
//! dialect, extra includes, extra flags.
//!
//! # Why the section and its application live together
//!
//! Because they are one statement: a project that names a toolkit means "and read the files as CUDA", and splitting
//! that across three call sites is how the include path would one day be added without the switch. The session calls
//! [`CudaSection::applied_to`] once, after the toolchain, and nothing else in the analysis needs to know the keys.
//!
//! # What will be here
//!
//! The rest of the CUDA-specific reading, as it is asked for:
//!
//! ```text
//! discovery     the toolkit's version (`nvcc --version`), the `<toolkit>/include/cccl` layout, and the flags a
//!               project's `-arch=` implies — none of which the host toolchain can answer
//! passes        which lines are the device pass, when `__CUDA_ARCH__` may and may not be defined
//! kernels       the launch bracket `<<<…>>>`, its grid/block argument types, and the kernel's own parameter rules
//! ```
//!
//! The rules that judge CUDA code are checks like any other and live in [`crate::sema::check`](crate::sema::check); this
//! module owns the configuration and the discovery, so that the grammar's CUDA flag has one source.

use std::path::PathBuf;

use serde::Deserialize;

/// `[compile.cuda]` — a CUDA project's extra configuration.
///
/// ```toml
/// [compile.cuda]
/// enable = true                                       # read this project as CUDA even if nothing says so
/// toolkit = "C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v12.4"
/// host_compiler = "cl"                                # the host dialect: cl (MSVC) or g++/clang++ (GNU)
/// include = ["third_party/cutlass/include"]           # extra include directories for CUDA units
/// args = ["-arch=sm_80"]                              # extra flags, appended to whatever won
/// ```
///
/// # What each key is for, and what is deliberately absent
///
/// `enable` is the escape hatch of the detection: the analysis reads a project as CUDA when its toolchain is `nvcc`
/// (`__CUDACC__` in the macro table, or the compiler's name), and a project that has `.cu` files but no compile
/// database has neither. Saying so here is one line, and it is the same switch — the grammar's CUDA rules, on.
///
/// `toolkit` is the key that makes `<cuda_runtime.h>` resolve: the toolkit installs its headers under
/// `<toolkit>/include` (and, from CUDA 12.8, a CCCL include beside it), and **nothing on the system path knows
/// that**. The directory is added as a system include path, so `#include <cuda_runtime.h>` and the vector types it
/// defines (`float4`, `dim3`) become visible without a single line in the project's own headers.
///
/// `host_compiler` decides the **dialect**, which is a different question from the CUDA flag: on Windows `nvcc`
/// drives `cl.exe`, on Linux `g++`, so `__int64` and `__cdecl` are MSVC's names in one project and GNU's in the
/// other. Only the name is read here — `cl` (and `cl.exe`) means MSVC, anything with `g++`, `gcc` or `clang` in it
/// means GNU — because the compiler itself is discovered and asked elsewhere.
///
/// **`__CUDA_ARCH__` is deliberately not a key.** It is defined **only in the device compilation pass**, so a
/// language server that defined it for the whole file would evaluate `#if __CUDA_ARCH__ >= 800` in host code that a
/// compiler never sees. An `arch` key would be a lie of exactly that shape; a project that needs it can put it in
/// `args` and accept the same lie knowingly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CudaSection {
    /// Read this project as CUDA even when the toolchain does not say so — see the section's own note.
    #[serde(default)]
    pub enable: bool,
    /// The CUDA toolkit's installation directory — `…/CUDA/v12.4`. Its `include` (and `include/cccl`, when present)
    /// are added as system include paths.
    #[serde(default)]
    pub toolkit: Option<PathBuf>,
    /// The host compiler's name, for the **dialect**: `cl` is MSVC, `g++`/`gcc`/`clang++` is GNU.
    #[serde(default)]
    pub host_compiler: Option<String>,
    /// Extra include directories, relative to the workspace root (or absolute), added after the toolkit's own.
    #[serde(default)]
    pub include: Vec<PathBuf>,
    /// Flags appended to whichever set won — the same arrangement [`CompileSection::extra_args`] has.
    ///
    /// [`CompileSection::extra_args`]: crate::project::config::CompileSection::extra_args
    #[serde(default)]
    pub args: Vec<String>,
}

impl CudaSection {
    /// **The configuration this section describes**, applied to what discovery worked out.
    ///
    /// One function rather than a handful of edits in the session, because the keys are one statement: a project that
    /// names a toolkit means "and read the files as CUDA", and splitting that across three call sites is how the
    /// include path would one day be added without the switch.
    pub fn applied_to(
        &self,
        mut config: crate::include::config::CompilerConfig,
        root: &std::path::Path,
    ) -> crate::include::config::CompilerConfig {
        if self.enable {
            config = config.with_cuda(true);
        }
        if let Some(host) = &self.host_compiler {
            let name = host
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(host)
                .trim_end_matches(".exe");
            let dialect = if name == "cl" {
                cpp_parser::Dialect::Msvc
            } else if name.contains("g++")
                || name.contains("gcc")
                || name.contains("clang")
                || name == "cc"
                || name == "c++"
            {
                cpp_parser::Dialect::Gnu
            } else {
                config.dialect()
            };
            config = config.with_dialect(dialect);
        }
        if let Some(toolkit) = &self.toolkit {
            // **A relative toolkit is relative to the workspace root**, like every other path in this file: the
            // process's own directory is wherever the editor was started from, which has nothing to do with the
            // project. Measured: `toolkit = "cuda"` added `cuda/include`, a relative spelling the include resolver
            // could not open, and the header it was supposed to bring in was reported missing.
            let toolkit = if toolkit.is_absolute() {
                toolkit.clone()
            } else {
                root.join(toolkit)
            };
            let toolkit = &toolkit;
            // **The toolkit's own include directories**, which no compiler on the machine answers for: `nvcc` ships
            // them, and they are not on `PATH` or in any system directory. `cccl` is the CUDA 12.8+ layout, where the
            // C++ standard library parallel algorithms moved out of `include` — added only when it exists, so an older
            // toolkit does not gain a directory that is not there.
            for relative in ["include", "include/cccl"] {
                let directory = toolkit.join(relative);
                if relative == "include" || directory.is_dir() {
                    config
                        .include_paths
                        .push(crate::include::config::IncludePath::system(directory));
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
                .push(crate::include::config::IncludePath::system(directory));
        }
        config.extra_args.extend(self.args.iter().cloned());
        config
    }
}
