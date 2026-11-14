//! # `textDocument/prepareTypeHierarchy`, `typeHierarchy/supertypes` and `typeHierarchy/subtypes`
//!
//! C++'s class hierarchy, as the editor draws it: prepare returns the class under the cursor, `supertypes` its
//! bases, and `subtypes` the classes that name it in their own `bases` list.
//!
//! # What is answered, precisely
//!
//! ```text
//! prepare       a cursor on a class's name, or on a `struct`/`class`/`union` keyword, gives that class
//! supertypes    the class's own `bases`, each resolved to the declaration it names
//! subtypes      every class whose `bases` names this one — the direction the index does not keep, so a scan
//! ```
//!
//! A base is matched by both its whole spelling and its **last segment**, the rule [`super::implementation`] states:
//! one file writes `Base` and another `ns::Base`, and both name the same class. Resolving a base to its declaration
//! asks the index (`ProjectIndex::definition`), so a base from a header nobody read answers nothing rather than a
//! guess.
//!
//! # Why the capability is not in `ServerCapabilities`
//!
//! The protocol's field is `typeHierarchyProvider` (LSP 3.17), and the `lsp_types` fork this server links against
//! predates it — its `ServerCapabilities` has no such field to set. The handlers below are real, so the capability is
//! written into the JSON `initialize` sends (see `crate::server`), which is what a client reads.
//! [`TypeHierarchyCapabilities`] is therefore a no-op that exists to say where the field is written rather than to set
//! one.
//!
//! # What the item's `data` carries
//!
//! A [`TypeHierarchyItem`] is handed back by the client to `supertypes`/`subtypes`, and the protocol expects the
//! server to remember what it was about. What is remembered is the **class's file and its written name**, in the same
//! `{file, name}` shape call hierarchy uses — enough to re-ask the index without a second query at any cursor.

use cpp_code_analysis::{DeclKind, FileProvider, Known, Session};
use lsp_types::{
    ClientCapabilities, Range, ServerCapabilities, SymbolKind, TypeHierarchyItem,
    TypeHierarchyPrepareParams, TypeHierarchySubtypesParams, TypeHierarchySupertypesParams, Uri,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{offset_at_position, path_to_uri, position_in_file, uri_to_file_path};

/// **The class under the cursor**, or nothing when the position is not on one.
pub async fn on_prepare_type_hierarchy(
    context: ServerContextSnapshot,
    params: TypeHierarchyPrepareParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<TypeHierarchyItem>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
        context.analysis().catch_up(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view_of_the_file(&path)?;
        let offset = offset_at_position(&view, position)?;
        let item = item_at(session, &view, offset)?;
        Some(vec![item])
    })
    .await
}

