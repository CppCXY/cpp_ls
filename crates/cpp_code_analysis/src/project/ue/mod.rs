//! **Unreal Engine** — everything the analysis knows about it, in one module.
//!
//! ```text
//! .cppls.toml                 [unreal] enable = true, engine = "…"
//! Engine/Source/Runtime/…     the engine's own headers (`Core/Public`, `Engine/Public`, `CoreUObject/Public`, …)
//! Source/<Module>/…           the project's modules (named by the project's `*.uproject`)
//! Intermediate/Build/…/UHT    the reflection headers the build tool generated
//! ```
//!
//! # What is here today
//!
//! [`UnrealSection`] — the `[unreal]` section of `.cppls.toml` — and [`UnrealSection::applied_to`], which turns it
//! into the compiler configuration an engine's headers need:
//!
//! ```text
//! engine = "…"    where the engine is installed. Every `Engine/Source/Runtime/<Module>/{Public,Classes}` becomes an
//!                 include directory, so `#include "CoreMinimal.h"` and `#include "GameFramework/Actor.h"` resolve
//!                 the way they do in a build
//! enable          the two engine rules run; the macros below are defined either way, because a project that names the
//!                 engine is engine code whether or not it also switched the rules on
//! ```
//!
//! **The macros an engine build defines are supplied by this layer**, not by the headers: `WITH_EDITOR`,
//! `WITH_EDITORONLY_DATA`, `WITH_ENGINE`, `UE_BUILD_DEVELOPMENT`, `UE_SERVER`, `IS_PROGRAM` and the platform the server
//! runs on. And from the project's `*.uproject`, one `<MODULE>_API` per module, defined as nothing — the engine's build
//! tool is what makes it a visibility attribute, and a reader that saw `class MYGAME_API AActor` without it would report
//! an unknown name. A define the project states itself wins: neither the section nor the engine may overwrite what a
//! compile command said.
//!
//! # Why a project's decision, and not a detection
//!
//! A file that writes `UCLASS()` outside UE is a file with an unknown macro, not a game, and running Epic's naming
//! rules on an engine-agnostic project would report every `class Widget` as a missing `A`/`U`/`F`.
//!
//! # What will be here
//!
//! ```text
//! discovery     the module list from `*.Build.cs` (the `.uproject` names the targets), the generated include
//!               directories under `Intermediate/Build/<Platform>/<Config>/Inc/<Module>/UHT`, and the engine version
//! reading       the reflection annotations — UCLASS/USTRUCT/UENUM/UINTERFACE/UPROPERTY/UFUNCTION and their
//!               specifiers — which are **empty macros** and therefore have to be read from the written text
//! members       what GENERATED_BODY() stands for: StaticClass(), GetPrivateStaticClass(), Super, ThisClass
//! ```
//!
//! **The rules themselves do not live here.** One check per code, in
//! [`crate::sema::check`](crate::sema::check) — `ue_naming`, `ue_generate_body` — like every other rule, because that is
//! where a rule is found by its code and where its tests are. What this module owns is the *answers* those rules read,
//! so that two rules cannot disagree about which class a `UCLASS()` annotates.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// **`[unreal]`** — whether this project is Unreal Engine code.
///
/// ```toml
/// [unreal]
/// enable = true
/// engine = "C:/Program Files/Epic Games/UE_5.4"
/// ```
///
/// `enable` alone is enough for a project whose headers already say where the engine is (a compile database with the
/// `-I` entries UBT generates); `engine` is what makes a project without a database — or without the engine installed
/// where the build says — resolve its includes, and it turns the rules on as well.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnrealSection {
    /// Run the Unreal conventions — see `ue-naming` and `ue-generate-body`.
    #[serde(default)]
    pub enable: bool,
    /// **The engine's installation root** — the directory that holds `Engine/Source/Runtime`, or a plain directory of
    /// headers. A relative path is relative to the workspace root, like every other path in `.cppls.toml`.
    #[serde(default)]
    pub engine: Option<PathBuf>,
}

impl UnrealSection {
    /// **Is this project engine code at all?** — the rules run, and the engine's macros are defined.
    ///
    /// `engine` implies it: naming the engine is saying what the project is, and a reader who had to write both keys
    /// would eventually write one.
    pub fn is_on(&self) -> bool {
        self.enable || self.engine.is_some()
    }

    /// **The engine's installation root, as an absolute directory** — the section's own path, or the directory under
    /// the workspace that holds `Engine/Source/Runtime`.
    ///
    /// **Two spellings are accepted for `engine`**: the installation root (`…/UE_5.4`, which contains `Engine/`) and
    /// that `Engine` directory itself (`…/UE_5.4/Engine`), because which one a person calls "the engine" is not
    /// something this layer can know and both are unambiguous on disk. The answer is always the installation root, so
    /// that everything below it has one spelling.
    ///
    /// The workspace's own tree is the fallback — a project checked out inside an engine, or beside one — and it is a
    /// **guess** rather than a fact, which is why it is only taken when it exists.
    pub fn engine_root(&self, root: &Path) -> Option<PathBuf> {
        if let Some(named) = self.engine.as_ref() {
            let named = if named.is_absolute() {
                named.clone()
            } else {
                root.join(named)
            };
            if named.join("Engine/Source/Runtime").is_dir() {
                return Some(named);
            }
            // The `Engine` directory itself: the parent is the installation root.
            if named.join("Source/Runtime").is_dir()
                && let Some(parent) = named.parent()
            {
                return Some(parent.to_path_buf());
            }
            // A directory of headers rather than an installation: used as it is, as an include root.
            return Some(named);
        }
        [root.to_path_buf(), root.parent()?.to_path_buf()]
            .into_iter()
            .find(|candidate| candidate.join("Engine/Source/Runtime").is_dir())
    }

