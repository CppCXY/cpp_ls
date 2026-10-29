# Configuring the analysis

There are **two** ways to tell this server how a project is built, and deliberately no third:

```text
compile_commands.json     the build, as it ran                    — every build system can export it
.cppls.toml               what a person knows and a build cannot  — this file
```

A server that also read *this* build system's cache and *that* one's project file would have to be taught every
build system in turn, and would still be wrong about the one nobody wrote an adapter for. The database is what they
all agree on; `.cppls.toml` is where a project says what no database can. **There is no CMake support, on purpose** —
CMake's job is to produce a compilation database, and
`-DCMAKE_EXPORT_COMPILE_COMMANDS=ON` (or `set(CMAKE_EXPORT_COMPILE_COMMANDS ON)`) is how you ask it to.

---

## What happens when there is no database

Three fallbacks, applied in this order, each weaker than the last:

```text
1. .cppls.toml says                     a person wrote the compilation down — it wins outright
2. compile_commands.json says           found beside the root, or up to three directories down (`build/`, `out/build/linux/`)
3. fuzzy include matching               an `#include` whose directory no search path could place, matched by the tail
                                        of its path against the files that are actually in the tree
```

`3` is what makes a **Visual Studio solution** work, and an **unconfigured checkout**. CMake cannot export a
compilation database for an MSVC solution generator, so those projects have no database and never will — without the
fallback, `#include "Session/StandardIOSession.h"` is searched for beside the including file and in MSVC's own
directories, and the file at `CodeFormatServer/include/Session/StandardIOSession.h` is never found. Measured on a real
project: a closure of **14 files instead of the whole program**.

It is **on by default exactly when there is no database**, and off when there is one (the key below overrides that
either way). It is safe by construction:

* it runs **only after every exact candidate has failed**, so it can never shadow a directory the project named;
* it refuses a target with **no directory in it** — `#include <vector>` is spelled that way because the search path
  is supposed to find it, and matching it against whatever `vector` is in the tree is the confident wrong answer;
* it matches the **tail of the path, component by component, backwards**, so it cannot be wrong about the file's
  name — only about which directory it is in, and the resolution records which one it chose.

---

## `.cppls.toml`