/// **The bases of the class an item names.**
pub async fn on_supertypes(
    context: ServerContextSnapshot,
    params: TypeHierarchySupertypesParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<TypeHierarchyItem>> {
    snapshot_query(context.analysis(), cancel_token, move |session| {
        let (file, name) = the_class_an_item_names(&params.item)?;
        let found = supertypes_of(session, &file, &name);
        if found.is_empty() {
            return None;
        }
        Some(found)
    })
    .await
}

/// **The classes whose `bases` name the class an item names.**
pub async fn on_subtypes(
    context: ServerContextSnapshot,
    params: TypeHierarchySubtypesParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<TypeHierarchyItem>> {
    snapshot_query(context.analysis(), cancel_token, move |session| {
        let (_file, name) = the_class_an_item_names(&params.item)?;
        let found = subtypes_of(session, &name);
        if found.is_empty() {
            return None;
        }
        Some(found)
    })
    .await
}

/// **The class under the cursor**, asked of the analysis rather than read off the text: `definitions` already answers
/// "what does this name declare", and a second reading of the identifier could disagree with it.
pub fn item_at<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
) -> Option<TypeHierarchyItem> {
    let Known::Yes(defined) = session.definitions(view, offset) else {
        return None;
    };
    defined
        .found
        .iter()
        .find(|found| found.fact.kind == DeclKind::Type)
        .and_then(|found| item_of_a_class(session, &found.file, &found.fact))
}

/// **The class's own base list, resolved** — `supertypes` of the class `name` written in `file`.
pub fn supertypes_of<F: FileProvider + Clone>(
    session: &Session<F>,
    file: &std::path::Path,
    name: &str,
) -> Vec<TypeHierarchyItem> {
    let Known::Yes(class) = session.index().definition(name, file) else {
        return Vec::new();
    };
    let mut found: Vec<TypeHierarchyItem> = Vec::new();
    // **The bases as the class wrote them**, then each resolved from the class's own file — where the names are
    // visible. A base that does not resolve whole is tried as its last segment: see the module documentation.
    for base in &class.fact.bases {
        let resolved = match session.index().definition(base, &class.file) {
            Known::Yes(found) => Some(found),
            _ => match session.index().definition(last_segment(base), &class.file) {
                Known::Yes(found) => Some(found),
                _ => None,
            },
        };
        if let Some(resolved) = resolved
            && let Some(item) = item_of_a_class(session, &resolved.file, &resolved.fact)
        {
            found.push(item);
        }
    }
    found
}

/// **The classes that name `name` in their own `bases`** — the direct subtypes, in the index's order.
///
/// A scan rather than an index query, and for the reason [`super::implementation`] gives for the same walk: the index
/// holds "what this class derives from" (`DeclFact::bases`), not "who derives from this class". The reverse edge
/// would be a table kept in step with every edit, while a hierarchy expansion asks once.
pub fn subtypes_of<F: FileProvider + Clone>(
    session: &Session<F>,
    name: &str,
) -> Vec<TypeHierarchyItem> {
    let class = last_segment(name);
    let mut found: Vec<TypeHierarchyItem> = Vec::new();
    for (path, summary) in session.index().all_summaries() {
        for fact in &summary.declarations {
            if fact.kind != DeclKind::Type {
                continue;
            }
            if !fact.bases.iter().any(|base| last_segment(base) == class) {
                continue;
            }
            if let Some(item) = item_of_a_class(session, &path, fact) {
                found.push(item);
            }
        }
    }
    found
}

/// **One hierarchy item for a class declaration** — its name, where it is written, and what the follow-up needs.
fn item_of_a_class<F: FileProvider + Clone>(
    session: &Session<F>,
    file: &std::path::Path,
    fact: &cpp_code_analysis::DeclFact,
) -> Option<TypeHierarchyItem> {
    let uri: Uri = path_to_uri(file)?;
    let held = session.files().held(file)?;
    let start = position_in_file(held, fact.name_range.start_offset)?;
    let end = position_in_file(held, fact.name_range.end_offset())?;
    let name_range = Range { start, end };
    Some(TypeHierarchyItem {
        name: fact.name.clone(),
        kind: SymbolKind::CLASS,
        tags: None,
        // **The scope as the detail**, which is what tells two same-named classes apart in the picker.
        detail: fact.scope.clone(),
        uri,
        range: name_range,
        selection_range: name_range,
        data: Some(serde_json::json!({
            "file": file.to_string_lossy(),
            "name": fact.name,
        })),
    })
}

/// **The file and the class name an item was made from** — the `data` [`item_of_a_class`] wrote.
fn the_class_an_item_names(item: &TypeHierarchyItem) -> Option<(std::path::PathBuf, String)> {
    let data = item.data.as_ref()?;
    let file = data.get("file")?.as_str()?;
    let name = data.get("name")?.as_str()?;
    Some((std::path::PathBuf::from(file), name.to_string()))
}

/// The last `::`-separated segment of a spelling: `ns::Base` → `Base`.
fn last_segment(spelling: &str) -> &str {
    spelling
        .rsplit("::")
        .next()
        .map(str::trim)
        .unwrap_or(spelling)
}

/// See the module documentation: the field this capability names is written into the `initialize` JSON by hand,
/// because the `lsp_types` fork has no `ServerCapabilities` field for it.
pub struct TypeHierarchyCapabilities;

impl RegisterCapabilities for TypeHierarchyCapabilities {
    fn register_capabilities(_server: &mut ServerCapabilities, _client: &ClientCapabilities) {}
}

#[cfg(test)]
mod tests {
    use cpp_code_analysis::{CompilerConfig, MemoryFiles, OpenDocuments, SessionFiles, WatchFilter};

    use super::*;

    fn session_of(files: &[(&str, &str)]) -> Session<MemoryFiles> {
        let mut memory = MemoryFiles::new();
        let mut paths = Vec::new();
        for (path, text) in files {
            memory = memory.with_file(*path, *text);
            paths.push(std::path::PathBuf::from(path));
        }
        let providers = SessionFiles::new(OpenDocuments::new(), memory);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files(paths.clone());
        for path in &paths {
            session.load(path);
            session.want_cooked_reading(path);
        }
        session.index_everything();
        session
    }

    /// **A family**: `Base`, `Derived : Base`, `Further : Derived`, and a class that derives from nothing.
    const BASE: &str = "struct Base {\n    virtual void run();\n};\n";
    const DERIVED: &str =
        "#include \"base.h\"\nstruct Derived : Base {\n    void run() override;\n};\n";
    const FURTHER: &str = "#include \"derived.h\"\nstruct Further : Derived {};\n";
    const ALONE: &str = "struct Alone {};\n";

    fn family() -> Session<MemoryFiles> {
        session_of(&[
            ("/p/base.h", BASE),
            ("/p/derived.h", DERIVED),
            ("/p/further.h", FURTHER),
            ("/p/alone.h", ALONE),
        ])
    }

    /// **`prepare` answers the class under the cursor**, and nothing for a cursor that is not on one.
    #[test]
    fn prepare_finds_the_class_under_the_cursor() {
        let session = family();
        let view = session
            .view_of_the_file(std::path::Path::new("/p/derived.h"))
            .expect("a view");
        let at = DERIVED.find("Derived :").expect("the fixture names it");
        let reading = view
            .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(at))
            .expect("the name is in the rendering");
        let item = item_at(&session, &view, reading).expect("the cursor is on a class");
        assert_eq!(item.name, "Derived");
        assert_eq!(item.kind, SymbolKind::CLASS);

        // A cursor on `int` inside `Base` is not a class.
        let base = session
            .view_of_the_file(std::path::Path::new("/p/base.h"))
            .expect("a view");
        let at = BASE.find("virtual").expect("the fixture writes it");
        let reading = base
            .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(at))
            .expect("the keyword is in the rendering");
        assert!(
            item_at(&session, &base, reading).is_none(),
            "`virtual` is not a class"
        );
    }

    /// **`supertypes` is the class's own base list, resolved** to the declaration it names.
    #[test]
    fn supertypes_are_the_bases_the_class_names() {
        let session = family();
        let bases: Vec<String> = supertypes_of(
            &session,
            std::path::Path::new("/p/derived.h"),
            "Derived",
        )
        .into_iter()
        .map(|item| item.name)
        .collect();
        assert_eq!(bases, ["Base"]);

        assert!(
            supertypes_of(&session, std::path::Path::new("/p/base.h"), "Base").is_empty(),
            "a class with no bases has no supertypes"
        );
    }

    /// **`subtypes` is the reverse edge**: only the *direct* classes that name the base.
    #[test]
    fn subtypes_are_the_classes_that_name_it() {
        let session = family();
        let direct: Vec<String> = subtypes_of(&session, "Base")
            .into_iter()
            .map(|item| item.name)
            .collect();
        assert_eq!(
            direct,
            ["Derived"],
            "`Further` derives from `Derived`, not from `Base` — the hierarchy lists direct subtypes"
        );

        let under_derived: Vec<String> = subtypes_of(&session, "Derived")
            .into_iter()
            .map(|item| item.name)
            .collect();
        assert_eq!(under_derived, ["Further"]);

        assert!(
            subtypes_of(&session, "Alone").is_empty(),
            "nothing derives from `Alone`"
        );
    }

    /// **A base written with a namespace resolves to the class it names**, which is the rule the whole hierarchy
    /// rests on: `ns::Base` in one file and `Base` in another are one class.
    #[test]
    fn a_qualified_base_resolves_to_the_class_it_names() {
        let session = session_of(&[
            ("/p/base.h", "namespace ns {\nstruct Base {};\n}\n"),
            ("/p/derived.h", "#include \"base.h\"\nstruct Derived : ns::Base {};\n"),
        ]);
        let bases: Vec<String> = supertypes_of(
            &session,
            std::path::Path::new("/p/derived.h"),
            "Derived",
        )
        .into_iter()
        .map(|item| item.name)
        .collect();
        assert_eq!(bases, ["Base"], "`ns::Base` resolves to the class `Base`");
    }
}
