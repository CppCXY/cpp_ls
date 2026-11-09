//! **The questions** — one function per command, each answering with a line for a reader and a value for `--json`.
//!
//! The set is the one this project kept re-writing as `tests/tmp_*.rs`: the type at a position, the hover card, the
//! declarations a name has, a class's members, the fact list behind a file, the macro in force, the completion list,
//! the findings, the summary's key, and the syntax tree at a point. Every one of them is a single query on the session
//! that [`crate::project`] built; nothing is computed here that the analysis does not already answer.

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{
    DeclFact, DeclKind, DiskFiles, Known, ProjectDefinition, ProjectMember, Session,
};
use serde_json::{Value, json};

use crate::place::{Place, line_col};
use crate::project::Opened;

/// **The answer to one question**: the text a person reads, and the value `--json` prints.
pub struct Answer {
    pub text: String,
    pub value: Value,
}

impl Answer {
    fn new(text: String, value: Value) -> Self {
        Self { text, value }
    }
}

/// Where an offset is, as a report writes it: `line:column`.
fn at_a_place(text: &str, offset: usize) -> String {
    let (line, column) = line_col(text, offset);
    format!("{line}:{column}")
}

/// One declaration's own location — the **declaring** file's coordinates, which are not this file's.
fn where_a_declaration(session: &Session<DiskFiles>, definition: &ProjectDefinition) -> String {
    let name = definition
        .file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| definition.file.display().to_string());
    match session.text(&definition.file) {
        Some(text) => format!(
            "{name}:{}",
            at_a_place(&text, definition.fact.name_range.start_offset)
        ),
        None => name,
    }
}

fn kind_of(kind: DeclKind) -> &'static str {
    match kind {
        DeclKind::Type => "type",
        DeclKind::Function => "function",
        DeclKind::Variable => "variable",
        DeclKind::Namespace => "namespace",
        DeclKind::MacroLike => "macro-like",
        DeclKind::Other => "other",
    }
}

/// The fields of a fact a reader asks about, in one line.
fn a_fact_and_what_it_says(fact: &DeclFact) -> String {
    format!("{:<28} {}", fact.name, what_it_says(fact))
}

/// The same, without the name — for a caller that prints the name in a column of its own.
fn what_it_says(fact: &DeclFact) -> String {
    let mut said = kind_of(fact.kind).to_string();
    if let Some(scope) = &fact.scope {
        said.push_str(&format!(" in {scope}"));
    }
    if let Some(type_of) = &fact.type_of {
        said.push_str(&format!(" type `{type_of}`"));
    }
    if let Some(returns) = &fact.returns {
        said.push_str(&format!(" returns `{returns}`"));
    }
    if let Some(parameters) = &fact.parameter_list {
        said.push_str(&format!(" parameters `{parameters}`"));
    }
    if !fact.parameters.is_empty() {
        said.push_str(&format!(" over [{}]", fact.parameters.join(", ")));
    }
    if let Some(requires) = &fact.requires_clause {
        said.push_str(&format!(" requires `{requires}`"));
    }
    // **The annotation the declaration was written under** — `UCLASS(Blueprintable)`. Reported here because the card is
    // rendered from the fact, so a fact that lost it and a card that does not show it look the same from outside.
    if let Some(reflection) = &fact.reflection {
        said.push_str(&format!(" under `{}`", reflection.said()));
    }
    if fact.modifiers.bits() != 0 {
        said.push_str(&format!(" modifiers {:#x}", fact.modifiers.bits()));
    }
    said
}

fn declaration_value(session: &Session<DiskFiles>, definition: &ProjectDefinition) -> Value {
    json!({
        "file": definition.file.display().to_string(),
        "where": where_a_declaration(session, definition),
        "name": definition.fact.name,
        "kind": kind_of(definition.fact.kind),
        "scope": definition.fact.scope,
        "type_of": definition.fact.type_of,
        "returns": definition.fact.returns,
        "parameter_list": definition.fact.parameter_list,
        "parameters": definition.fact.parameters,
        "requires_clause": definition.fact.requires_clause,
        "bases": definition.fact.bases,
        "reflection": definition.fact.reflection.as_ref().map(|reflection| reflection.said()),
        "modifiers": definition.fact.modifiers.bits(),
    })
}

