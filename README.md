# cpp_ls

A C++ language server written in Rust.

The design goal is **fidelity to a real compiler's reading**. A C++ file does not mean what its
characters say until a preprocessor has run, and every layer here is built around that fact rather
than around a heuristic that approximates it.

---

## The route

```
                     ┌──────────────────────────────────────────────┐
   source files ────▶│  1  discovery                                │  which compiler, which flags,
                     │     compile_commands.json, CMake, toolchain  │  where its headers are
                     └────────────────────┬─────────────────────────┘
                                          ▼
                     ┌──────────────────────────────────────────────┐
                     │  2  translation unit                         │  one walk per program:
                     │     every file, in include order, with the   │  macros, conditions and the
                     │     #if branches the conditions chose        │  include graph in one timeline
                     └────────────────────┬─────────────────────────┘
                                          ▼
                     ┌──────────────────────────────────────────────┐
                     │  3  rendering                                │  the unit written out as a
                     │     macros replaced, directives resolved     │  token stream a parser can read
                     └────────────────────┬─────────────────────────┘
                                          ▼
                     ┌──────────────────────────────────────────────┐
                     │  4  parse                                    │  a lossless syntax tree,
                     │     error-tolerant, every token kept         │  ranges mapped back to files
                     └────────────────────┬─────────────────────────┘
                                          ▼
                     ┌──────────────────────────────────────────────┐
                     │  5  index                                    │  declarations, scopes, macros,
                     │     per file, keyed by content               │  includes — cached on disk
                     └────────────────────┬─────────────────────────┘
                                          ▼
                     ┌──────────────────────────────────────────────┐
                     │  6  queries                                  │  completion, hover, definition,
                     │     answered from one session, read-locked   │  references, diagnostics, …
                     └──────────────────────────────────────────────┘
```

### 1. Discovery

The compiler is asked, not guessed. `compile_commands.json` supplies the flags when a project has
one; otherwise the project's own configuration file does, and failing that the toolchain on `PATH`
is run once with `-E -v -x c++ -` to print its search list. A project with no compiler is not an
error — the analysis then resolves the project's own headers and reports the standard library as
unresolved, which is the honest answer rather than a silently empty index.

### 2. The translation unit

A file's meaning is a **position in one walk of the program**, not a property of the file. MSVC's
standard library opens its namespaces with a macro defined in another header, and a conditional
declaration is in or out depending on a definition three includes away. So the unit is walked once —
every file entered, every definition recorded where it was written, every branch the conditions
chose taken — and every later question is a lookup in that timeline.

Measured on a 138-file project, building a macro environment per file instead of once per unit
evaluated 17.6 million conditional facts and took 147 s of a 174 s run. The walk is also cached on
disk, keyed by the content of every file it entered.

### 3. The rendering

This is the layer a compiler has and most language servers do not. The unit is written out as a
**rendering**: the token stream with macros already replaced, so that

```cpp
_STD_BEGIN                             namespace std {
struct widget { … };          ───▶     struct widget { … };
_STD_END                               }
```

The parser is given that text, and nothing else. It never sees a macro invocation, so it never has
to guess which identifier is one — which is the whole of the difference between a grammar that reads
C++ and a grammar that reads C++ plus a family of conventions for the preprocessor's leftovers.

Every token of the rendering carries where it was **written** in the file and where to **report** it,
so a position a client sends is translated into the reading and an answer is translated back. A
token a macro produced reports where the macro was invoked, which is the place the reader can see.

Measured over eight standard-library headers, the rendering reads **24% more declarations with no
parse errors at all**, where the files' own text reported between 1 and 84 errors each and recovered
from them by inventing declarations — a call inside a function body read as a declaration, a local
read as a file-scope name.

### 4. The parse

Error-tolerant and lossless: every token is in the tree, including the ones that did not fit, and a
reading is never thrown away for being wrong. The grammar follows the standard's own structure rather
than a subset of it, so a construct that fails produces a diagnostic and a recovered reading rather
than an empty result.

A missing `;` after a class definition is reported rather than absorbed, and the declarations after it
are still read. Measured, one missing semicolon in a real file took it from 8 declarations to 2 with
no diagnostic at all before this was fixed.

### 5. The index

Per file, and cached on disk under `.cppls/`, keyed by the file's content and its compilation context.
A file read twice is read once; a file whose text changed is read again. The cache has a budget and is
swept in the background when a project is opened, so a long-lived checkout does not accumulate
readings of text that no longer exists.

### 6. Queries

One `Session` holds the index, the open buffers and the work queue. A query takes a read lock and
answers from what is held; the writer reads files, builds renderings and re-indexes. Nothing blocks a
query on work that has not been done: a question about a file whose rendering is not built yet is
answered from the file's own tokens, the rendering is built for the next one, and the protocol's
`isIncomplete` tells a client to come back for it.

---

## What is deliberately not here

**Heuristic resolution where evidence exists.** The toolchain is asked for its search path rather than
assuming one. A name's type comes from its declaration rather than from its spelling. Where the
evidence is genuinely absent — a file read with no include closure, a template parameter with no
instantiation — the answer is **"not known, and here is why"** rather than a plausible guess.

**A second parser for the file's own text.** A file's own tokens are read when its rendering has not
been built yet, and that reading is a fallback; nothing is designed around it.

**Silence about what the analysis does not know.** Every answer is three-valued — yes, no, and
unknown-with-a-reason — and the reasons are specific: the name is not declared here, the macro was not
expanded, the template argument is unknown, the include was not found. A consumer can tell a missing
answer from a wrong one, which is the difference a diagnostic depends on.

---

## Layout

```
crates/
  cpp_parser/          lexer, preprocessor, grammar, syntax tree
  cpp_code_analysis/   translation units, index, scopes, types, diagnostics
  cpp_ls/              the language server: LSP transport, handlers, capabilities
```

Each module's own documentation is the reference for that module and is kept in step with its code;
`cargo doc --open` is the way to read it.

## Building

```sh
cargo build --release
```

The server speaks LSP over stdio and is started by a client rather than by hand.
