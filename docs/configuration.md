# Configuration and limits

This is the reference for **how the server is configured** and **what it deliberately does not do**. The language
features themselves are in [`c-support.md`](c-support.md) and [`cpp-support.md`](cpp-support.md).

## 1. Two inputs, and no third

```text
compile_commands.json     the build, as it ran                    — every build system can export it
.cppls.toml               what a person knows and a build cannot  — this file
```

A server that also read *this* build system's cache and *that* one's project file would have to be taught every build
system in turn, and would still be wrong about the one nobody wrote an adapter for. The database is what they all agree
on; `.cppls.toml` is where a project says what no database can. **There is no CMake support, on purpose** — CMake's job
is to produce a compilation database, and `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON` (or
`set(CMAKE_EXPORT_COMPILE_COMMANDS ON)`) is how you ask it to.

When there is no database, three fallbacks apply in this order, each weaker than the last:

```text
1. .cppls.toml says                a person wrote the compilation down — it wins outright
2. compile_commands.json says      found beside the root, or up to three directories down (build/, out/build/linux/)
3. fuzzy include matching          an #include whose directory no search path could place, matched by the tail
                                   of its path against the files that are actually in the tree
```

Fallback 3 is **on exactly when there is no database** (the key below overrides it either way). It runs only after every
exact candidate has failed, refuses a target with no directory in it (`#include <vector>` is spelled that way because the
search path is supposed to find it), and matches the tail of the path component by component, backwards.

## 2. `.cppls.toml`

Read from the workspace root (or the file named by `--config`). Every key is optional; a section that is absent is a
section at its defaults.

### `[workspace]` — what to read at all

```toml
[workspace]
exclude = ["**/build/**", "third_party/**"]   # globs the analysis must not read
source_extensions = ["cuh", "cppm"]           # extensions *added* to the engine's own list
```

`exclude` is the same list an editor uses to hide files. `source_extensions` is **additive**: the engine's own list is
what a C/C++ project is made of, and writing this says "and also these" — a CUDA `.cuh`, a module `.cppm`. To read fewer
files, use `exclude`.

The engine's own list: `c cc cpp cxx c++ h hh hpp hxx h++ ipp inl tpp tcc cppm ixx`, plus `def mpp cuh` for headers
reached through an `#include`.

### `[compile]` — how the project is built

```toml
[compile]
database = "build/compile_commands.json"   # where the database is, when discovery would not find it
compiler = "clang++"                       # a name on PATH or a path; "auto" (or absent) leaves it to discovery
standard = "c++20"                         # the project-wide language standard, as `-std=` spells it
args = ["-std=c++20", "-Iinclude"]         # the project's **whole** command line, instead of the database's
extra_args = ["-DFEATURE=1"]               # added on top of whichever flags won
remove_args = ["-Werror"]                  # dropped from whichever flags won, matched by whole spelling
fuzzy_includes = true                      # force the tail-matching fallback on (or false to forbid it)
```

* **`args` wins outright.** When it is present the database's flags are ignored entirely: a project that writes `args`
  has said what its compilation is, and merging two sets of flags would make the result depend on their order in a way
  nobody can predict by reading the file. Use `extra_args` to add to whichever set won.
* **`remove_args` is applied before `extra_args`**, which is what makes "drop the database's `-std=c++17`, add
  `-std=c++20`" work.
* **`compiler`** decides whose headers are read. `"auto"` and absence are the same thing.
* **`standard`** is also what decides whether the file is C or C++ — see §5.

#### `[compile.include]` — extra include directories for a subtree

```toml
[compile.include]
"src/legacy" = ["legacy/include"]
"*" = ["include"]
```

A directory and the include paths it needs. **Longest matching prefix wins, and `"*"` applies everywhere** — both can be
true at once, and the nearer rule's directories are added first, because the resolver searches include paths in order.
The names may be relative to the workspace root, and absolute paths are reported as a problem.

#### `[[compile.target]]` — one compilation, when the project is several

A real project is not one compilation: it is several, each with its own include directories and its own defines, and a
single `args` list cannot describe them. Writing every target's `-I` into one list is not a simplification but a **wrong
answer**.

```toml
[[compile.target]]
name     = "CodeFormatServer"
sources  = ["CodeFormatServer/**", "CodeFormatServer/*/src/**"]
includes = ["CodeFormatServer/include", "CodeFormatServer/src", "Util/include"]
defines  = ["URI_STATIC_BUILD", "WIN32_LEAN_AND_MEAN"]
standard = "c++20"

[[compile.target]]
name     = "LuaParser"
sources  = ["LuaParser/**"]
includes = ["LuaParser/include", "LuaParser/src"]
defines  = ["LUA_COMPAT_5_3"]
```

| `.cppls.toml` | CMake | meaning |
|---|---|---|
| `name` | the target's name | what to call it in a message |
| `sources` | `add_executable`/`add_library` sources | which files it describes, **as patterns** |
| `includes` | `target_include_directories` | its include directories, relative to the root |
| `defines` | `target_compile_definitions` | its preprocessor definitions — `"NAME"` or `"NAME=value"`, no `-D` |
| `standard` | `CXX_STANDARD` | its standard; the project's `[compile].standard` is used when absent |
| `args`, `extra_args`, `remove_args` | — | the same three keys as `[compile]`, for these files |

It is **not a build system**: there are no dependencies, no sources to compile, no commands to run. It describes
compilations, which is all an analysis needs.

**Patterns.** Matched against the file's path relative to the workspace root, component by component:

```text
"CodeFormatServer/**"     the subtree, at any depth
"*/src/**"                any target's src tree
"Util/*.cpp"              direct children only
```

`**` stands for any number of components (including none), `*` for any run of characters **within** one component, and
everything else is literal. A pattern must consume the whole path unless its last component is `**`. Matching is
case-insensitive.

**Which target describes a file.** The **most specific** match wins — the one fixing the most literal components — and
ties go to the one written first. **Every** target that describes the file contributes, nearest first. A file **no**
target describes is compiled exactly as `[compile]` says. A target with no `sources` describes nothing.

#### `[compile.cuda]` — a CUDA project

```toml
[compile.cuda]
enable = true                                   # read it as CUDA even without an `nvcc` in the database
toolkit = "C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v12.4"
host_compiler = "cl"                            # the host dialect: cl (MSVC) or g++/clang++ (GNU)
include = ["third_party/cutlass/include"]       # extra include directories for CUDA units
args = ["-arch=sm_80"]                          # extra flags, appended to whatever won
```

The analysis reads a project as CUDA when its toolchain is `nvcc` (`__CUDACC__` in the macro table, or the compiler's
name); `enable` is the escape hatch for a project that has `.cu` files but no compile database. `toolkit` is what makes
`#include <cuda_runtime.h>` resolve — the toolkit installs its headers under `<toolkit>/include` (and, from CUDA 12.8, a
CCCL include beside it), and nothing on the system path knows that. `host_compiler` decides the **dialect**, which is a
different question from the CUDA flag: on Windows `nvcc` drives `cl.exe`, on Linux `g++`.

### `[index]` — what is read and remembered

```toml
[index]
cache_dir = ".cppls"     # one component, relative to the root; where cooked readings are cached
max_files = 20000        # how many files the project scan may list
```

### Read by the language server

```toml
[diagnostics]
on_change_ms = 300       # how long a file's diagnostics wait for typing to stop
enable = []              # start from this list only (an empty list means "everything")
disable = []             # never report these — wins over `enable`, and over the defaults

[hover]
enable = true            # whether hover answers at all

[format]                 # where this project's format strings are
enable = true            # the colouring, the popup and the completion — `false` is all three at once
std = true               # `{…}` — std::format / std::print / fmt::format, fmt::print
printf = true            # `%…` — printf, fprintf, snprintf, fmt::printf, fmt::sprintf
scanf = true             # `%…` with scansets — scanf, sscanf

[format.functions]       # your own, keyed by the name the analysis RESOLVES them to
"mylib::logf" = "printf"                                 # printf's syntax, format string first
"mylib::log" = "std"
"mylib::trace" = { syntax = "printf", argument = 1 }     # `trace(level, fmt, …)`
```

A wrapper has a *position*: `mylib::trace(level, fmt, …)` puts the format string second, which
`{ syntax = "printf", argument = 1 }` says and a list of names cannot. A bare string means the common case — that syntax,
the first argument. Either form is matched against the **resolved** qualified name, so `using mylib::logf;` works and a
local `logf` that is something else is not read as the wrapper.

## 3. When a key is wrong

Unknown keys fail their **whole section**, with a message, and the section keeps its defaults:

```text
[compile] could not be read, so that section keeps its defaults: unknown field `standrad`
```

A rule name in `[diagnostics]` that this build does not have is logged the same way. Silently ignoring an unknown name is
the failure mode the analysis is written against — a rule the user believes is on, that never runs.

## 4. What is not supported

| Not supported | Why |
|---|---|
| Build-system integration (CMake, Meson, …) | the database is the interface; `compile_commands.json` is what they export |
| `textDocument/onTypeFormatting` | the trigger is unreliable in practice; formatting on save and on demand covers it |
| clang-format itself | `cpp_formatter` is this project's own formatter; it reads a common subset of `.clang-format` |
| C++26 (contracts, reflection, `#embed`) | no feature bits yet; the syntax is read as C++23 |
| C23 `#embed` | not implemented |
| `references` / `rename` of ordinary names | answering them means parsing every candidate file; the server refuses rather than editing half a name's uses (macros are answered exactly) |
| Module linking | modules are parsed, indexed and resolved by name; nothing is linked or built |
| Dataflow analysis | the checks are local to the file's facts plus the index |
| Macro-expanded source mapping | hover and completion are answered from the macro environment, not from expanded text |

## 5. C and C++ in one project

The language of a file is decided by its `-std=` first (`-std=c17` → C, `-std=c++20` → C++), then by its extension;
`.h` belongs to whichever language includes it, and C++ is the wider list. A project that states no language is read
as "not told", which is not the same as C.

**Eight rules are C++ only, and are switched off for a file compiled as C** — `use-nullptr`, `use-auto`,
`use-override`, `c-style-cast`, `access-invisible`, `concept-not-satisfied`, `module-note` and
`implicit-bool-conversion`. Six of them name grammar C does not have (`use-override`, `access-invisible`,
`concept-not-satisfied`, `module-note`, `use-auto`, `use-nullptr`); the other two name C++ style that C's own idiom
contradicts — a C-style cast is the only cast C has, and an `int` in a condition is the language's idiom rather than a
mistake. Measured before the gate existed: a C corpus configured with `-std=c17` answered **14 `c-style-cast` findings**
telling the program to write `static_cast`.

The other nineteen rules are about both languages. The gate is **not** a configuration key: a file compiled as C is read
as C, and the eight rules are not opinions about it. `[diagnostics].disable` turns any rule off; nothing turns a
C++-only rule on for C.