/// **The type of the expression at the position** — `type_at`, which asks about the innermost expression there.
pub fn the_type(opened: &Opened, place: &Place) -> Result<Answer, String> {
    let at = place.offset(&opened.text)?;
    let known = opened.session.type_at(&opened.view, ReadingOffset::new(at));
    let where_at = at_a_place(&opened.text, at);
    let (text, value) = match &known {
        Known::Yes(found) => (
            format!(
                "type at {where_at}: `{}` is `{}` (declared in {})",
                found.expression,
                found.type_of,
                found.file.display()
            ),
            json!({
                "at": at,
                "where": where_at,
                "expression": found.expression,
                "type_of": found.type_of,
                "file": found.file.display().to_string(),
            }),
        ),
        Known::Unknown(reason) => (
            format!("type at {where_at}: not known — {reason:?}"),
            json!({ "at": at, "where": where_at, "unknown": format!("{reason:?}") }),
        ),
        Known::No => (
            format!("type at {where_at}: not an expression"),
            json!({ "at": at, "where": where_at, "not_an_expression": true }),
        ),
    };
    Ok(Answer::new(text, value))
}

/// **The hover card the server would show** — the server's own rendering, which is the answer an editor gets.
pub fn the_hover(opened: &Opened, place: &Place) -> Result<Answer, String> {
    let at = place.offset(&opened.text)?;
    let where_at = at_a_place(&opened.text, at);
    // **What the analysis answers before the card is rendered** — the probe this question needed: `hover` and
    // `define` disagreed about the same cursor, and the difference had to be seen before `hover_text` composed
    // anything. See `docs/buglist.md` section 7.
    if std::env::var("CPPLS_TRACE_HOVER").is_ok() {
        let picked = opened
            .session
            .definition(&opened.view, ReadingOffset::new(at));
        let all = opened
            .session
            .definitions(&opened.view, ReadingOffset::new(at));
        eprintln!("[hover] at {at}: picked {picked:?}");
        eprintln!("[hover] at {at}: all {all:?}");
    }
    let shown = cpp_ls::handlers::hover::hover_text(
        &opened.session,
        &opened.view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    );
    let (text, value) = match &shown {
        Some(markdown) => (
            format!("hover at {where_at}:\n{markdown}"),
            json!({ "at": at, "where": where_at, "markdown": markdown }),
        ),
        None => (
            format!("hover at {where_at}: none"),
            json!({ "at": at, "where": where_at, "markdown": Value::Null }),
        ),
    };
    Ok(Answer::new(text, value))
}

/// **The declarations of the name at the position** — the one the analysis picks, and every one it knows.
pub fn what_it_names(opened: &Opened, place: &Place) -> Result<Answer, String> {
    let at = place.offset(&opened.text)?;
    let where_at = at_a_place(&opened.text, at);
    let session = &opened.session;
    let picked = session.definition(&opened.view, ReadingOffset::new(at));
    let all = session.definitions(&opened.view, ReadingOffset::new(at));

    let mut text = format!("define at {where_at}:\n");
    let mut picked_value = Value::Null;
    match &picked {
        Known::Yes(definition) => {
            picked_value = declaration_value(session, definition);
            text.push_str(&format!(
                "  picked   {:<40} {}\n",
                where_a_declaration(session, definition),
                a_fact_and_what_it_says(&definition.fact)
            ));
        }
        Known::Unknown(reason) => text.push_str(&format!("  picked   unknown — {reason:?}\n")),
        Known::No => text.push_str("  picked   no declaration\n"),
    }
    match &all {
        Known::Yes(found) => {
            for definition in &found.found {
                text.push_str(&format!(
                    "  found    {:<40} {}\n",
                    where_a_declaration(session, definition),
                    a_fact_and_what_it_says(&definition.fact)
                ));
            }
            if found.found.is_empty() {
                text.push_str("  found    none\n");
            }
        }
        Known::Unknown(reason) => text.push_str(&format!("  found    unknown — {reason:?}\n")),
        Known::No => text.push_str("  found    none\n"),
    }
    let value = json!({
        "at": at,
        "where": where_at,
        "picked": picked_value,
        "found": match &all {
            Known::Yes(found) => found.found.iter().map(|definition| declaration_value(session, definition)).collect::<Vec<_>>(),
            _ => Vec::new(),
        },
    });
    Ok(Answer::new(text, value))
}