    /// **The configuration this section describes**, applied to what discovery worked out.
    ///
    /// One function rather than a handful of edits in the session, for the same reason [`CudaSection`] has one: the
    /// keys are one statement — naming an engine means "and its headers resolve, and its macros are defined" — and
    /// splitting that across call sites is how one of the three would eventually be forgotten.
    ///
    /// [`CudaSection`]: crate::project::cuda::CudaSection
    pub fn applied_to(
        &self,
        mut config: crate::include::config::CompilerConfig,
        root: &Path,
    ) -> crate::include::config::CompilerConfig {
        if !self.is_on() {
            return config;
        }
        if let Some(engine) = self.engine_root(root) {
            for include in engine_include_directories(&engine) {
                config
                    .include_paths
                    .push(crate::include::config::IncludePath::system(include));
            }
        }
        // **What an engine build defines.** Nothing here overwrites a define the project stated: a compile database
        // that says `WITH_EDITOR=0` is describing the build that exists, and a guess about the editor must not win.
        for (name, value) in engine_defines() {
            define_if_absent(&mut config, name, value);
        }
        // **One `<MODULE>_API` per module the project names.** `MYGAME_API` is what the build tool turns into a
        // visibility attribute; to a reader it expands to nothing, and a class that writes it must still parse.
        for module in game_modules(root) {
            // **`MyGame` becomes `MYGAME_API`**, which is how the build tool spells the visibility macro of a module.
            define_if_absent(&mut config, &format!("{}_API", module.to_uppercase()), "");
        }
        config
    }
}

/// **What `GENERATED_BODY()` writes into a class that a macro cannot** — the two names that need the class and its base.
///
/// The engine's build tool writes `typedef <Base> Super;` and `typedef <ThisClass> ThisClass;` into the generated code,
/// which is why a macro in the engine's own header can declare `StaticClass()` (no name needed) and not these two. A
/// language server that only reads the macro therefore answers `Super` and `ThisClass` as unknown names — measured on
/// `UE-like`, where `Super::BeginPlay()` resolved to nothing.
///
/// The answer is derived, not invented: `Super` **is** the class's first base as its declaration wrote it, and
/// `ThisClass` **is** the class. Returned as `(name, type)` in the order the generated code writes them.
pub fn generated_members(class_name: &str, bases: &[String]) -> Vec<(&'static str, String)> {
    let mut members = vec![("ThisClass", class_name.to_string())];
    if let Some(base) = bases.first() {
        members.push(("Super", base.clone()));
    }
    members
}

/// **The include directories an engine root offers**, in the order a build would read them.
///
/// The real layout is `Engine/Source/Runtime/<Module>/Public` (the module's headers) and, for some modules,
/// `<Module>/Classes` (the assets-facing part, which is where `GameFramework/Actor.h` lives in a UE4 tree). Both are
/// added when they exist, sorted by module so that the search order does not depend on the filesystem's.
///
/// A directory that does not have `Engine/Source/Runtime` is used **as an include root itself**: that is what a
/// simulation or a hand-made header set looks like, and refusing it would make `[unreal] engine` mean "the whole
/// installation or nothing" for no reason.
fn engine_include_directories(engine: &Path) -> Vec<PathBuf> {
    let runtime = engine.join("Engine/Source/Runtime");
    if !runtime.is_dir() {
        return vec![engine.to_path_buf()];
    }
    let mut modules: Vec<PathBuf> = std::fs::read_dir(&runtime)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default();
    modules.sort();
    let mut directories = Vec::new();
    for module in modules {
        for relative in ["Public", "Classes"] {
            let directory = module.join(relative);
            if directory.is_dir() {
                directories.push(directory);
            }
        }
    }
    directories
}

/// **The macros an engine build defines**, as (name, value).
///
/// A deliberately short list, and each entry is a statement about the build a language server is describing: the engine's
/// **editor** configuration, on the platform the server runs on. `__CUDA_ARCH__`'s lesson applies here too — a macro
/// that is only defined in a pass a compiler never sees would make the analysis read a different program — so nothing
/// that depends on the *target* is defined beyond the host's own platform.
fn engine_defines() -> Vec<(&'static str, &'static str)> {
    let mut defines = vec![
        ("WITH_EDITOR", "1"),
        ("WITH_EDITORONLY_DATA", "1"),
        ("WITH_ENGINE", "1"),
        ("UE_BUILD_DEVELOPMENT", "1"),
        ("UE_SERVER", "0"),
        ("IS_PROGRAM", "0"),
    ];
    if cfg!(windows) {
        defines.push(("PLATFORM_WINDOWS", "1"));
        defines.push(("PLATFORM_DESKTOP", "1"));
    } else if cfg!(target_os = "macos") {
        defines.push(("PLATFORM_MAC", "1"));
        defines.push(("PLATFORM_DESKTOP", "1"));
    } else {
        defines.push(("PLATFORM_LINUX", "1"));
        defines.push(("PLATFORM_DESKTOP", "1"));
    }
    defines
}

