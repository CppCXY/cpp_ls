# Raw-reading semantic audit — the work list

Scope: every call site of the seven readings, every path by which a declaration fact enters the index, every
fallback where a **semantic** answer is served from the raw reading, and the count of what must change for the rule

> *semantic answers come ONLY from the cooked reading; the raw reading serves only lexical/editor-position needs.*

Read at `HEAD = 3070947` ("one semantic authority: the raw reading cannot mean anything"), working tree clean.
Nothing in this document is inferred from a name: every row quotes the line it is about. Where a mechanism could
not be established from the code it says so instead of guessing.

What is counted where:

* **product code** = `crates/cpp_code_analysis/src/**` and `crates/cpp_ls/src/**`;
* `tests/` and `examples/` are listed separately in the appendix. Nothing in them ships, but they pin the behaviour
  a change will break.

---

## The precedence defect

### Fact 1 — the measurement (`examples/cooked_parse.rs`)

`crates/cpp_code_analysis/examples/cooked_parse.rs` (added by `HEAD`) parses both readings and counts `ErrorNode`s:
the raw text at line 36–38, and the **whole stitched unit** — every file the walk reached, in include order,
branches taken — at line 53–60.

```rust
36:     if let Some(source) = session.text(&file) {
37:         let errors = error_nodes(&source);
38:         println!("raw reading  ({} bytes): {:>4} error node(s)", source.len(), errors.len());
...
53:     let text = unit.text.as_str();
54:     let errors = error_nodes(text);
55:     println!(
56:         "\ncooked reading ({} bytes, {} file(s) stitched): {:>4} error node(s)",
```

Measured over six MSVC headers:

| header | raw error nodes | cooked error nodes | stitched files |
|---|---|---|---|
| `<xutility>` | 84 | 0 | 77 |
| `<memory>` | 8 | 0 | — |
| `<xstring>` | 17 | 0 | — |
| `<utility>` | 12 | 0 | — |
| `<vector>` | 0 | 0 | — |
| `<type_traits>` | 0 | 0 | — |

**"The raw reading is broken" is not universal — it is roughly half these headers.** Two of the six parse clean in
both readings (`<vector>`, `<type_traits>`), so a rule that says "raw is always wrong" is falsified by this same
instrument, and a change that swaps precedence wholesale must be checked against the two clean rows.

### Fact 2 — where the precedence is decided

`crates/cpp_code_analysis/src/index/project.rs:6290` `fn visible_declarations_upto<'a>(` is the one function that
decides which reading is believed, and it believes **raw**:

```rust
6398:             let mut raw: Vec<&DeclFact> = Vec::new();
...
6411:             for fact in &raw {
6412:                 found.push(VisibleDeclaration {
...
6419:             let Some(cooked) = cooked else {
6420:                 continue;
6421:             };
...
6423:             // …and what the file was **cooked** into, minus what the raw reading already said. `(name, kind)` is
6424:             // the identity a candidate is deduplicated by — see the method's note.
6435:             for fact in candidates.filter(|fact| contributes(fact)) {
...
6448:                 if raw.iter().any(|known| {
6449:                     known.name == fact.name && known.kind == fact.kind && known.scope == fact.scope
6450:                 }) {
6451:                     continue;
6452:                 }
```

Every raw fact is pushed into `found` **unconditionally** (6411–6417). A cooked fact is added only after the raw
list, and only if no raw fact of the same identity exists (6448–6452). So for a file that has both readings, the
raw reading is the authority and the cooked reading may only supplement it.

### Correction to `docs/one-semantic-authority.md` §2 — the identity is `(name, kind, scope)`, not `(name, kind)`

`docs/one-semantic-authority.md:90` says *"The dedup identity is already `(name, kind)`; it becomes 'a cooked fact
suppresses the raw fact of the same identity' rather than the reverse."* That is **not what the code at 6290 does
today**:

* at `project.rs:6449` the identity is `known.name == fact.name && known.kind == fact.kind && known.scope == fact.scope`
  — three fields, and the comment immediately above it (6436–6447) records that `(name, kind)` was **replaced**
  *because* it dropped the one fact a query asks for:
  `6443: // Measured: std::string::size() and std::basic_string<char>::size() both answered`
  `6444: // NotDeclaredHere **while the fact was in the index**, because the raw twin had won the`
  `6445: // deduplication and its scope was basic_string.`
* the two-field identity `(name, kind)` **does** still exist, but in the *other* union: `project.rs:6001`
  `.any(|(_, raw)| raw.kind == fact.kind);` inside `fn hits_in` (5976), which serves `symbols_matching`
  (workspace symbol, 5864).

So the defect is one rule in two places with two different identities, and a change written against "`(name, kind)`"
will not match line 6448. This is the single most important correction in this document.

### What the audit did **not** establish

The mechanism by which `std::basic_string` is *absent* from the index was not traced here. Under the 6448 rule a
cooked fact survives unless a raw fact with the same `(name, kind, scope)` exists, so "raw suppresses it" needs a
raw twin to be present — and `every_fact_named` (6222) has a slot-aliasing path that can return a *raw* fact for a
*cooked* posting (see §2, path U-C) which was not ruled in or out. Stated as unresolved rather than guessed at.

---

## 1. Every entry point and every call site

Reading key:

```text
cooked        FileView::parse_rendering — macros expanded, the text a compiler parses
raw           FileView::parse — the file's own tokens, no macro expansion
raw+macros    FileView::parse_with — the file's own text, but the parse and the scope walk are
              given the include closure's macro bodies. Neither reading; a third one
```

Consumer class:

```text
(P)  producer     — chooses the reading for other callers; not a consumer itself
(a)  semantic     — feeds declarations, types, scopes, members, resolution, references, rename, index facts
(b)  lexical / editor-position — positions, text, brackets, macro editing, diagnostics message spans
```

### 1.1 `Session::view` — `session.rs:2357`

```rust
2372:         match self.known_rendering_of(&file.path, &file.text) {
2373:             Some(rendered) => Some(FileView::parse_rendering(file, &rendered)),
2374:             None => {
2375:                 if let Ok(mut work) = self.macro_work.lock() {
2376:                     work.want(&file.path);
2377:                 }
2378:                 Some(FileView::parse(file))
```