/// **A class's members**, with the class that declares each and how many base steps away it is.
pub fn the_members(opened: &Opened, class: &str, limit: usize) -> Result<Answer, String> {
    let session = &opened.session;
    let mut text = String::new();
    let listed = session.members_of(&opened.view, class);
    let value = match &listed {
        Known::Yes(list) => {
            let shown = match limit {
                0 => list.members.len(),
                limit => limit.min(list.members.len()),
            };
            text.push_str(&format!(
                "members of `{class}`: {} member(s), {} unlisted base(s)\n",
                list.members.len(),
                list.unlisted.len()
            ));
            for base in &list.unlisted {
                text.push_str(&format!(
                    "  unlisted base `{}`: {:?}\n",
                    base.spelling, base.reason
                ));
            }
            for member in list.members.iter().take(shown) {
                text.push_str(&format!(
                    "  d{} {}\n",
                    member.depth,
                    a_member_and_where_it_is(session, member)
                ));
            }
            json!({
                "class": class,
                "count": list.members.len(),
                "unlisted": list.unlisted.iter().map(|base| json!({ "spelling": base.spelling, "reason": format!("{:?}", base.reason) })).collect::<Vec<_>>(),
                "members": list.members.iter().take(shown).map(|member| json!({
                    "name": member.fact.name,
                    "kind": kind_of(member.fact.kind),
                    "declared_in": member.declared_in,
                    "depth": member.depth,
                    "file": member.file.display().to_string(),
                    "type_of": member.fact.type_of,
                    "returns": member.fact.returns,
                    "parameter_list": member.fact.parameter_list,
                    "requires_clause": member.fact.requires_clause,
                    "modifiers": member.fact.modifiers.bits(),
                })).collect::<Vec<_>>(),
            })
        }
        Known::Unknown(reason) => {
            text.push_str(&format!("members of `{class}`: unknown — {reason:?}\n"));
            json!({ "class": class, "unknown": format!("{reason:?}") })
        }
        Known::No => {
            text.push_str(&format!("members of `{class}`: no such class here\n"));
            json!({ "class": class, "none": true })
        }
    };
    Ok(Answer::new(text, value))
}

fn a_member_and_where_it_is(session: &Session<DiskFiles>, member: &ProjectMember) -> String {
    let where_is = match session.text(&member.file) {
        Some(text) => at_a_place(&text, member.fact.name_range.start_offset),
        None => String::new(),
    };
    let name = member
        .file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "{:<28} {:<40} (declared in `{}`) {name}:{where_is}",
        member.fact.name,
        what_it_says(&member.fact),
        member.declared_in
    )
}

/// **The macro in force for the name at the position** — what "go to the definition" of an `#define` asks.
pub fn the_macro(opened: &Opened, place: &Place) -> Result<Answer, String> {
    let at = place.offset(&opened.text)?;
    let where_at = at_a_place(&opened.text, at);
    let known = opened
        .session
        .macro_at_a_file_offset(&opened.view, FileOffset::new(at));
    let (text, value) = match &known {
        Known::Yes(found) => {
            let where_is = match opened.session.text(&found.file) {
                Some(text) => at_a_place(&text, found.fact.range.start_offset),
                None => String::new(),
            };
            (
                format!(
                    "macro at {where_at}: `{}` is {} at {}:{where_is} (function-like {})",
                    found.fact.name,
                    match found.fact.kind {
                        cpp_code_analysis::MacroKind::Definition => "defined",
                        cpp_code_analysis::MacroKind::Undefinition => "undefined",
                    },
                    found.file.display(),
                    found.fact.function_like
                ),
                json!({
                    "at": at,
                    "where": where_at,
                    "name": found.fact.name,
                    "kind": format!("{:?}", found.fact.kind),
                    "function_like": found.fact.function_like,
                    "file": found.file.display().to_string(),
                    "where_in_the_file": where_is,
                }),
            )
        }
        Known::Unknown(reason) => (
            format!("macro at {where_at}: unknown — {reason:?}"),
            json!({ "at": at, "where": where_at, "unknown": format!("{reason:?}") }),
        ),
        Known::No => (
            format!("macro at {where_at}: no"),
            json!({ "at": at, "where": where_at, "none": true }),
        ),
    };
    Ok(Answer::new(text, value))
}

