//! **Unreal Engine support against a simulated engine** — the section, the engine's include tree, and the module macros.
//!
//! There is no engine installation on this machine, so the fixture writes a **simulated** one: the include layout the
//! engine really has (`Engine/Source/Runtime/<Module>/Public/<Header>.h`), its reflection macros expanded to nothing,
//! and the handful of core types a game module derives from. A project beside it names the engine in `.cppls.toml` and
//! its module in a `*.uproject`, which is everything the analysis needs to read UE code:
//!
//! ```text
//!   Engine/Source/Runtime/Core/Public/CoreMinimal.h       UCLASS/UPROPERTY/GENERATED_BODY, int32, TArray, FString
//!   Engine/Source/Runtime/CoreUObject/Public/UObject/Object.h    UObject, TObjectPtr
//!   Engine/Source/Runtime/Engine/Public/GameFramework/Actor.h    AActor
//!   MyGame.uproject                                      Modules[].Name = MyGame  → MYGAME_API
//! ```
//!
//! The questions are the ones a reader of UE code has: do the engine's includes resolve, is `MYGAME_API` a name, do the
//! two engine rules still run, and does the class keep its own members.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    DiagnosticCode, DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter,
};

const CORE_MINIMAL: &str = r#"
#pragma once

#define UCLASS(...)
#define USTRUCT(...)
#define UENUM(...)
#define UINTERFACE(...)
#define UPROPERTY(...)
#define UFUNCTION(...)
#define GENERATED_BODY() \
public: \
    static UClass* StaticClass(); \
    static UClass* GetPrivateStaticClass();
#define GENERATED_UCLASS_BODY()
#define GENERATED_USTRUCT_BODY()
#define UE_LOG(CategoryName, Verbosity, Format, ...)
#define check(expr) ((void)0)
#define ensure(expr) (!!(expr))
#define TEXT(x) x
#define FORCEINLINE inline

using int32 = int;
using uint8 = unsigned char;

template <typename T>
class TArray
{
public:
    int32 Num() const { return 0; }
    void Add(const T& Value) {}
};

class FString
{
public:
    FString() {}
    FString(const char* Text) {}
};
"#;

const OBJECT: &str = r#"
#pragma once

#include "CoreMinimal.h"

class UClass;

class UObject
{
public:
    virtual ~UObject() {}
    FString GetName() const { return FString(); }
};

template <typename T>
class TObjectPtr
{
public:
    T* Get() const { return nullptr; }
    T* operator->() const { return nullptr; }
};
"#;

const ACTOR: &str = r#"
#pragma once

#include "CoreMinimal.h"
#include "UObject/Object.h"

class AActor : public UObject
{
public:
    virtual void BeginPlay();
    FString GetActorName() const { return FString(); }
};
"#;

/// A simulated engine tree and a game project beside it.
struct World {
    root: PathBuf,
}

impl World {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "cppls_ue_engine_{name}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let engine = root.join("Engine");
        for (relative, text) in [
            ("Source/Runtime/Core/Public/CoreMinimal.h", CORE_MINIMAL),
            ("Source/Runtime/CoreUObject/Public/UObject/Object.h", OBJECT),
            ("Source/Runtime/Engine/Public/GameFramework/Actor.h", ACTOR),
        ] {
            let path = engine.join(relative);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
            std::fs::write(&path, text).expect("the header");
        }
        let project = root.join("Game");
        std::fs::create_dir_all(project.join("Source/Game")).expect("a project");
        std::fs::write(
            project.join(".cppls.toml"),
            "[unreal]\nengine = \"../Engine\"\n\n[compile]\nstandard = \"c++20\"\n",
        )
        .expect("the configuration");
        std::fs::write(
            project.join("Game.uproject"),
            "{\n  \"FileVersion\": 3,\n  \"Modules\": [ { \"Name\": \"Game\", \"Type\": \"Runtime\" } ]\n}\n",
        )
        .expect("the uproject");
        std::fs::write(
            project.join("Source/Game/Game.generated.h"),
            "#pragma once\n",
        )
        .expect("the generated header");
        World { root: project }
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(&path, text).expect("the source");
        path
    }

    fn session(&self) -> Session<DiskFiles> {
        let mut session = Session::open(
            self.root.clone(),
            SessionFiles::new(OpenDocuments::new(), DiskFiles),
            WatchFilter::new(&self.root),
        );
        let files: Vec<PathBuf> = session
            .project_files()
            .iter()
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| matches!(extension, "h" | "cpp"))
            })
            .cloned()
            .collect();
        session.add_project_files(files);
        session.index_everything();
        let mut steps = 0;
        while session.pending_work() > 0 && steps < 20_000 {
            session.advance(64);
            steps += 1;
        }
        session
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.root.parent().unwrap_or(&self.root));
    }
}