**cached → cooked. not cached → raw** (and it queues a rendering request on `macro_work`, built by the next drain —
`session.rs:1891–1901`, `1934–1946`, `1952–1957`). The reading therefore *changes between two identical requests*.

| # | call site | line quoted | cached | not cached | class |
|---|---|---|---|---|---|
| 1 | [session.rs:2749](crates/cpp_code_analysis/src/session.rs#L2749) | `self.view(path)` | cooked | raw | (a) — the tree `checks_about`/`semantic_model` run over, in `diagnostics`' *cooked* branch |
| 2 | [session.rs:2782](crates/cpp_code_analysis/src/session.rs#L2782) | `self.view(path)` | cooked | raw | (a) — same, in `diagnostics`' *raw* branch |
| 3 | [session.rs:3951](crates/cpp_code_analysis/src/session.rs#L3951) | `&mut \|path\| self.view(path),` | cooked | raw | (a) — `definition_across_files` resolver |
| 4 | [session.rs:3967](crates/cpp_code_analysis/src/session.rs#L3967) | `&mut \|path\| self.view(path),` | cooked | raw | (a) — `definitions_across_files` resolver |
| 5 | [session.rs:4077](crates/cpp_code_analysis/src/session.rs#L4077) | `&mut \|path\| self.view(path),` | cooked | raw | (a) — `member_completions_at` resolver |
| 6 | [session.rs:4220](crates/cpp_code_analysis/src/session.rs#L4220) | `&mut \|path\| self.view(path),` | cooked | raw | (a) — `type_of_expression` resolver |
| 7 | [session.rs:4304](crates/cpp_code_analysis/src/session.rs#L4304) | `crate::signature::signatures_at(self.store.index(), view, offset, \|path\| self.view(path));` | cooked | raw | (a) — `signatures_at` resolver |
| 8 | [hover/mod.rs:121](crates/cpp_ls/src/handlers/hover/mod.rs#L121) | `let view = session.view(&path)?;` | cooked | raw | (a) — `definition`/`definitions`/`type_at` for the popup |
| 9 | [signature_help/mod.rs:68](crates/cpp_ls/src/handlers/signature_help/mod.rs#L68) | `let view = session.view(&path)?;` | cooked | raw | (a) — `signatures_at` |
| 10 | [selection_range/mod.rs:47](crates/cpp_ls/src/handlers/selection_range/mod.rs#L47) | `let view = session.view(&path)?;` | **cooked** | raw | (b) — `view.selection_chain(offset)`, then mapped with `position_in_file(held, …)` (line 75) |
| 11 | [folding_range/mod.rs:48](crates/cpp_ls/src/handlers/folding_range/mod.rs#L48) | `let view = session.view(&path)?;` | **cooked** | raw | (b) — `folding_ranges(&view.source, view.tree.get_tokens())`, then `position_in_file(held, …)` (line 70) |
| 12 | [document_symbol/mod.rs:57](crates/cpp_ls/src/handlers/document_symbol/mod.rs#L57) | `let view = session.view(&path)?;` | **cooked** | raw | (b) — outline, see §1.9 for why this one is a coordinate defect |
| 13 | [completion/mod.rs:427](crates/cpp_ls/src/handlers/completion/mod.rs#L427) | `let view = session.view(&file)?;` | **cooked** | raw | (b) — `session.documentation(&view, &file, offset)` for a completion item |

13 call sites: 9 × (a), 4 × (b).

### 1.2 `Session::view_of_the_file` — `session.rs:2401`

```rust
2401:     pub fn view_of_the_file(&self, path: impl AsRef<Path>) -> Option<FileView> {
2402:         Some(FileView::parse(self.vfs.held(path)?))
```

**cached → raw. not cached → raw.** The rendering is not consulted at all — this is not a fallback, it is the raw
reading *always*. Its own note (2383–2400) says a rendering has the macro already replaced, "so `#define API …`,
every `API` written below it, and the node a rename would edit are all *gone* from it" — a claim about macro
editing, which is true, but the same function is what the semantic handlers call.

| # | call site | line quoted | cached | not cached | class |
|---|---|---|---|---|---|
| 1 | [session.rs:2890](crates/cpp_code_analysis/src/session.rs#L2890) | `let resolver = Box::new(move \|path: &Path\| -> Option<FileView> { session.view_of_the_file(path) });` | raw | raw | (a) — **every** file `SemanticModel` reaches for a type |
| 2 | [session.rs:4257](crates/cpp_code_analysis/src/session.rs#L4257) | `self.view_of_the_file(path)` | raw | raw | (a) — the callee's declaration for `parameter_hints` |
| 3 | [definition/mod.rs:102](crates/cpp_ls/src/handlers/definition/mod.rs#L102) | `let view = session.view_of_the_file(&path)?;` | raw | raw | (a) — `session.definitions(&view, offset)` at line 122 |
| 4 | [completion/mod.rs:157](crates/cpp_ls/src/handlers/completion/mod.rs#L157) | `let view = session.view_of_the_file(&path)?;` | raw | raw | (a) — `session.completions(&view, offset)` at line 162 (uses `view.scopes`, `view.root`) |
| 5 | [references/mod.rs:75](crates/cpp_ls/src/handlers/references/mod.rs#L75) | `let written = session.view_of_the_file(&path)?;` | raw | raw | (a) — `session.symbol_references(&written, …)` at line 96 |
| 6 | [rename/mod.rs:74](crates/cpp_ls/src/handlers/rename/mod.rs#L74) | `let view = session.view_of_the_file(&path)?;` | raw | raw | (a) — `prepareRename`; `macro_at(session, &view, offset)` |
| 7 | [rename/mod.rs:101](crates/cpp_ls/src/handlers/rename/mod.rs#L101) | `let view = session.view_of_the_file(&path)?;` | raw | raw | (a) — `session.macro_references(view, offset)` at line 139 |
| 8 | [semantic_token/mod.rs:205](crates/cpp_ls/src/handlers/semantic_token/mod.rs#L205) | `let view = session.view_of_the_file(&path)?;` | raw | raw | (a) — `session.classified_names(&view)` at line 220 (reads `view.scopes` + the index) |
| 9 | [inlay_hint/mod.rs:93](crates/cpp_ls/src/handlers/inlay_hint/mod.rs#L93) | `let view = session.view_of_the_file(&path)?;` | raw | raw | (a) — `session.inlay_hints(&view, …)` at line 108 |
| 10 | [hover/mod.rs:105](crates/cpp_ls/src/handlers/hover/mod.rs#L105) | `let written = session.view_of_the_file(&path)?;` | raw | raw | (b) — `header_at` / `macro_definition`, and the position is taken with it (line 106) |

10 call sites: 9 × (a), 1 × (b).

### 1.3 `Session::view_of_the_file_with_macros` — `session.rs:2411`

```rust
2413:         match self.known_rendering_of(&file.path, &file.text) {
2414:             Some(rendered) => Some(FileView::parse_rendering(file, &rendered)),
2415:             None => match self.known_macros_of(&file.path, &file.text) {
2416:                 Some(macros) => Some(FileView::parse_with(file, &macros)),
2417:                 None => Some(FileView::parse(file)),
```

**cached → cooked. not cached → `raw+macros` if an environment is cached, else raw.** Three readings in one
function.

**Call sites in product code: none.** (`grep` over `crates/**/src` finds only this definition and its own doc
links.) It is dead product code that a change should delete rather than migrate.

### 1.4 `Session::view_of_the_rendering` — `session.rs:2449`

```rust
2449:     pub fn view_of_the_rendering(&mut self, path: impl AsRef<Path>) -> Option<FileView> {
2450:         let file = self.vfs.held(path)?.clone();
2451:         let rendered = self.rendering_of(&file.path)?;
2452:         // Remembered, so the next `view` — which cannot build one — finds it.
2453:         self.known_rendering_of(&file.path, &file.text);
2454:         Some(FileView::parse_rendering(&file, &rendered))
```

**cooked, always** (it builds the rendering if there is none). **Call sites in product code: none** — two examples
only (`examples/types_probe.rs:133`, `examples/coordinates_probe.rs:65`).

Note for the work list: line 2453 does **not** remember anything. `known_rendering_of` (2458–2465) is a pure
getter:

```rust
2458:     fn known_rendering_of(
...
2463:         let key = (path.to_path_buf(), crate::cache::content_hash(text));
2464:         self.renderings.lock().ok()?.get(&key).cloned()
```

The comment is false; only `commit_the_drain` (`session.rs:1952–1957`) writes `self.renderings`. This matters
because it means a rendering exists in the cache **only** for paths `Session::view` asked for — the population of
the cache that decides whether rows 1.1 get cooked is `macro_work.want` at 2376.

### 1.5 `Session::view_with_macros` — `session.rs:2498`

```rust
2500:         match self.the_macros_of(&file.path, &file.text) {
2501:             Some(macros) => Some(FileView::parse_with(file, &macros)),
2502:             None => Some(FileView::parse(file)),
```

**Never the rendering.** cached environment → `raw+macros`; not cached → raw. **Call sites in product code: none**
(1 test, 3 example calls).

### 1.6 `FileView::parse` — `view.rs:254` (producer)

```rust
254:     pub fn parse(file: &VfsFile) -> FileView {
...
262:             cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default())
...
267:             crate::sema::scopes::build_scopes(&root, &crate::sema::scopes::NoMacroBodies)
```

Always **raw**, and the scope walk is explicitly given `NoMacroBodies` (the note at 247–253 calls this "an answer
rather than a gap"). Call sites in product code: 4 — `session.rs:2378`, `2402`, `2417`, `2502` (all four inside
§1.1–§1.3). Class (P).

### 1.7 `FileView::parse_with` — `view.rs:324` (producer)

```rust
326:         let config = cpp_parser::ParserConfig::default().with_macros_from_includes(macros);
327:         let tree = cpp_parser::CppParser::parse(&source, config);
...
329:         let scopes = crate::sema::scopes::build_scopes(&root, macros);
```

**`raw+macros`** — the file's own text (so still unexpanded and still not a program), read with the closure's macro
bodies. This is the only place in the crate where `with_macros_from_includes` is called: the doc on
`FileIndexer::macro_facts` (`index/mod.rs:99–107`) claims the indexer's parse does the same, and it does not — see
§2, note R1. Call sites in product code: 2 — `session.rs:2416`, `2501`, both in functions with zero callers.
Class (P).

### 1.8 `FileView::parse_rendering` — `view.rs:363` (producer)

```rust
367:         let source: Arc<str> = Arc::from(rendered.text.as_str());
368:         let line_index = Arc::new(LineIndex::parse(&source));
369:         let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
...
373:         let scopes = crate::sema::scopes::build_scopes(&root, &crate::sema::scopes::NoMacroBodies);
```

Always **cooked**, and the offsets are the rendering's (`written: Some(file.text.clone())` at 384,
`reading: Arc::from(rendered.spans.as_slice())` at 386). Call sites in product code: 3 — `session.rs:2373`,
`2414`, `2454`. Class (P).

### 1.9 Two sites where the reading and the coordinates disagree (found while classifying)

These are not fallbacks; they are places where §1's table says a consumer gets a reading whose **offsets are not
the ones the next line assumes**. They are what a swap of precedence will make worse before it makes better, so
they must be in the work list.

**(a) `document_symbol` — a raw-fact outline mapped through a rendering view.**
`session.view(&path)` at `document_symbol/mod.rs:57` is cooked whenever a rendering is cached (§1.1). Then:

```rust
58:         let outline = session.outline(&view);
```

and `Session::outline` returns the **raw summary's** facts whenever a summary exists:

```rust
4461:     pub fn outline(&self, view: &FileView) -> Vec<OutlineSymbol> {
4462:         if let Some(summary) = self.store.index().summary(&view.path) {
4463:             return summary.outline();
4464:         }
```

`FileSummary::outline` is raw by design (`summary.rs:482`, and its note at 453–462: *"The **raw** reading,
deliberately."*). Those facts carry **file** offsets. The handler then maps them through the view:

```rust
118: fn range_in_file(view: &FileView, range: cpp_parser::SourceRange) -> Option<Range> {
119:     Some(Range::new(
120:         position_at_offset(view, range.start_offset)?,
```

and `position_at_offset` (`cpp_ls/src/util/position.rs:63–70`) treats its argument as a **rendering** offset and
sends it through `view.file_offset_of`. So a file offset is looked up in the span table of the rendering. It is
either placed at the wrong position or, past the last span, answers `None` → `Range::default()` (0,0) at
`document_symbol/mod.rs:91`. Reproducible once the rendering is cached — i.e. from the first `documentSymbol`
request *after* the drain that builds it, since that request is what queues the rendering
(`session.rs:2376` → `macro_work` → `1891–1901` → `1934–1946` → `1952–1957`).

**(b) `selection_range`, `folding_range` — a rendering view mapped through the file's line index.**
`selection_range/mod.rs:53` builds the chain in the view's reading, then line 75–76 places it with the VFS file:

```rust
53:             let chain = view.selection_chain(offset);
...
75:         let start = position_in_file(file, range.start_offset)?;
```

Same shape in `folding_range/mod.rs`: `session.folding_ranges(&view)` (line 53) is
`crate::folding::folding_ranges(&view.source, view.tree.get_tokens())` (`session.rs:4283`, the rendering's text
and tokens), and `position_in_file(file, fold.range.start_offset)` (line 70) is the file's index. Both are
correct exactly when the view is the file's own — which is what the raw reading is for. `completion/mod.rs:427`
has the same shape: `session.documentation(&view, &file, offset)` with a **file** offset
(`completion/mod.rs:427–428`) against a possibly-rendering view, and `Session::documentation` walks `view.root`
with it (`session.rs:4179–4181`).

---

## 2. Every path by which a declaration fact enters the index

There are exactly **two** stores and **three** insert functions. `ProjectIndex` keeps two maps
(`index/project.rs:702` `summaries`, `751` `cooked`), and the name index keeps both under one key with a flag bit:

```rust
names.rs:39: const COOKED: u32 = 1 << 31;
names.rs:51:         let slot = index as u32 | if cooked { COOKED } else { 0 };
```

so one name maps to two authorities by construction, and one file can hold two `basic_string`s.

### Fact construction — the four places in product code that call `build_scopes`/`build_facts`

| # | site | line quoted | reading of the tree |
|---|---|---|---|
| F1 | [index/mod.rs:653](crates/cpp_code_analysis/src/index/mod.rs#L653) | `build_scopes(&root, evidence)` | **depends on the caller of `index_tree`** — raw from `index` (§R1), cooked from `index_rendering` (§C1), cooked from `index_unit_rendering` (§C2) |
| F2 | [index/mod.rs:666](crates/cpp_code_analysis/src/index/mod.rs#L666) | `build_facts(&scopes, &preprocessing, &root, &errors)` | same — this is the one join both readings go through |
| F3 | [session.rs:4473](crates/cpp_code_analysis/src/session.rs#L4473) | `let (facts, _guards) = crate::build_facts(&view.scopes, &preprocessing, &view.root, &errors);` | **raw or cooked**, whatever `Session::view` returned to the caller — and note the branch above it (4462) is raw. Not inserted into the index; the facts go to `outline_of` (4475) |
| F4 | [view.rs:267](crates/cpp_code_analysis/src/file/view.rs#L267) / [view.rs:329](crates/cpp_code_analysis/src/file/view.rs#L329) / [view.rs:373](crates/cpp_code_analysis/src/file/view.rs#L373) | `build_scopes(&root, &crate::sema::scopes::NoMacroBodies)` / `build_scopes(&root, macros)` / `build_scopes(&root, &crate::sema::scopes::NoMacroBodies)` | raw / **raw+macros** / cooked — the scope tree a query is answered against, not an index insert |

`build_facts` itself is `sema/declarations.rs:68`; `build_scopes` is `sema/scopes.rs:75`; both are re-exported at
`lib.rs:98` and `lib.rs:196`.

`index_tree` (`index/mod.rs:614`) is the shared join, and the *errors* it feeds `build_facts` come from the tree it
was handed — `index/mod.rs:658–662` `tree.get_errors()` — which is how `DeclFact::clean`
(`sema/declarations.rs:1966`) becomes a per-reading claim.

### Paths that INSERT a fact

**RAW paths.**

| # | path | lines | reading |
|---|---|---|---|
| R1 | `SummaryStore::prepare_inner` → `FileIndexer::index` → `index_tree` → `build_scopes` + `build_facts` | [store.rs:584](crates/cpp_code_analysis/src/index/store.rs#L584) `let summary = indexer.index(path, &source, key);` → [index/mod.rs:271](crates/cpp_code_analysis/src/index/mod.rs#L271) `pub fn index(&self, path: &Path, source: &str, key: SummaryKey) -> FileSummary {` → [index/mod.rs:303](crates/cpp_code_analysis/src/index/mod.rs#L303) `self.index_tree(path, source, &tree, key)` | **raw** — `source` is the file's own text, parsed at `index/mod.rs:299` `CppParser::parse(source, config)`. The scope walk *may* get macro bodies ([index/mod.rs:647](crates/cpp_code_analysis/src/index/mod.rs#L647) `let evidence: &dyn cpp_parser::MacroBodies = match self.bodies {`), the **parse never does** |
| R1a | …inserted by `SummaryStore::commit` | [store.rs:610](crates/cpp_code_analysis/src/index/store.rs#L610) `self.index.insert_at(path, stored);` / [store.rs:620](crates/cpp_code_analysis/src/index/store.rs#L620) `self.index.insert(summary);` → [project.rs:5603](crates/cpp_code_analysis/src/index/project.rs#L5603) / [5586](crates/cpp_code_analysis/src/index/project.rs#L5586) → [project.rs:5691](crates/cpp_code_analysis/src/index/project.rs#L5691) `self.names.add_declarations(sequence, false, &summary.declarations);` | raw, filed as the **non-cooked** authority (`cooked: false`) |
| R1b | entry points into R1: `prepare` ([store.rs:459](crates/cpp_code_analysis/src/index/store.rs#L459)), `prepare_with_the_environment` ([store.rs:493](crates/cpp_code_analysis/src/index/store.rs#L493)), `prepare_telling` ([store.rs:509](crates/cpp_code_analysis/src/index/store.rs#L509)) via `prepare_closure`/`prepare_many` ([store.rs:652](crates/cpp_code_analysis/src/index/store.rs#L652), [store.rs:776](crates/cpp_code_analysis/src/index/store.rs#L776)) | — | raw |
| R2 | disk cache hit — `read_summary` inside `prepare_inner`, `PreparedOutcome::Stored` → the same `commit` | [store.rs:535](crates/cpp_code_analysis/src/index/store.rs#L535) `if let Ok(stored) = read_summary(&key.path_under(&self.cache))` | raw facts written by an earlier run (`index/mod.rs:1444` `write_summary`, `index/mod.rs:1471` `read_summary`, codec in `summary_codec.rs`) |
| R3 | second pass — `SummaryStore::prepare_the_re_read` → `FileIndexer::index` **with macro bodies** | [store.rs:1285](crates/cpp_code_analysis/src/index/store.rs#L1285) `let made = indexer.with_macro_bodies(&environment).index(path, source, key);` and [store.rs:1313–1317](crates/cpp_code_analysis/src/index/store.rs#L1313) `.with_macro_bodies(&view).without_these_macros(&dead).index(path, source, key);` | **raw text again**, macro-informed scopes. The comment at 1306 says it outright: *"The summary is built from the raw reading and would keep it"* |
| R3a | …inserted by `commit_the_re_read` → `commit_reread` → `ProjectIndex::insert` | [store.rs:951](crates/cpp_code_analysis/src/index/store.rs#L951) `pub fn commit_the_re_read(&mut self, plan: ReRead) -> usize {` → [store.rs:1338](crates/cpp_code_analysis/src/index/store.rs#L1338) `self.index.insert(rebuilt);` | replaces the file's **raw** summary (`insert` → `insert_at` → 5691, `cooked: false`) |
| R4 | `Session::index_one` → `prepare_with_the_environment` → R1 | [session.rs:2132](crates/cpp_code_analysis/src/session.rs#L2132) `let prepared = self.store.prepare_with_the_environment(&path, view, view);` | **raw text**, with the unit's macro bodies for the scope walk |
| R5 | `Session::outline` fallback → `build_facts` | [session.rs:4473](crates/cpp_code_analysis/src/session.rs#L4473) | **raw or cooked** (the caller's view). Produces `OutlineSymbol`s, **not** an index insert — but it is the one place outside `index_tree` that manufactures `DeclFact`s, so it is a fact source for a semantic-looking consumer |
| R6 | `index::summarize` (public helper) → `FileIndexer::index` | [index/mod.rs:925](crates/cpp_code_analysis/src/index/mod.rs#L925) `FileIndexer::new(&NoFiles, &CompilerConfig::default()).index(path, source, key)` | raw. No `src` caller; tests and other crates' fixtures |

**COOKED paths.**

| # | path | lines | reading |
|---|---|---|---|
| C1 | `Session::render_a_cooked` → `FileIndexer::index_rendering` → `index_tree` (rendering) → `map_into_the_file` | [session.rs:3731](crates/cpp_code_analysis/src/session.rs#L3731) `let indexed = indexer.index_rendering(path, &rendered, *key);` → [index/mod.rs:389](crates/cpp_code_analysis/src/index/mod.rs#L389) → [index/mod.rs:406](crates/cpp_code_analysis/src/index/mod.rs#L406) `self.index_tree(path, &rendered.text, &tree, key)` | **cooked** — `CppParser::parse(&rendered.text, config)` at `index/mod.rs:402`, and `index/mod.rs:398` deliberately passes **no** macro environment |
| C1a | …inserted by `Session::commit_a_cooked` → `ProjectIndex::insert_cooked` | [session.rs:3753](crates/cpp_code_analysis/src/session.rs#L3753) `self.store.index_mut().insert_cooked(&path, indexed.into());` → [project.rs:5779](crates/cpp_code_analysis/src/index/project.rs#L5779) → [project.rs:5796](crates/cpp_code_analysis/src/index/project.rs#L5796) `self.names.add_declarations(sequence, true, &reading.declarations);` | cooked, filed as the **cooked** authority (`cooked: true`) |
| C1b | entry points into C1: `Session::cook` ([session.rs:3632](crates/cpp_code_analysis/src/session.rs#L3632)), `Session::render_them` + `commit_them` ([session.rs:3784](crates/cpp_code_analysis/src/session.rs#L3784), [3795](crates/cpp_code_analysis/src/session.rs#L3795)) — the pump's path, [initialized/mod.rs:390](crates/cpp_ls/src/handlers/initialized/mod.rs#L390) `session.render_them(materials),` and [initialized/mod.rs:405](crates/cpp_ls/src/handlers/initialized/mod.rs#L405) `let cooked = session.commit_them(rendered_cooks).len();` | — | cooked |
| C2 | `Session::read_the_unit` → `FileIndexer::index_unit_rendering` — one parse of the **whole program's** rendering, split back by file | [session.rs:3340](crates/cpp_code_analysis/src/session.rs#L3340) `let indexed = indexer.index_unit_rendering(&root, &stream, key);` → [index/mod.rs:522](crates/cpp_code_analysis/src/index/mod.rs#L522) → [index/mod.rs:579](crates/cpp_code_analysis/src/index/mod.rs#L579) `self.index_tree(root, &stream.text, &tree, key)` | **cooked** (the unit stream). `file_what_was_found` at `index/mod.rs:590` splits the facts by the file each was written in |
| C2a | …inserted per file | [session.rs:3359–3361](crates/cpp_code_analysis/src/session.rs#L3359) `for (path, cooked) in indexed.files {` / `self.store.index_mut().insert_cooked(&path, cooked);` | cooked. Caller: `Session::read_a_unit_of_the_project` (`session.rs:3384`). **Not wired into the pump** — the call site at [session.rs:1851](crates/cpp_code_analysis/src/session.rs#L1851) is commented out `// self.read_a_looked_at_unit();` |

### Where the two authorities meet — and what survives the suppression rule

Every read of a declaration goes through one of these. The dedup column is what the parent asked for: does a fact
from this path survive when a raw fact of the same identity exists?

| # | site | line quoted | identity rule | does the fact survive a raw twin? |
|---|---|---|---|---|
| U-A | `ProjectIndex::visible_declarations_upto` — the union behind `definition`, `definitions`, `declarations_in`, `files_declaring`, `members_of`, member/name completions | [project.rs:6290](crates/cpp_code_analysis/src/index/project.rs#L6290); raw pushed at [6411–6417](crates/cpp_code_analysis/src/index/project.rs#L6411); cooked added at [6435](crates/cpp_code_analysis/src/index/project.rs#L6435) | **`(name, kind, scope)`** — [6448–6450](crates/cpp_code_analysis/src/index/project.rs#L6448) `known.name == fact.name && known.kind == fact.kind && known.scope == fact.scope` | **raw survives always** (pushed first, unconditionally). **Cooked is dropped** iff a raw fact in the same file has the same `(name, kind, scope)`; it **survives** when raw has the same `(name, kind)` but a **different scope** — which is exactly the `std::basic_ios` vs file-scope `basic_ios` case, both answers in one list |
| U-B | `ProjectIndex::hits_in` — the union behind `symbols_matching` (workspace symbol) | [project.rs:5976](crates/cpp_code_analysis/src/index/project.rs#L5976); [5994–6005](crates/cpp_code_analysis/src/index/project.rs#L5994) `if posting.is_cooked() {` … `.any(\|(_, raw)\| raw.kind == fact.kind);` | **`(name, kind)`** — two fields | **Cooked is dropped** when a raw twin has the same name and kind *even if the scope differs* — the stricter, older rule, still live here |
| U-C | `ProjectIndex::every_fact_named` | [project.rs:6222](crates/cpp_code_analysis/src/index/project.rs#L6222); [6231](crates/cpp_code_analysis/src/index/project.rs#L6231) `if let Some(fact) = summary.declarations.get(posting.index())` then [6237](crates/cpp_code_analysis/src/index/project.rs#L6237) `if let Some(cooked) = self.cooked.get(key)` | **none — and the lookup is by slot, not by the posting's own flag** | A **cooked** posting is first resolved against the **raw** list at the same index; if that raw fact's `name == name` it is pushed and the loop `continue`s, so a cooked fact can be silently answered with a raw one that merely occupies the same slot. It survives only when the raw fact at that slot has a different name (or the raw list is shorter). **Unverified which callers hit this**; the shape is quoted so the change does not preserve it |
| U-D | `NameIndex` storage — the two-authority table itself | [names.rs:39](crates/cpp_code_analysis/src/index/names.rs#L39) `const COOKED: u32 = 1 << 31;`; [names.rs:80](crates/cpp_code_analysis/src/index/names.rs#L80) `pub fn add_declarations(&mut self, file: u32, cooked: bool, facts: &[DeclFact]) {` | one list per name, raw slots before cooked slots | n/a — this is *why* a raw and a cooked fact of one name coexist |
| U-E | `ProjectIndex::resolve` — a posting to its fact | [project.rs:6031](crates/cpp_code_analysis/src/index/project.rs#L6031); [6034–6038](crates/cpp_code_analysis/src/index/project.rs#L6034) | the posting's own `is_cooked` bit | exact: this one cannot confuse the two. The reason U-C can is that it does not use it |
| U-F | `FileSummary::outline` | [summary.rs:482](crates/cpp_code_analysis/src/summary.rs#L482) `outline_of(&self.declarations)` | raw only, by design | n/a — cooked facts are never consulted; the raw reading is the *only* source, which is what §1.9(a) then mismatches against a rendering view |
| U-G | `ProjectIndex::cooked_declarations` / `cooked_reading` | [project.rs:5802](crates/cpp_code_analysis/src/index/project.rs#L5802), [5812](crates/cpp_code_analysis/src/index/project.rs#L5812) | cooked map only | n/a — used by `Session::diagnostics` (2737), `is_ready_for_a_request_about` (1484), `want_everything_cooked` (1504) |
| U-H | invalidation of each authority | `forget` [project.rs:5726](crates/cpp_code_analysis/src/index/project.rs#L5726) `if let Some(cooked) = self.cooked.remove(&path)`; `forget_cooked` [5826](crates/cpp_code_analysis/src/index/project.rs#L5826); `insert_at` [5643–5646](crates/cpp_code_analysis/src/index/project.rs#L5643) `if previous.key != summary.key` | — | A raw re-index **takes the cooked reading with it** only when the key moved; `forget_cooked` drops cooked and keeps raw. So the pair can be of different ages, and `insert_cooked` replaces only cooked ([5793–5798](crates/cpp_code_analysis/src/index/project.rs#L5793)) |

Note **R1**: the `FileIndexer` doc says the parse is given the environment —
[index/mod.rs:94–97](crates/cpp_code_analysis/src/index/mod.rs#L94) *"the *parse* reads it through
`ParserConfig::with_macros_from_includes`"* — and the field is written at
[index/mod.rs:186–187](crates/cpp_code_analysis/src/index/mod.rs#L186) and
[205–206](crates/cpp_code_analysis/src/index/mod.rs#L205). It is **never read**: `grep` over the crate finds no
`self.macro_facts` use, and `index/mod.rs:295` builds the config as
`ParserConfig::default().with_dialect(self.config.dialect())`. The raw summary's *parse* therefore sees no macro
bodies; only its *scope walk* does (`index/mod.rs:647`, `653`). The one place that really does set it is
`FileView::parse_with` (`view.rs:326`), which has no product caller. So there are **three** readings, not two, and
the third is documented as the second.

---

## 3. Fallback sites — "if no rendering, use the raw reading for a SEMANTIC answer"

The three named sites are confirmed, and they are three of **twenty**. Every row below answers a semantic question
from a tree the compiler never parsed, or hands a semantic consumer a reading that may be raw.

| # | site | line quoted | what makes it semantic |
|---|---|---|---|
| 1 | [session.rs:2372–2379](crates/cpp_code_analysis/src/session.rs#L2372) — `Session::view` | `2378: Some(FileView::parse(file))` | **the master fallback.** Every §1.1 row inherits it: 9 semantic call sites get a raw tree whenever no rendering is cached, and a *different* tree when one is |
| 2 | [session.rs:2413–2418](crates/cpp_code_analysis/src/session.rs#L2413) — `view_of_the_file_with_macros` | `2417: None => Some(FileView::parse(file)),` | same shape, one level deeper: `parse_with` at 2416 is also not the compiler's reading |
| 3 | [session.rs:2500–2503](crates/cpp_code_analysis/src/session.rs#L2500) — `view_with_macros` | `2502: None => Some(FileView::parse(file)),` | same. 0 product call sites |
| 4 | [session.rs:2402](crates/cpp_code_analysis/src/session.rs#L2402) — `view_of_the_file` | `Some(FileView::parse(self.vfs.held(path)?))` | **not a fallback — an unconditional raw answer**, and 9 semantic call sites use it (§1.2). This is the largest single block of the change |
| 5 | [session.rs:2737](crates/cpp_code_analysis/src/session.rs#L2737) / [2780–2819](crates/cpp_code_analysis/src/session.rs#L2780) — `Session::diagnostics` | `2737: if let Some(cooked) = self.store.index().cooked_reading(&key) {` … `2798: Some(FileDiagnostics {` / `2799: reading: DiagnosticReading::Raw,` | the raw branch publishes the raw parse's errors **and** runs the language checks over the raw view: `2817: self.checks_about(&key, &view)` → `2830` → `2843: let model = self.semantic_model(view);` |
| 6 | [session.rs:2749](crates/cpp_code_analysis/src/session.rs#L2749) | `self.view(path)` | the *cooked* branch of `diagnostics` still asks for a view, and it is raw whenever no rendering is cached — while the index already holds a cooked reading for the file. The checks and the module notes run over it (2753, 2758) |
| 7 | [session.rs:2890](crates/cpp_code_analysis/src/session.rs#L2890) — `semantic_model`'s resolver | `let resolver = Box::new(move \|path: &Path\| -> Option<FileView> { session.view_of_the_file(path) });` | raw by construction, and this resolver is what answers **every** other file's types for the checks and the queries |
| 8 | [session.rs:3951](crates/cpp_code_analysis/src/session.rs#L3951) — `definition` | `&mut \|path\| self.view(path),` | resolution |
| 9 | [session.rs:3967](crates/cpp_code_analysis/src/session.rs#L3967) — `definitions` | `&mut \|path\| self.view(path),` | resolution |
| 10 | [session.rs:4077](crates/cpp_code_analysis/src/session.rs#L4077) — `member_completions` | `&mut \|path\| self.view(path),` | members, types |
| 11 | [session.rs:4220](crates/cpp_code_analysis/src/session.rs#L4220) — `type_at` | `&mut \|path\| self.view(path),` | types |
| 12 | [session.rs:4304](crates/cpp_code_analysis/src/session.rs#L4304) — `signatures_at` | `crate::signature::signatures_at(self.store.index(), view, offset, \|path\| self.view(path));` | declarations |
| 13 | [session.rs:4257](crates/cpp_code_analysis/src/session.rs#L4257) — `inlay_hints`' resolver | `self.view_of_the_file(path)` | the callee's declaration and its parameter list |
| 14 | [session.rs:4461–4476](crates/cpp_code_analysis/src/session.rs#L4461) — `outline` fallback | `4473: let (facts, _guards) = crate::build_facts(&view.scopes, &preprocessing, &view.root, &errors);` | declaration facts from the caller's view; the branch above (4462) is raw-only |
| 15 | [definition/mod.rs:102](crates/cpp_ls/src/handlers/definition/mod.rs#L102) | `let view = session.view_of_the_file(&path)?;` | `session.definitions(&view, offset)` at 122 |
| 16 | [completion/mod.rs:157](crates/cpp_ls/src/handlers/completion/mod.rs#L157) | `let view = session.view_of_the_file(&path)?;` | `session.completions(&view, offset)` at 162 |
| 17 | [references/mod.rs:75](crates/cpp_ls/src/handlers/references/mod.rs#L75) | `let written = session.view_of_the_file(&path)?;` | `session.symbol_references(&written, …)` at 96 |
| 18 | [rename/mod.rs:74](crates/cpp_ls/src/handlers/rename/mod.rs#L74) | `let view = session.view_of_the_file(&path)?;` | rename, which the rule classifies as semantic |
| 19 | [rename/mod.rs:101](crates/cpp_ls/src/handlers/rename/mod.rs#L101) | `let view = session.view_of_the_file(&path)?;` | same, `macro_references` at 139 |
| 20 | [semantic_token/mod.rs:205](crates/cpp_ls/src/handlers/semantic_token/mod.rs#L205) | `let view = session.view_of_the_file(&path)?;` | `session.classified_names(&view)` at 220, which reads `view.scopes` and the index |
| 21 | [inlay_hint/mod.rs:93](crates/cpp_ls/src/handlers/inlay_hint/mod.rs#L93) | `let view = session.view_of_the_file(&path)?;` | `session.inlay_hints(&view, …)` at 108 |

Rows 5–6 are the ones the parent's list of three does not name; rows 15–21 are the shell's half of the same
fallback, and they are the ones a fix *inside* the crate cannot reach.

Two sites are the **inverse** of a fallback — a lexical question served from a cooking — and they are in §1.1 rows
10–13 (selection range, folding, document symbol, completion documentation).

One site is deliberate and should **not** be changed: [hover/mod.rs:105](crates/cpp_ls/src/handlers/hover/mod.rs#L105)
asks the file's own text for a `#include` target and a macro name, and its note (94–104) is the correct reading of
the rule: *"Neither is a fallback: each is the only reading that has the thing being asked about."*

---

## 4. How many sites must change, and in what order

### The count

| group | sites | needs the reading changed? |
|---|---|---|
| entry-point call sites in product code | **32** | 22 |
| — `Session::view` (§1.1) | 13 | 9 semantic must become cooked-**only**; 4 lexical must become raw |
| — `Session::view_of_the_file` (§1.2) | 10 | 9 semantic must stop using it; 1 lexical (`hover:105`) stays |
| — `Session::view_of_the_file_with_macros` (§1.3) | 0 | the function should be deleted |
| — `Session::view_of_the_rendering` (§1.4) | 0 | the false "Remembered" comment at 2452–2453 should be fixed or made true |
| — `Session::view_with_macros` (§1.5) | 0 | the function should be deleted |
| — `FileView::parse` (§1.6) | 4 | all 4 are the producers of the two-semantics problem |
| — `FileView::parse_with` (§1.7) | 2 | the third reading — retire it, or confine it to macro editing |
| — `FileView::parse_rendering` (§1.8) | 3 | keep; the cooked side is correct |
| consumer classification of those 32 | semantic **(a)** 18 · lexical **(b)** 5 · producer **(P)** 9 | 18 + 4 = **22 today on the wrong reading** |
| fallback sites (§3) | **21 rows**, **20 distinct code sites** | all 20 |
| fact-entry paths (§2) | **raw 6 (R1–R6), cooked 4 (C1–C2a)** | the 6 raw paths are the ones to stop believing; R2 (disk cache) means **a raw fact can arrive without any parse at all** |
| places where the two authorities meet (§2 U) | **8** | all 8: U-A/U-B/U-C are three different identity rules for one question |

**Headline: 32 entry-point call sites (18 semantic, 5 lexical, 9 producer); 22 of them are on the wrong reading
today. Add the 20 fallback sites and the 8 merge points, and the change touches 60 sites — of which the 9 producers
and the 8 merge points are where the architecture moves, and the other 43 are call sites.**

### Prioritized work list

1. **Decide the identity, once, before touching precedence.** `(name, kind)` at `project.rs:6001` and
   `(name, kind, scope)` at `project.rs:6448–6450` answer the same question two ways, and
   `docs/one-semantic-authority.md:90` names the wrong one. `every_fact_named` (6222) answers it a third way (slot
   aliasing). One question, one rule, or the change will be argued with the old rule still live somewhere.
2. **Reverse the precedence in `visible_declarations_upto`** (`project.rs:6411–6452`): cooked first, raw only for a
   file with no cooked reading. This is the fix the measurement points at, and it is one function.
3. **Wire the unit reading into the pump** — the call at `session.rs:1851` is commented out and the doc above it
   (3299–3321) records the unexplained regression. `docs/one-semantic-authority.md:74–77` claims the precedence rule
   *is* the explanation; step 2 has to be in place before that can be seen, and `examples/std_probe.rs`'s
   `--cooked-index` is the instrument. C2 (`index_unit_rendering`) is the only path that gives **every** file a
   cooked reading, which is what makes step 4 possible at all.
4. **Stop producing raw semantic facts** — `index/mod.rs:666` via `index_tree` from `index()` (R1), and R3's second
   pass (store.rs:1285, 1313). Both remain legitimate producers of *guards, macros, includes and macro-readings*;
   what must stop is their `declarations` being believed.
5. **Then the fallbacks, in this order** — because each earlier one removes the reason for the later:
   `session.rs:2378` (the master), then `2402` + the 9 semantic `view_of_the_file` call sites (§1.2 rows 3–9), then
   the 5 resolver closures (3951, 3967, 4077, 4220, 4304) and the 2 raw resolvers (2890, 4257), then
   `diagnostics`' raw branch (2780–2819) and its checks (2817).
6. **Pin the lexical consumers to the raw reading** — `selection_range/mod.rs:47`, `folding_range/mod.rs:48`,
   `document_symbol/mod.rs:57`, `completion/mod.rs:427`. Grouped with §1.9: these four are where a rendering view is
   already being mapped with the file's own line index, so they are wrong **today**, before any swap.
7. **Delete what the split left behind**: `view_of_the_file_with_macros` (2411), `view_with_macros` (2498),
   `FileView::parse_with` (324) — and the `FileIndexer::macro_facts` field (107), which is written and never read
   while the doc on it (94–97) describes behaviour the crate does not have.
8. **Then re-run the instrument at the top of this file on all six headers**, including `<vector>` and
   `<type_traits>`, which parse clean in both readings and are the rows that falsify "raw is always wrong".

---

## Appendix — call sites outside product code (nothing here ships)

`Session::view`: `cpp_code_analysis/tests/` — `inlay.rs:36,219,286`; `types.rs:39,281,307,409,723,754`;
`documentation.rs:37,130,163,181`; `signature.rs:44,139,168,208,231,269`;
`completion.rs:51,101,125,143,187,275,314,364,424,458,478,515,546,581,639,670,694,708,763,827,936,1012`;
`semantic.rs:36,137,220,316,348,413,448,502,936,988,1176,1186,1221,1393,1470,1528`; `selection.rs:23`;
`modules.rs:1237,1287,1340,1445,1480`; `session.rs`'s own unit tests at
`6359,6403,6441,6474,6717,6746,6768,6787,6807,7370`.
`cpp_ls/tests`-adjacent: `cpp_ls/src/util/position.rs:204`, `cpp_ls/src/handlers/selection_range/mod.rs:131`,
`rename/mod.rs:285,295`, `references/mod.rs:284,298`, `context/analysis_state.rs:517,537`.
`examples/`: `coordinates_probe.rs:67`, `completion_probe.rs:54,110`, `checks_timing.rs:68`, `lookup_probe.rs:57,58`,
`find_references.rs:272,290`, `editor_probe.rs:172,176,204,228,288,503,666,751,902`, `diagnostics_cost.rs:120,148`,
`workspace_probe.rs:104,302`, `view_probe.rs:99,106`, `modules_probe.rs:135`, `member_probe_at.rs:45,69`,
`unit_read.rs:65`, `member_probe.rs:194`, `open_project.rs:154,281,294`, `semantic_probe.rs:71`.

`Session::view_of_the_file`: `examples/parse_errors.rs:31`, `examples/completion_latency.rs:138,266,324,367,378,388`.

`Session::view_of_the_file_with_macros`: none anywhere.

`Session::view_of_the_rendering`: `examples/types_probe.rs:133`, `examples/coordinates_probe.rs:65`.

`Session::view_with_macros`: `tests/semantic.rs:1081`, `examples/editor_probe.rs:181,759,764`.

`FileView::parse`: `file/view.rs:513,533,542` (unit tests).
`FileView::parse_with`: none.
`FileView::parse_rendering`: `tests/semantic.rs:1323`.

`index_rendering` outside the product path: `tests/translation_unit.rs:967`, `examples/std_probe.rs:745`,
`index/project.rs` tests at `8843, 8877, 8893, 12251`.

`index_unit_rendering`: `tests/translation_unit.rs` (via `indexer.index` at 944 and `index_rendering` at 967);
no product caller besides `session.rs:3340`.

`build_facts` / `build_scopes` outside product code: `tests/checks.rs:50`, `tests/semantic.rs:592`,
`tests/scopes.rs:29,1866,1923,2021`, `sema/declarations.rs:3265,3656,3678`, `sema/resolve.rs:1136`,
`index/project.rs` tests (20 sites, 9461–11539), `examples/std_query.rs:153`, `examples/use_index_cost.rs:112`.