/// **The completion labels offered at the position.**
pub fn the_completions(opened: &Opened, place: &Place, limit: usize) -> Result<Answer, String> {
    let at = place.offset(&opened.text)?;
    let where_at = at_a_place(&opened.text, at);
    let set = opened
        .session
        .completions(&opened.view, FileOffset::new(at));
    let shown = match limit {
        0 => set.items.len(),
        limit => limit.min(set.items.len()),
    };
    let mut text = format!(
        "complete at {where_at}: scope `{}`, prefix `{}`, {} item(s)\n",
        set.scope,
        set.prefix,
        set.items.len()
    );
    for item in set.items.iter().take(shown) {
        text.push_str(&format!(
            "  {:<32} {:?} {}\n",
            item.label,
            item.kind,
            item.detail.clone().unwrap_or_default()
        ));
    }
    let value = json!({
        "at": at,
        "where": where_at,
        "scope": set.scope,
        "prefix": set.prefix,
        "count": set.items.len(),
        "items": set.items.iter().take(shown).map(|item| json!({
            "label": item.label,
            "kind": format!("{:?}", item.kind),
            "detail": item.detail,
            "insert": item.insert,
            "snippet": item.snippet,
        })).collect::<Vec<_>>(),
    });
    Ok(Answer::new(text, value))
}

/// **The facts a file's summary holds**, filtered.
pub struct FactFilters<'a> {
    pub name: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub scope: Option<&'a str>,
    pub limit: usize,
}

pub fn the_facts(opened: &Opened, filters: &FactFilters<'_>) -> Result<Answer, String> {
    let summary = opened
        .session
        .index()
        .summary(&opened.file)
        .ok_or_else(|| format!("{} has no summary", opened.file.display()))?;
    let matching: Vec<&DeclFact> = summary
        .declarations
        .iter()
        .filter(|fact| {
            filters.name.is_none_or(|name| fact.name.contains(name))
                && filters
                    .kind
                    .is_none_or(|kind| kind_of(fact.kind).eq_ignore_ascii_case(kind))
                && filters.scope.is_none_or(|scope| {
                    fact.scope.as_deref().is_some_and(|own| own.contains(scope))
                })
        })
        .collect();
    let limit = match filters.limit {
        0 => matching.len(),
        limit => limit,
    };
    let mut text = format!(
        "facts in {}: {} of {} declaration(s)\n",
        opened.file.display(),
        matching.len(),
        summary.declarations.len()
    );
    for fact in matching.iter().take(limit) {
        text.push_str(&format!(
            "  {}:{}  {}\n",
            line_col(&opened.text, fact.name_range.start_offset).0,
            line_col(&opened.text, fact.name_range.start_offset).1,
            a_fact_and_what_it_says(fact)
        ));
    }
    let value = json!({
        "file": opened.file.display().to_string(),
        "total": summary.declarations.len(),
        "matched": matching.len(),
        "facts": matching.iter().take(limit).map(|fact| json!({
            "name": fact.name,
            "kind": kind_of(fact.kind),
            "scope": fact.scope,
            "type_of": fact.type_of,
            "returns": fact.returns,
            "parameter_list": fact.parameter_list,
            "parameters": fact.parameters,
            "requires_clause": fact.requires_clause,
            "modifiers": fact.modifiers.bits(),
            "line": line_col(&opened.text, fact.name_range.start_offset).0,
            "column": line_col(&opened.text, fact.name_range.start_offset).1,
        })).collect::<Vec<_>>(),
    });
    Ok(Answer::new(text, value))
}