fn findings(session: &Session<DiskFiles>, path: &Path) -> Vec<(DiagnosticCode, String)> {
    session
        .diagnostics(path)
        .map(|found| {
            found
                .checks
                .iter()
                .map(|finding| (finding.code, finding.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// **The engine's includes resolve, the module's API macro is a name, and the class keeps its members.**
#[test]
fn a_game_module_over_a_simulated_engine_reads_and_lints() {
    let world = World::new("clean");
    let header = world.write(
        "Source/Game/MyActor.h",
        r#"
#pragma once

#include "CoreMinimal.h"
#include "GameFramework/Actor.h"
#include "Game.generated.h"

UCLASS()
class GAME_API AMyActor : public AActor
{
    GENERATED_BODY()

public:
    UPROPERTY(EditAnywhere)
    bool bIsReady = false;

    UPROPERTY()
    TObjectPtr<UObject> Target;

    TArray<FString> Names;
};
"#,
    );
    let mut session = world.session();
    session.cook(&header);

    let errors = session
        .diagnostics(&header)
        .map(|found| found.errors.len())
        .unwrap_or(usize::MAX);
    assert_eq!(errors, 0, "the engine's headers and the macros parse");
    assert_eq!(
        findings(&session, &header),
        vec![],
        "a clean module is quiet"
    );

    // The class is a class: its own members are recorded, under the name the macro prefixes.
    let view = session.view(&header).expect("the header is held");
    let Known::Yes(list) = session.members_of(&view, "AMyActor") else {
        panic!("`AMyActor` has members");
    };
    let names: Vec<&str> = list
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    for expected in ["bIsReady", "Target", "Names", "StaticClass"] {
        assert!(names.contains(&expected), "{expected} of {names:?}");
    }

    // **The two members the generated code writes that a macro cannot.** `Super` is the class's first base as its
    // declaration spelled it, `ThisClass` is the class itself — derived from the declaration, not invented.
    let super_member = list
        .members
        .iter()
        .find(|member| member.fact.name == "Super")
        .expect("the generated `Super`");
    assert_eq!(super_member.fact.type_of.as_deref(), Some("AActor"));
    let this_class = list
        .members
        .iter()
        .find(|member| member.fact.name == "ThisClass")
        .expect("the generated `ThisClass`");
    assert_eq!(this_class.fact.type_of.as_deref(), Some("AMyActor"));
}

/// **A class with no base gets no `Super`**, and a project that is not engine code gets neither: the synthesis follows
/// the generated code's own rule (`Super` names a base), not the presence of the macro alone.
#[test]
fn a_class_without_a_base_has_no_super() {
    let world = World::new("no_base");
    let header = world.write(
        "Source/Game/MyData.h",
        r#"
#pragma once

#include "CoreMinimal.h"
#include "Game.generated.h"

USTRUCT()
struct FMyData
{
    GENERATED_BODY()

    int32 Value = 0;
};
"#,
    );
    let mut session = world.session();
    session.cook(&header);
    let view = session.view(&header).expect("the header is held");
    let Known::Yes(list) = session.members_of(&view, "FMyData") else {
        panic!("`FMyData` has members");
    };
    let names: Vec<&str> = list
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert!(names.contains(&"ThisClass"), "{names:?}");
    assert!(!names.contains(&"Super"), "a struct has no base: {names:?}");
}

/// **The two engine rules run on a project that names an engine**, and the engine's own headers are quiet: the
/// `Engine/` tree is outside the workspace, so nothing reports the engine's own spelling of `UCLASS` as a violation.
#[test]
fn the_engine_rules_run_and_the_engine_itself_is_not_judged() {
    let world = World::new("bad");
    let header = world.write(
        "Source/Game/MyActor.h",
        r#"
#pragma once

#include "CoreMinimal.h"
#include "GameFramework/Actor.h"
#include "Game.generated.h"

UCLASS()
class MyActor : public AActor
{
public:
    UPROPERTY()
    bool isReady = false;
};
"#,
    );
    let mut session = world.session();
    session.cook(&header);
    let found = findings(&session, &header);
    assert_eq!(
        found,
        vec![
            (DiagnosticCode::UeNaming, "MyActor".to_string()),
            (DiagnosticCode::UeGenerateBody, "MyActor".to_string()),
            (DiagnosticCode::UeNaming, "isReady".to_string()),
        ],
        "{found:?}"
    );
}

/// **`engine` alone turns the rules on** — naming the engine is saying what the project is, and a project that forgot
/// `enable` would otherwise get the headers but not the rules.
#[test]
fn naming_the_engine_turns_the_conventions_on() {
    let world = World::new("implied");
    let header = world.write(
        "Source/Game/MyActor.h",
        r#"
#include "CoreMinimal.h"
#include "Game.generated.h"

UCLASS()
class MyActor
{
public:
    bool isReady = false;
};
"#,
    );
    let mut session = world.session();
    session.cook(&header);
    let found = findings(&session, &header);
    assert!(
        found
            .iter()
            .any(|(code, _)| *code == DiagnosticCode::UeNaming),
        "the conventions ran: {found:?}"
    );
}