/// **Define a macro unless the project already said what it is.**
fn define_if_absent(config: &mut crate::include::config::CompilerConfig, name: &str, value: &str) {
    let taken = config
        .defines
        .iter()
        .any(|defined| defined.name.as_ref() == name)
        || config
            .undefines
            .iter()
            .any(|undefined| undefined.as_ref() == name);
    if taken {
        return;
    }
    let value = if value.is_empty() { None } else { Some(value) };
    config.defines.push(match value {
        Some(value) => {
            crate::include::config::CommandLineMacro::with_value(name.to_string(), value)
        }
        // `-DFOO` is `1`; an **empty** value is a different macro, and this is the only spelling the type has for it.
        None => crate::include::config::CommandLineMacro::with_value(name.to_string(), ""),
    });
}

/// **The modules the project's `*.uproject` names** — `["MyGame"]` for a game module, and one per plugin's module too.
///
/// Read rather than run: the file is a small JSON object, and the only field this layer needs is `Modules[].Name`. A
/// missing or unreadable file is not an error here — the engine's headers still resolve, and the `*_API` macros are the
/// one thing lost.
fn game_modules(root: &Path) -> Vec<String> {
    let uproject = match std::fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "uproject")
            }),
        Err(_) => None,
    };
    let Some(uproject) = uproject else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&uproject) else {
        return Vec::new();
    };
    module_names_in(&text)
}

/// **The `Modules[].Name` entries of a `.uproject`'s text.**
///
/// A hand-written scan rather than a JSON parser, for the same reason the compile database has one: this crate has no
/// JSON dependency, the shape asked for is one array of one field, and a parser that rejects the file over a comment or
/// a trailing comma would lose the module names for a reason that has nothing to do with them.
fn module_names_in(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let Some(modules) = text.find("\"Modules\"") else {
        return names;
    };
    // The array that follows, and its end — `Modules` is the only array this reads, so the depth is all the state.
    let Some(open) = text[modules..].find('[').map(|at| modules + at) else {
        return names;
    };
    let mut depth = 0usize;
    let mut index = open;
    let bytes = text.as_bytes();
    while index < bytes.len() {
        match bytes[index] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            b'"' => {
                // A string key or value: `"Name"` followed by `:` then the value.
                let start = index + 1;
                let Some(end) = text[start..].find('"').map(|at| start + at) else {
                    break;
                };
                let word = &text[start..end];
                if word == "Name" {
                    let rest = &text[end + 1..];
                    if let Some(value) = quoted_value_after_colon(rest) {
                        names.push(value);
                    }
                }
                index = end;
            }
            _ => {}
        }
        index += 1;
    }
    names
}

/// The string after a `:` in `rest`, when the next non-space characters are `: "…"`.
fn quoted_value_after_colon(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uprojects_modules_are_read() {
        let text = "{\n  \"FileVersion\": 3,\n  \"Modules\": [\n    { \"Name\": \"MyGame\", \"Type\": \"Runtime\" },\n    \
                    { \"Name\": \"MyGameEditor\", \"Type\": \"Editor\" }\n  ]\n}\n";
        assert_eq!(module_names_in(text), vec!["MyGame", "MyGameEditor"]);
    }

    #[test]
    fn a_file_without_modules_names_none() {
        assert!(module_names_in("{\"FileVersion\": 3}").is_empty());
        assert!(module_names_in("").is_empty());
    }

    #[test]
    fn an_engine_root_with_the_real_layout_offers_its_modules() {
        let root = std::env::temp_dir().join(format!("cppls_ue_engine_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Engine/Source/Runtime/Core/Public")).expect("a tree");
        std::fs::create_dir_all(root.join("Engine/Source/Runtime/Engine/Public")).expect("a tree");
        std::fs::create_dir_all(root.join("Engine/Source/Runtime/Engine/Classes")).expect("a tree");
        let directories = engine_include_directories(&root);
        assert_eq!(
            directories,
            vec![
                root.join("Engine/Source/Runtime/Core/Public"),
                root.join("Engine/Source/Runtime/Engine/Public"),
                root.join("Engine/Source/Runtime/Engine/Classes"),
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_plain_header_directory_is_an_include_root() {
        let root = std::env::temp_dir().join(format!("cppls_ue_stub_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a tree");
        assert_eq!(engine_include_directories(&root), vec![root.clone()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_project_that_names_the_engine_is_engine_code() {
        let section = UnrealSection {
            enable: false,
            engine: Some(PathBuf::from("Engine")),
        };
        assert!(section.is_on());
        let off = UnrealSection::default();
        assert!(!off.is_on());
    }
}