/// **The inlay hints the analysis asks an editor to draw** — parameter names and deduced types.
pub fn the_inlay(opened: &Opened) -> Result<Answer, String> {
    let whole = cpp_parser::SourceRange::new(0, opened.text.len());
    let parameters = opened.session.inlay_hints(&opened.view, whole);
    let types = opened.session.type_hints(&opened.view, whole);
    let mut text = format!(
        "inlay hints in {}: {} parameter(s), {} type(s)\n",
        opened.file.display(),
        parameters.len(),
        types.len()
    );
    for hint in &parameters {
        let (line, column) = line_col(&opened.text, hint.offset);
        text.push_str(&format!("  {line}:{column}  parameter  `{}:`\n", hint.name));
    }
    for hint in &types {
        let (line, column) = line_col(&opened.text, hint.offset);
        let punctuation = match hint.kind {
            cpp_code_analysis::TypeHintKind::DeducedReturn => "->",
            _ => ":",
        };
        text.push_str(&format!(
            "  {line}:{column}  type       {punctuation} `{}`  ({:?})\n",
            hint.label, hint.kind
        ));
    }
    let value = json!({
        "file": opened.file.display().to_string(),
        "parameters": parameters.iter().map(|hint| json!({
            "line": line_col(&opened.text, hint.offset).0,
            "column": line_col(&opened.text, hint.offset).1,
            "name": hint.name,
        })).collect::<Vec<_>>(),
        "types": types.iter().map(|hint| json!({
            "line": line_col(&opened.text, hint.offset).0,
            "column": line_col(&opened.text, hint.offset).1,
            "label": hint.label,
            "kind": format!("{:?}", hint.kind),
        })).collect::<Vec<_>>(),
    });
    Ok(Answer::new(text, value))
}

/// **The findings a diagnostics channel would report for the file.**
pub fn the_diagnostics(opened: &Opened) -> Result<Answer, String> {
    let found = opened
        .session
        .diagnostics(&opened.file)
        .ok_or_else(|| format!("{} has no reading to diagnose", opened.file.display()))?;
    // **Both channels**: the reading's errors and the rules' findings. Printing only `errors` answered "0 finding(s)" for a file
    // whose `missing-return` rule fires, which hides the whole analysis from the tool that exists to show it.
    let mut text = format!(
        "diagnostics in {}: {} error(s), {} check finding(s), reading {:?}, {} unplaced\n",
        opened.file.display(),
        found.errors.len(),
        found.checks.len(),
        found.reading,
        found.unplaced
    );
    for error in &found.errors {
        let (line, column) = line_col(&opened.text, error.start);
        text.push_str(&format!(
            "  {line}:{column}  parse  {}  {}\n",
            error.code.name(),
            error.message
        ));
    }
    for finding in &found.checks {
        let (line, column) = line_col(&opened.text, finding.range.start_offset);
        text.push_str(&format!(
            "  {line}:{column}  check  {}  {}\n",
            finding.code.name(),
            finding.message
        ));
    }
    let mut findings: Vec<serde_json::Value> = found
        .errors
        .iter()
        .map(|error| {
            json!({
                "channel": "parse",
                "line": line_col(&opened.text, error.start).0,
                "column": line_col(&opened.text, error.start).1,
                "code": error.code.name(),
                "message": error.message,
            })
        })
        .collect();
    findings.extend(found.checks.iter().map(|finding| {
        json!({
            "channel": "check",
            "line": line_col(&opened.text, finding.range.start_offset).0,
            "column": line_col(&opened.text, finding.range.start_offset).1,
            "code": finding.code.name(),
            "name": finding.name,
            "message": finding.message,
        })
    }));
    let value = json!({
        "file": opened.file.display().to_string(),
        "reading": format!("{:?}", found.reading),
        "unplaced": found.unplaced,
        "findings": findings,
    });
    Ok(Answer::new(text, value))
}