Read from the workspace root (or the file named by `--config`). Every key is optional; a section that is absent is a
section at its defaults. **A key that is not understood fails its whole section** — see
[When a key is wrong](#when-a-key-is-wrong).

### `[workspace]` — what to read at all

```toml
[workspace]
exclude = ["**/build/**", "third_party/**"]   # globs the analysis must not read
source_extensions = ["cuh", "cppm"]           # extensions *added* to the engine's own list
```

`exclude` is the same list an editor uses to hide files. A project that keeps sources under `build/` is unusual, not
wrong, and a server that guessed the wrong way would silently analyse nothing.

`source_extensions` is **additive**: the engine's own list is what a C++ project is made of, and writing this says
"and also these" — a CUDA `.cuh`, a module `.cppm`. To read fewer files, use `exclude`.

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

#### `[compile.include]` — extra include directories for a subtree

```toml
[compile.include]
"src/legacy" = ["legacy/include"]
"*" = ["include"]
```

The older, smaller spelling of the idea that `[[compile.target]]` generalises: a directory and the include paths it
needs. **Longest matching prefix wins, and `"*"` applies everywhere** — both can be true at once, and the nearer rule's
directories are added first, because the resolver searches include paths in order. The names may be relative to the
workspace root, and absolute paths are [reported as a problem](#when-a-key-is-wrong).

#### `[[compile.target]]` — one compilation, when the project is several

**This is the key that makes a real C++ project work.** A real project is not one compilation: it is several, each
with its own include directories and its own defines, and a single `args` list cannot describe them. Writing every
target's `-I` into one list is not a simplification but a **wrong answer** — a header reachable only from another
target's directory then resolves, and `#include "Config.h"` takes whichever `Config.h` the search order happens to
reach first.

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

The vocabulary is CMake's, reduced to the three ideas that matter, so there is nothing new to learn:

| `.cppls.toml` | CMake | meaning |
|---|---|---|
| `name` | the target's name | what to call it in a message |
| `sources` | `add_executable`/`add_library` sources | which files it describes, **as patterns** |
| `includes` | `target_include_directories` | its include directories, relative to the root |
| `defines` | `target_compile_definitions` | its preprocessor definitions — `"NAME"` or `"NAME=value"`, no `-D` |
| `standard` | `CXX_STANDARD` | its standard; the project's `[compile].standard` is used when absent |
| `args`, `extra_args`, `remove_args` | — | the same three keys as `[compile]`, for these files |

**It is not a build system.** There are no dependencies, no sources to compile, no commands to run, and nothing here
can make anything happen. It describes compilations — which is all an analysis needs and all a person should have to
write down. Anything that must be *generated* belongs in a compilation database, and that is what `[compile].database`
is for.

**Patterns.** Matched against the file's path relative to the workspace root, component by component:

```text
"CodeFormatServer/**"     the subtree, at any depth
"*/src/**"                any target's src tree
"Util/*.cpp"              direct children only
```

`**` stands for any number of components (including none), `*` for any run of characters **within** one component,
and everything else is literal. A pattern must consume the whole path unless its last component is `**`. Matching is
case-insensitive, because a path that differs only in case is the same file on the platforms this runs on.

**Which target describes a file.** The **most specific** match wins — the one fixing the most literal components —
and ties go to the one written first. So `Util/src/legacy/**` (two literal components) beats `**` (none), whichever
order the tables are in. **Every** target that describes the file contributes, nearest first, because that is what
`[compile.include]` has always documented and the two are one mechanism; patterns usually exclude each other, and
where they do not, the nearer one's directories are simply searched first.

A file **no** target describes is compiled exactly as `[compile]` says. A target with no `sources` describes nothing —
it is not a target for every file.

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

[hover]
enable = true            # whether hover answers at all

[format]
enable = true            # the colouring, the popup and the completion — `false` is all three at once
std = true               # `{…}` — std::format / std::print / fmt::format, fmt::print …
printf = true            # `%…` — printf, fprintf, snprintf, fmt::printf, fmt::sprintf …
scanf = true             # `%…` with scansets — scanf, sscanf …

[format.functions]       # our own, keyed by the name the analysis RESOLVES them to
"mylib::logf" = "printf"                                 # printf's syntax, format string first
"mylib::log" = "std"
"mylib::trace" = { syntax = "printf", argument = 1 }     # `trace(level, fmt, …)`
```

**One section for one feature**, and the default is on for every grammar: whether a string is a format string is
something the analysis can prove about the text, so it does not wait for a project to opt in — and a grammar set to
`false` costs nothing at all, because the pass answers before it looks at a token. `enable = false` turns the colour,
the popup and the completion off together; they are three faces of one question and a project that turned it off gets
none of them.

**The project's own functions are a table keyed by name** rather than a list, for one reason: a wrapper has a
*position*. `mylib::trace(level, fmt, …)` puts the format string second, which `{ syntax = "printf", argument = 1 }`
says and a list of names cannot. A bare string means the common case — that syntax, the first argument. Either form is
matched against the **resolved** qualified name, so `using mylib::logf;` works and a local `logf` that is something
else is not read as the wrapper.

The colouring sends four custom token types plus three standard fallbacks; the same rule decides what the popup
says, and what the completion offers.

---

## When a key is wrong

Two kinds of problem, and they behave differently:

```text
[compile] could not be read, so that section keeps its defaults: unknown field `standrad`
compile.include names the absolute path "C:/build/inc"; absolute paths in a committed file do not travel

the first is an **error**   — what that section said is not in effect
the second is a **warning** — the rest of the configuration is in effect
```

**An unknown key fails its whole section**, not just the key. That is deliberate — a section read halfway would be a
configuration nobody wrote — but it has a sharp edge worth knowing: a single typo leaves `[compile]` at its defaults,
and the *symptom* is a project analysed with the wrong include paths rather than a message about the typo. The message
**is** reported (the server shows configuration errors to the client), so the first thing to do when the analysis looks
wrong is to read the report.

The most common instance, measured: writing `[[compile.target]]` while the table was still called something else — the
section decoded with no problem reported and no target in effect, because the *tests'* fixture had the typo too. A
fixture that does not decode measures nothing, which is why `tests/targets.rs` now asserts that its own configuration
was understood before it asserts anything about the result.

---

## A worked example

A six-target CMake project, `E:\EmmyLuaCodeStyle`, whose MSVC solution CMake cannot export a database for:

```toml
# No `database` key: there is none to point at. The include matching below is what makes the
# project readable, and it is on by default because no database was found.
[workspace]
exclude = ["**/build*/**", "3rd/googletest-1.13.0/**"]

[compile]
standard = "c++20"
fuzzy_includes = true          # stated rather than implied, for a reader

[[compile.target]]
name     = "CodeFormatCore"
sources  = ["CodeFormatCore/**"]
includes = ["CodeFormatCore/include", "CodeFormatCore/src", "Util/include", "LuaParser/include"]
defines  = ["WIN32_LEAN_AND_MEAN"]

[[compile.target]]
name     = "CodeFormatCLib"
sources  = ["CodeFormatCLib/**"]
includes = ["CodeFormatCLib/include", "CodeFormatCore/include", "Util/include"]

[[compile.target]]
name     = "CodeFormatServer"
sources  = ["CodeFormatServer/**"]
includes = ["CodeFormatServer/include", "CodeFormatServer/src", "CodeFormatCore/include", "Util/include"]
defines  = ["URI_STATIC_BUILD"]

[[compile.target]]
name     = "LuaParser"
sources  = ["LuaParser/**"]
includes = ["LuaParser/include", "LuaParser/src", "3rd/lua-5.4.3/src"]

[[compile.target]]
name     = "Util"
sources  = ["Util/**"]
includes = ["Util/include"]

[[compile.target]]
name     = "Tests"
sources  = ["Test/**", "Test2/**"]
includes = ["CodeFormatCore/include", "Util/include", "3rd/googletest-1.13.0/googletest/include"]
defines  = ["UNIT_TEST"]
```

Third-party trees do not need targets of their own: a file no target describes falls back to `[compile]`, and the
fuzzy fallback places `#include "asio/detail/…"` from the tail of the path.

---

## Checking a configuration

The server logs what it worked out at open: the toolchain it chose, where each include path came from, whether a
database was found, and how many entries it held. On the command line, two probes answer the same questions:

```bash
# what the analysis thinks the project is, and what it can and cannot resolve
cargo run --release -p cpp_code_analysis --example workspace_probe -- <root> <file.cpp>

# how a project was discovered: its file, its database, its problems, and the flags in force
cargo run --release -p cpp_code_analysis --example discover -- <project root>
```

`discover` is the one to reach for when a configuration key seems to have no effect: it prints the discovery report,
including every problem the file's sections produced.