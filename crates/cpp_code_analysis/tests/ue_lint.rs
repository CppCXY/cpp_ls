//! **The Unreal rules** — `ue-naming` and `ue-generate-body`, and the `[unreal]` switch that turns them on.
//!
//! These tests write a project **on disk**, because the switch is a project's configuration and the annotations are read
//! from the file's own text: `UCLASS()` and `GENERATED_BODY()` are empty macros, so the rendering no longer holds them —
//! measured with the simulated engine headers, `UCLASS()` disappeared from the tree and only the class remained.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    DiagnosticCode, DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// A project of one header, with the simulated engine macros spelled out and the given configuration body.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str, configuration: &str, header: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "cppls_ue_{name}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a directory to write the fixture into");
        std::fs::write(root.join(".cppls.toml"), configuration).expect("the configuration");
        std::fs::write(root.join("game.h"), header).expect("the header");
        Project { root }
    }

    fn session(&self) -> Session<DiskFiles> {
        let mut session = Session::open(
            self.root.clone(),
            SessionFiles::new(OpenDocuments::new(), DiskFiles),
            WatchFilter::new(&self.root),
        );
        session.add_project_files([self.root.join("game.h")]);
        session.index_everything();
        let mut steps = 0;
        while session.pending_work() > 0 && steps < 20_000 {
            session.advance(64);
            steps += 1;
        }
        session.cook(self.root.join("game.h").as_path());
        session
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The findings of the two engine rules, as `(code, name)` pairs.
fn engine_findings(session: &Session<DiskFiles>, path: &Path) -> Vec<(DiagnosticCode, String)> {
    session
        .diagnostics(path)
        .map(|found| {
            found
                .checks
                .iter()
                .filter(|finding| finding.code.is_unreal_only())
                .map(|finding| (finding.code, finding.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The macros a UE project has, expanded to nothing, exactly as the engine's headers leave them.
const ENGINE: &str = "\
#define UCLASS(...)
#define USTRUCT(...)
#define UENUM(...)
#define UINTERFACE(...)
#define UPROPERTY(...)
#define UFUNCTION(...)
#define GENERATED_BODY()
";

const ENABLED: &str = "[unreal]\nenable = true\n";

/// **A `UCLASS` whose name has no engine prefix and whose body has no `GENERATED_BODY()`** — both rules report, at the
/// positions the file has them.
#[test]
fn a_ue_class_is_told_about_its_prefix_and_its_missing_body() {
    let project = Project::new(
        "bad",
        ENABLED,
        &format!(
            "{ENGINE}
UCLASS()
class MyActor
{{
public:
    UPROPERTY()
    bool isReady = false;
}};
"
        ),
    );
    let session = project.session();
    let findings = engine_findings(&session, &project.root.join("game.h"));
    assert_eq!(
        findings,
        vec![
            (DiagnosticCode::UeNaming, "MyActor".to_string()),
            (DiagnosticCode::UeGenerateBody, "MyActor".to_string()),
            (DiagnosticCode::UeNaming, "isReady".to_string()),
        ],
        "{findings:?}"
    );
}

/// **The same code with the engine's conventions followed** is silent — the other half of the rule, and the one that
/// makes it usable: `A`/`U`/`F`/`E`, `b` on a boolean, and the body macro written.
#[test]
fn a_ue_class_that_follows_the_conventions_is_silent() {
    let project = Project::new(
        "clean",
        ENABLED,
        &format!(
            "{ENGINE}
UCLASS()
class AMyActor
{{
    GENERATED_BODY()
public:
    UPROPERTY()
    bool bIsReady = false;
}};

USTRUCT()
struct FMyData
{{
    GENERATED_BODY()
    int32 Value = 0;
}};

UENUM()
enum class EMyState : uint8
{{
    Idle
}};
"
        ),
    );
    let session = project.session();
    let findings = engine_findings(&session, &project.root.join("game.h"));
    assert!(findings.is_empty(), "{findings:?}");
}

/// **Without `[unreal]` the rules say nothing**, however the file is written — the switch is the project's, and an
/// ordinary project that happens to define `UCLASS` (or does not) must not be told about Epic's prefixes.
#[test]
fn the_engine_rules_are_off_without_the_switch() {
    let engine = format!(
        "{ENGINE}
UCLASS()
class MyActor
{{
public:
    bool isReady = false;
}};
"
    );
    for configuration in ["", "[unreal]\nenable = false\n"] {
        let project = Project::new("off", configuration, &engine);
        let session = project.session();
        let findings = engine_findings(&session, &project.root.join("game.h"));
        assert!(findings.is_empty(), "`{configuration}`: {findings:?}");
        // The file still parses: the macros are the project's own.
        let errors = session
            .diagnostics(project.root.join("game.h"))
            .map(|found| found.errors.len())
            .unwrap_or(usize::MAX);
        assert_eq!(errors, 0);
    }
}

/// **The names the rules answer to**, because a project switches them off by name in `[diagnostics] disable`.
#[test]
fn the_engine_rules_are_named() {
    assert_eq!(DiagnosticCode::UeNaming.name(), "ue-naming");
    assert_eq!(DiagnosticCode::UeGenerateBody.name(), "ue-generate-body");
}