/// **The summary's key, includes and counts** — what the cache and the include graph decided about the file.
pub fn the_summary(opened: &Opened) -> Result<Answer, String> {
    let index = opened.session.index();
    let summary = index
        .summary(&opened.file)
        .ok_or_else(|| format!("{} has no summary", opened.file.display()))?;
    let resolved = summary
        .includes
        .iter()
        .filter(|include| include.resolved.is_some())
        .count();
    let cooked = index.cooked_reading(&opened.file).is_some();
    let mut text = format!(
        "summary of {}: {} declaration(s), {} macro fact(s), {} include(s) ({} resolved), cooked reading {}\n",
        opened.file.display(),
        summary.declarations.len(),
        summary.macros.len(),
        summary.includes.len(),
        resolved,
        match cooked {
            true => "yes",
            false => "no",
        }
    );
    text.push_str(&format!(
        "  key: content {:#018x} context {:#018x} environment {:#018x}\n",
        summary.key.content_hash, summary.key.context_hash, summary.key.env_hash
    ));
    for include in &summary.includes {
        text.push_str(&format!(
            "  include {:?} -> {}\n",
            include.spelling,
            match &include.resolved {
                Some(resolved) => resolved.display().to_string(),
                None => "unresolved".to_string(),
            }
        ));
    }
    let value = json!({
        "file": opened.file.display().to_string(),
        "declarations": summary.declarations.len(),
        "macros": summary.macros.len(),
        "includes": summary.includes.iter().map(|include| json!({
            "spelling": include.spelling,
            "resolved": include.resolved.as_ref().map(|resolved| resolved.display().to_string()),
        })).collect::<Vec<_>>(),
        "cooked_reading": cooked,
        "key": {
            "content": summary.key.content_hash,
            "context": summary.key.context_hash,
            "environment": summary.key.env_hash,
        },
    });
    Ok(Answer::new(text, value))
}

/// **The syntax tree at the position**, innermost first — the node kind and the text of each ancestor.
pub fn the_tree(opened: &Opened, place: &Place, limit: usize) -> Result<Answer, String> {
    let at = place.offset(&opened.text)?;
    let where_at = at_a_place(&opened.text, at);
    let mut text = format!("tree at {where_at}:\n");
    let mut chain: Vec<Value> = Vec::new();
    if let Some(token) = cpp_parser::token_at(&opened.view.root, at) {
        text.push_str(&format!(
            "  token {:?} {:?}\n",
            token.kind(),
            token.text().to_string()
        ));
        let mut node = token.parent();
        let mut depth = 0;
        while let Some(current) = node {
            if limit != 0 && depth >= limit {
                break;
            }
            let spelled = current.text().to_string();
            let shown: String = spelled.chars().take(60).collect();
            text.push_str(&format!("  {:?} {:?}\n", current.kind(), shown));
            chain.push(json!({
                "kind": format!("{:?}", current.kind()),
                "text": shown,
            }));
            node = current.parent();
            depth += 1;
        }
    } else {
        text.push_str("  no token at that offset\n");
    }
    let value = json!({ "at": at, "where": where_at, "ancestors": chain });
    Ok(Answer::new(text, value))
}

/// **The reading format the index on disk uses** — a cache written by another version is not read, and the number is
/// what says so.
pub fn the_format() -> Answer {
    Answer::new(
        format!(
            "format version {}, reading fingerprint {:#018x}",
            cpp_code_analysis::FORMAT_VERSION,
            cpp_code_analysis::READING_FINGERPRINT
        ),
        json!({
            "format_version": cpp_code_analysis::FORMAT_VERSION,
            "reading_fingerprint": cpp_code_analysis::READING_FINGERPRINT,
        }),
    )
}

/// The session's own counters, for `--stats`.
pub fn the_stats(opened: &Opened) -> String {
    let stats = opened.session.stats();
    format!(
        "stats: {} file(s) in the project, summaries reused {} built {} not stored {}",
        opened.session.project_files().len(),
        stats.reused,
        stats.rebuilt,
        stats.unstored
    )
}
