//! Microsoft's toolchain: where it is, what its `INCLUDE` is, and what it predefines.
//!
//! Every claim in this module is measured; [`docs/msvc-notes.md`](../../../../docs/msvc-notes.md) is the fact sheet,
//! with the command and the raw output behind each one. The five that shaped the code:
//!
//! ```text
//! vswhere -latest -format json          ~24 ms, and the only call needed to find the IDE's install directory
//! VC\Tools\MSVC\<version>               the toolset, enumerated from disk rather than asked for
//! INCLUDE, built by hand                ~50 ms for the macro dump, against ~1300 ms through vcvars64.bat
//! cl /nologo /Zc:preprocessor /PD /c    the macros; `/PD` alone is ignored at exit 0 with no output at all
//! exit 1 / 2 / 4 / 0                    "could not ask" / "rejected the command line" / "no resources" / "fine"
//! ```
//!
//! # Why `vcvars64.bat` is not run
//!
//! Because it costs 1.3 seconds of every session's startup and buys nothing this needs: it sets `INCLUDE`, `LIB`
//! and `PATH`, and of those only `INCLUDE` decides what the *analysis* can read. So the eight directories are built
//! here, in the order `vcvars64` produces them, and the compiler is run with `INCLUDE` in its environment. Linking
//! is not attempted (`/c`), so `LIB` is not needed.
//!
//! # Why nothing is parsed out of a message
//!
//! Because `cl`'s diagnostics are localized and the machine this was written on has only the Chinese resources —
//! `VSLANG=1033` does nothing, and `/showIncludes`'s marker is `注意: 包含文件:`. Text is therefore never matched:
//! the exit status is the signal, and the `D####`/`C####` codes are the only other thing worth looking at.
//!
//! # Why `cl.exe` is never copied
//!
//! Because it loads `clui.dll` from a `2052\` (or `1033\`) directory beside itself, and a copy without it fails
//! with `C1510` — measured, exit 4. The path found here is the path used.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::include::config::CommandLineMacro;
use crate::include::toolchain::{CommandRunner, Output};

/// What every `vswhere`-based question needs: where Windows keeps its programs.
///
/// Passed in rather than read from the process, for the same reason [`Environment`](crate::Environment) is data:
/// "which installs are there" has to be testable without the machine having any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowsLayout {
    /// `%ProgramFiles(x86)%` first, then `%ProgramFiles%`: the installer lives under the 32-bit one even on a
    /// 64-bit machine, and a machine without that variable set is one where the other is worth trying.
    pub program_files: Vec<PathBuf>,
    /// `%WindowsSdkDir%`, when a developer prompt set it.
    pub windows_sdk_dir: Option<PathBuf>,
    /// `%VCToolsInstallDir%`, when a developer prompt set it.
    pub vc_tools_install_dir: Option<PathBuf>,
}

impl WindowsLayout {
    /// The process environment, on Windows. Empty everywhere else.
    pub fn current() -> Self {
        if !cfg!(windows) {
            return WindowsLayout::default();
        }

        let mut program_files = Vec::new();
        for name in ["ProgramFiles(x86)", "ProgramFiles", "ProgramW6432"] {
            if let Some(value) = std::env::var_os(name) {
                let path = PathBuf::from(value);
                if !program_files.contains(&path) {
                    program_files.push(path);
                }
            }
        }

        WindowsLayout {
            program_files,
            windows_sdk_dir: std::env::var_os("WindowsSdkDir").map(PathBuf::from),
            vc_tools_install_dir: std::env::var_os("VCToolsInstallDir").map(PathBuf::from),
        }
    }

    /// `vswhere.exe`, if it is where the installer puts it.
    pub fn vswhere(&self) -> Option<PathBuf> {
        self.program_files.iter().find_map(|program_files| {
            let candidate = program_files
                .join("Microsoft Visual Studio")
                .join("Installer")
                .join("vswhere.exe");
            candidate.is_file().then_some(candidate)
        })
    }
}

/// A Visual Studio installation, as `vswhere` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualStudio {
    pub installation: PathBuf,
    /// `installationVersion` — `17.5.33424.131` on the machine this was measured on. For a human; nothing decides
    /// anything with it.
    pub version: Option<String>,
}

/// One MSVC toolset: the directory whose `include` is the standard library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolset {
    pub root: PathBuf,
    /// The directory's own name, `14.35.32215`: the version is in the path, so no call is needed to learn it.
    pub version: String,
    /// `cl.exe`, for the host/target pair this analysis runs on.
    pub cl: PathBuf,
}

/// One Windows SDK: the directory whose `Include\<version>` holds `ucrt`, `um`, `shared` and the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsSdk {
    pub root: PathBuf,
    pub version: String,
}

/// A toolchain found on Windows: the toolset, the SDK, and the search list that joins them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msvc {
    pub toolset: Toolset,
    pub sdk: Option<WindowsSdk>,
    /// The `INCLUDE` a developer prompt would set, in its order, with any directory that does not exist left out.
    pub include: Vec<PathBuf>,
    /// A developer prompt's own `INCLUDE` was already set, so it was used as it is.
    pub from_environment: bool,
}

impl Msvc {
    /// The environment a `cl` invocation needs to see this search list.
    pub fn environment(&self) -> Vec<(OsString, OsString)> {
        if self.from_environment {
            // The process already has it; re-setting it would be a second way of saying the same thing.
            return Vec::new();
        }

        let joined = std::env::join_paths(&self.include).unwrap_or_default();
        vec![(OsString::from("INCLUDE"), joined)]
    }

    /// The include directories, as the analysis wants them: **system** paths, in search order.
    pub fn include_paths(&self) -> Vec<PathBuf> {
        self.include.clone()
    }
}

/// Find the toolchain, without running anything but `vswhere`.
///
/// The order, and each step is a weaker claim than the one before it:
///
/// ```text
/// 1. `INCLUDE` from the environment   a developer prompt is the project's own statement of its toolchain
/// 2. `%VCToolsInstallDir%`            set by vcvars, so: the same thing, for a process that inherited it
/// 3. vswhere → the newest toolset and the newest SDK
/// ```
///
/// `None` when there is no Visual Studio at all — which on a machine with a MinGW toolchain is the ordinary case,
/// not a failure.
pub fn discover(
    runner: &impl CommandRunner,
    layout: &WindowsLayout,
) -> Option<Msvc> {
    if !cfg!(windows) {
        return None;
    }

    // A developer prompt has already answered the question, and answered it better than any reconstruction: use it.
    let from_prompt: Vec<PathBuf> = std::env::var_os("INCLUDE")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    if !from_prompt.is_empty() {
        let toolset = layout
            .vc_tools_install_dir
            .as_ref()
            .and_then(|root| toolset_at(root))
            .or_else(|| newest_toolset(&newest_installation(runner, layout)?));
        let sdk = layout
            .windows_sdk_dir
            .as_ref()
            .and_then(|root| newest_sdk(root))
            .or_else(|| find_sdk(&layout.program_files));

        if let Some(toolset) = toolset {
            return Some(Msvc {
                toolset,
                sdk,
                include: from_prompt,
                from_environment: true,
            });
        }
    }

    let installation = newest_installation(runner, layout)?;
    let toolset = newest_toolset(&installation)?;
    let sdk = find_sdk(&layout.program_files);

    let include = include_directories(&installation, &toolset, sdk.as_ref());
    if include.is_empty() {
        return None;
    }

    Some(Msvc {
        toolset,
        sdk,
        include,
        from_environment: false,
    })
}

/// The newest Visual Studio with the C++ toolset installed, asked of `vswhere`.
///
/// One call, JSON, ~24 ms: the same cost as asking for a single property, and the only form that also gives the
/// version — see the fact sheet. `-requires` is what makes the answer mean "and it has the C++ tools", which is
/// the question here; an install without them is skipped by `vswhere` rather than filtered afterwards.
pub fn newest_installation(
    runner: &impl CommandRunner,
    layout: &WindowsLayout,
) -> Option<VisualStudio> {
    let vswhere = layout.vswhere()?;

    let output = runner.run(
        &vswhere,
        &[
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-format",
            "json",
        ],
        &[],
    )?;

    if !output.succeeded {
        return None;
    }

    let installation = json_string_field(&output.stdout, "installationPath")?;

    Some(VisualStudio {
        installation: PathBuf::from(installation),
        version: json_string_field(&output.stdout, "installationVersion"),
    })
}

/// Every MSVC toolset under an installation, newest first.
pub fn toolsets(installation: &Path) -> Vec<Toolset> {
    let root = installation.join("VC").join("Tools").join("MSVC");

    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };

    let mut found: Vec<Toolset> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| toolset_at(&entry.path()))
        .collect();

    // Newest first, by the version segments rather than by spelling: `14.9` comes after `14.35` as text and before
    // it as a version.
    found.sort_by(|one, two| compare_versions(&two.version, &one.version));
    found
}

/// The newest toolset under an installation.
pub fn newest_toolset(installation: &VisualStudio) -> Option<Toolset> {
    toolsets(&installation.installation).into_iter().next()
}

/// One toolset directory, if it is one and it has a `cl.exe` for this machine.
fn toolset_at(root: &Path) -> Option<Toolset> {
    let version = root.file_name()?.to_str()?.to_string();
    if !root.is_dir() {
        return None;
    }

    let cl = cl_for_this_host(root)?;

    Some(Toolset {
        root: root.to_path_buf(),
        version,
        cl,
    })
}

/// `cl.exe` for the host/target pairs that exist, in order of what this analysis needs.
///
/// A 64-bit host targeting 64-bit code first, then the 32-bit-host equivalents: the *target* decides the
/// predefined macros (`_WIN64`, `_M_X64`) that conditions are evaluated against, and a 64-bit target is the one a
/// modern project builds. A missing pair is not an error — ARM64 does not exist on the machine this was measured
/// on, and a machine with only `Hostx86` is a machine with a 32-bit toolset.
fn cl_for_this_host(toolset: &Path) -> Option<PathBuf> {
    const PAIRS: &[(&str, &str)] = &[
        ("Hostx64", "x64"),
        ("Hostx64", "x86"),
        ("Hostx86", "x64"),
        ("Hostx86", "x86"),
    ];

    PAIRS.iter().find_map(|(host, target)| {
        let candidate = toolset
            .join("bin")
            .join(host)
            .join(target)
            .join("cl.exe");
        candidate.is_file().then_some(candidate)
    })
}

/// Windows SDKs, newest first, from either the environment's `WindowsSdkDir` or the installer's default place.
pub fn windows_sdks(layout: &WindowsLayout) -> Vec<WindowsSdk> {
    let mut roots: Vec<PathBuf> = Vec::new();

    if let Some(sdk_dir) = &layout.windows_sdk_dir {
        roots.push(sdk_dir.clone());
    }
    for program_files in &layout.program_files {
        roots.push(program_files.join("Windows Kits").join("10"));
    }

    let mut found: Vec<WindowsSdk> = Vec::new();

    for root in roots {
        let Ok(entries) = std::fs::read_dir(root.join("Include")) else {
            continue;
        };

        for entry in entries.flatten() {
            let Some(version) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };

            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && !found.iter().any(|known| known.version == version)
            {
                found.push(WindowsSdk {
                    root: root.clone(),
                    version,
                });
            }
        }
    }

    found.sort_by(|one, two| compare_versions(&two.version, &one.version));
    found
}

/// The newest SDK.
pub fn newest_sdk(root: &Path) -> Option<WindowsSdk> {
    let layout = WindowsLayout {
        program_files: Vec::new(),
        windows_sdk_dir: Some(root.to_path_buf()),
        vc_tools_install_dir: None,
    };

    windows_sdks(&layout).into_iter().next()
}

/// The newest SDK a machine has, from the installer's default location.
fn find_sdk(program_files: &[PathBuf]) -> Option<WindowsSdk> {
    windows_sdks(&WindowsLayout {
        program_files: program_files.to_vec(),
        windows_sdk_dir: None,
        vc_tools_install_dir: None,
    })
    .into_iter()
    .next()
}

/// The eight directories `vcvars64.bat` produces, in its order, without the ones that are not there.
///
/// Measured order (the fact sheet prints the raw `INCLUDE`): the toolset's own `include`, ATL's, the VS auxiliary
/// one, then the SDK's `ucrt`, `um`, `shared`, `winrt`, `cppwinrt`. The SDK portion is what a project actually
/// needs — `ucrt` holds the C library, `um` the Win32 headers — and a directory that does not exist is left out
/// rather than passed on: `cl` accepts an `INCLUDE` entry that is not there, so a missing one would be invisible.
fn include_directories(
    installation: &VisualStudio,
    toolset: &Toolset,
    sdk: Option<&WindowsSdk>,
) -> Vec<PathBuf> {
    let mut include = Vec::new();

    let mut push = |path: PathBuf| {
        if path.is_dir() && !include.contains(&path) {
            include.push(path);
        }
    };

    push(toolset.root.join("include"));
    push(toolset.root.join("ATLMFC").join("include"));
    push(
        installation
            .installation
            .join("VC")
            .join("Auxiliary")
            .join("VS")
            .join("include"),
    );

    if let Some(sdk) = sdk {
        let version = sdk.root.join("Include").join(&sdk.version);
        for part in ["ucrt", "um", "shared", "winrt", "cppwinrt"] {
            push(version.join(part));
        }
    }

    include
}

/// Ask `cl` what it predefines.
///
/// The invocation is the one the fact sheet measured: `/nologo` (no banner), `/Zc:preprocessor` (without it `/PD`
/// is **ignored at exit 0** — the trap that looks like success), `/PD` (print the macro table), `/c` (do not link;
/// without it the run continues into the linker and appends `LNK1561`), and a **real file**, because `cl` does not
/// read standard input (`-` and `/Tc-` both fail with exit 2).
///
/// `None` when the answer could not be obtained. The exit status is what decides that: `1` means the process could
/// not be started properly, `2` means `cl` rejected the command line or could not find headers, `4` means it could
/// not load its resources. All three are "could not ask", and none of them is "predefines nothing".
pub fn predefined_macros(
    runner: &impl CommandRunner,
    msvc: &Msvc,
    standard: Option<&str>,
) -> Option<Vec<CommandLineMacro>> {
    let scratch = Scratch::new()?;
    let environment = msvc.environment();

    // `/std:c++20` **and** `/Zc:__cplusplus`: measured, `/std:` alone leaves `__cplusplus` at `199711L` (MSVC's
    // historical value) and only `_MSVC_LANG` moves. `__cplusplus` is what half the standard library's feature
    // tests ask about, so the flag that makes it true is worth passing.
    let requested = standard.map(|standard| format!("/std:{standard}"));
    let object = scratch.path.join("predefined.obj");

    let mut arguments: Vec<&str> = vec!["/nologo", "/Zc:preprocessor", "/Zc:__cplusplus", "/PD", "/c"];
    if let Some(requested) = &requested {
        arguments.push(requested);
    }
    let object_argument = format!("/Fo{}", object.display());
    arguments.push(&object_argument);
    let source_argument = scratch.source.to_string_lossy().into_owned();
    arguments.push(&source_argument);

    let output: Output = runner.run(&msvc.toolset.cl, &arguments, &environment)?;

    if !output.succeeded {
        return None;
    }

    let mut macros = crate::include::toolchain::parse_builtin_macros(&output.stdout);
    // Measured: the table comes out in hash order, so two runs of the same compiler would otherwise produce two
    // orders — and a configuration that differs between two runs is one whose comparisons mean nothing.
    macros.sort_by(|one, two| one.name.cmp(&two.name));
    macros.dedup_by(|one, two| one.name == two.name);

    (!macros.is_empty()).then_some(macros)
}

/// `MSVC <toolset> (_MSC_VER <n>)` — a version line for a human, built from facts already in hand.
///
/// Deliberately not `/Bv`: that is another 62 ms and another process, to print what the toolset's directory name
/// and the macro table already say (`docs/msvc-notes.md`, *Timing*).
pub fn version_line(msvc: &Msvc, macros: &[CommandLineMacro]) -> String {
    let value_of = |name: &str| {
        macros
            .iter()
            .find(|define| define.name.as_ref() == name)
            .and_then(|define| define.value.as_deref())
    };

    match value_of("_MSC_FULL_VER").or_else(|| value_of("_MSC_VER")) {
        Some(version) => format!("MSVC {} (_MSC_FULL_VER {version})", msvc.toolset.version),
        None => format!("MSVC {}", msvc.toolset.version),
    }
}

/// A temporary directory with one empty translation unit in it.
///
/// Because `cl` cannot be asked anything without a file, and because it writes its object file **next to the
/// source** unless told otherwise — the fact sheet records `empty.obj` appearing in the directory the compiler was
/// started in. So the file, the object and everything else go in a directory of their own, removed when this is
/// dropped.
struct Scratch {
    path: PathBuf,
    source: PathBuf,
}

impl Scratch {
    fn new() -> Option<Scratch> {
        let unique = format!(
            "cppls-msvc-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        );

        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).ok()?;

        let source = path.join("predefined.cpp");
        // Empty: the macro table of a translation unit that includes nothing *is* the compiler's predefined set.
        // (A file that included a header would print that header's macros too — measured, 1540 lines for `<cstdio>`.)
        std::fs::write(&source, "").ok()?;

        Some(Scratch { path, source })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Compare two dotted version strings, numerically.
fn compare_versions(one: &str, two: &str) -> std::cmp::Ordering {
    let parts = |version: &str| -> Vec<u64> {
        version
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect()
    };

    let (one, two) = (parts(one), parts(two));
    one.cmp(&two)
}

/// The value of a top-level string field in a small JSON document.
///
/// A hand-written reader for the same reason `parse_compile_commands` is one: the document is `vswhere`'s answer,
/// and the two fields wanted are top-level strings. Escapes are unescaped because a Windows path arrives with
/// `\\` in it. An object or array value is **not** skipped over cleverly — the callers ask for fields `vswhere`
/// writes as strings, and a field that is not a string is reported as absent rather than half-read.
fn json_string_field(json: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let start = json.find(&key)? + key.len();

    let rest = json[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;

    let mut value = String::new();
    let mut characters = rest.chars();

    while let Some(character) = characters.next() {
        match character {
            '"' => return Some(value),
            '\\' => match characters.next()? {
                'n' => value.push('\n'),
                't' => value.push('\t'),
                'r' => value.push('\r'),
                'u' => {
                    // `\uXXXX`, which `vswhere` uses only inside the localized `description` this never reads.
                    // Four hex digits are consumed and the character is dropped rather than guessed at.
                    for _ in 0..4 {
                        characters.next()?;
                    }
                }
                other => value.push(other),
            },
            other => value.push(other),
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// One call the test's runner was asked to make: the program, its arguments, and the environment it was given.
    type Asked = (PathBuf, Vec<String>, Vec<(OsString, OsString)>);

    /// A runner that answers from a script and records what it was asked.
    struct Scripted {
        answers: Mutex<Vec<(String, Output)>>,
        asked: Mutex<Vec<Asked>>,
    }

    impl Scripted {
        fn new(answers: Vec<(&str, Output)>) -> Self {
            Scripted {
                answers: Mutex::new(
                    answers
                        .into_iter()
                        .map(|(program, output)| (program.to_string(), output))
                        .collect(),
                ),
                asked: Mutex::new(Vec::new()),
            }
        }

        fn answering(stdout: &str) -> Self {
            Scripted::new(vec![(
                "vswhere.exe",
                Output {
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                    status: Some(0),
                    succeeded: true,
                },
            )])
        }
    }

    impl CommandRunner for Scripted {
        fn run(
            &self,
            program: &Path,
            arguments: &[&str],
            environment: &[(OsString, OsString)],
        ) -> Option<Output> {
            self.asked.lock().expect("not poisoned").push((
                program.to_path_buf(),
                arguments.iter().map(|argument| argument.to_string()).collect(),
                environment.to_vec(),
            ));

            let name = program.file_name()?.to_string_lossy().to_string();
            let mut answers = self.answers.lock().expect("not poisoned");
            let position = answers.iter().position(|(known, _)| known == &name)?;
            Some(answers.remove(position).1)
        }
    }

    #[test]
    fn vswhere_json_gives_the_installation_and_its_version() {
        let runner = Scripted::answering(
            "[\n  {\n    \"instanceId\": \"3d1f0b4c\",\n    \"installationVersion\": \"17.5.33424.131\",\n    \
             \"installationPath\": \"C:\\\\Program Files\\\\Microsoft Visual Studio\\\\2022\\\\Community\",\n    \
             \"description\": \"localized text nobody reads\"\n  }\n]\n",
        );
        let layout = WindowsLayout {
            program_files: vec![PathBuf::from("C:/Program Files (x86)")],
            ..WindowsLayout::default()
        };

        // The discovery needs `vswhere.exe` to exist, which the scripted layout cannot promise — so the call is
        // made directly, with the path the layout would have produced.
        let vswhere = layout
            .program_files
            .first()
            .expect("a program files directory")
            .join("Microsoft Visual Studio")
            .join("Installer")
            .join("vswhere.exe");

        let output = runner
            .run(&vswhere, &["-latest", "-format", "json"], &[])
            .expect("the script answers");
        let installation = json_string_field(&output.stdout, "installationPath").expect("a path");
        let version = json_string_field(&output.stdout, "installationVersion").expect("a version");

        assert_eq!(
            installation,
            "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community"
        );
        assert_eq!(version, "17.5.33424.131");
        assert_eq!(
            json_string_field(&output.stdout, "nothing"),
            None,
            "a field that is not there is absent, not empty"
        );
    }

    #[test]
    fn the_newest_toolset_and_the_newest_sdk_are_chosen_by_version_and_not_by_spelling() {
        // `14.9` sorts after `14.35` as text and *before* it as a version, which is the bug this pins.
        assert_eq!(
            compare_versions("14.35.32215", "14.9.1"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("10.0.22621.0", "10.0.22000.0"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn a_toolset_is_enumerated_from_disk_with_the_four_host_target_pairs() {
        let root = std::env::temp_dir().join("cppls-msvc-toolsets");
        let _ = std::fs::remove_dir_all(&root);

        let installation = root.join("VS");
        for (toolset, host, target) in [
            ("14.9.1", "Hostx64", "x64"),
            ("14.35.32215", "Hostx64", "x64"),
            ("14.35.32215", "Hostx64", "x86"),
        ] {
            let bin = installation
                .join("VC/Tools/MSVC")
                .join(toolset)
                .join("bin")
                .join(host)
                .join(target);
            std::fs::create_dir_all(&bin).expect("the fixture directory");
            std::fs::write(bin.join("cl.exe"), "").expect("the fixture writes");
        }
        // A toolset directory that was never completed — it has no `cl.exe` — is not a toolset.
        std::fs::create_dir_all(installation.join("VC/Tools/MSVC/14.40.0/include"))
            .expect("the fixture directory");

        let found = toolsets(&installation);

        assert_eq!(found.len(), 2, "two usable toolsets: {found:?}");
        assert_eq!(found[0].version, "14.35.32215", "the newest first");
        assert_eq!(found[1].version, "14.9.1");
        assert!(
            found[0]
                .cl
                .to_string_lossy()
                .replace('\\', "/")
                .ends_with("bin/Hostx64/x64/cl.exe"),
            "a 64-bit host targeting 64-bit code: {}",
            found[0].cl.display()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_the_sdk_parts_that_exist_are_in_the_search_list() {
        let root = std::env::temp_dir().join("cppls-msvc-include");
        let _ = std::fs::remove_dir_all(&root);

        let installation = VisualStudio {
            installation: root.join("VS"),
            version: None,
        };
        let toolset_root = installation.installation.join("VC/Tools/MSVC/14.35.32215");
        let sdk_root = root.join("Kits/10");

        for directory in [
            toolset_root.join("include"),
            toolset_root.join("ATLMFC/include"),
            installation
                .installation
                .join("VC/Auxiliary/VS/include"),
            sdk_root.join("Include/10.0.22621.0/ucrt"),
            sdk_root.join("Include/10.0.22621.0/um"),
            sdk_root.join("Include/10.0.22621.0/shared"),
            // `winrt` and `cppwinrt` are deliberately absent: an older SDK has neither.
        ] {
            std::fs::create_dir_all(&directory).expect("the fixture directory");
        }

        let toolset = Toolset {
            root: toolset_root.clone(),
            version: "14.35.32215".to_string(),
            cl: toolset_root.join("bin/Hostx64/x64/cl.exe"),
        };
        let sdk = WindowsSdk {
            root: sdk_root.clone(),
            version: "10.0.22621.0".to_string(),
        };

        let include = include_directories(&installation, &toolset, Some(&sdk));
        let shown: Vec<String> = include
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_string()
            })
            .collect();

        assert_eq!(
            shown,
            [
                "VS/VC/Tools/MSVC/14.35.32215/include",
                "VS/VC/Tools/MSVC/14.35.32215/ATLMFC/include",
                "VS/VC/Auxiliary/VS/include",
                "Kits/10/Include/10.0.22621.0/ucrt",
                "Kits/10/Include/10.0.22621.0/um",
                "Kits/10/Include/10.0.22621.0/shared",
            ],
            "vcvars64's order, with the parts that do not exist left out"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_macro_dump_is_asked_for_with_the_flags_that_make_it_work() {
        // The three traps the fact sheet measured, pinned as a test: `/Zc:preprocessor` (without it `/PD` is
        // ignored at exit 0 with no output), `/c` (without it the linker appends `LNK1561`), and a real file (cl
        // does not read stdin). Plus `/std:` and `/Zc:__cplusplus`, because `__cplusplus` is `199711L` otherwise.
        let toolset_root = std::env::temp_dir().join("cppls-msvc-dump/VC/Tools/MSVC/14.35.32215");
        let msvc = Msvc {
            toolset: Toolset {
                root: toolset_root.clone(),
                version: "14.35.32215".to_string(),
                cl: toolset_root.join("bin/Hostx64/x64/cl.exe"),
            },
            sdk: None,
            include: vec![toolset_root.join("include")],
            from_environment: false,
        };

        let runner = Scripted::new(vec![(
            "cl.exe",
            Output {
                stdout: "#define _MSC_VER 1935\n#define _WIN32 1\n#define _MSC_VER 1935\n".to_string(),
                stderr: String::new(),
                status: Some(0),
                succeeded: true,
            },
        )]);

        let macros = predefined_macros(&runner, &msvc, Some("c++20")).expect("the script answers");

        let asked = runner.asked.lock().expect("not poisoned").clone();
        let (program, arguments, environment) = asked.first().expect("cl was asked");
        assert!(program.ends_with("cl.exe"));
        for required in ["/nologo", "/Zc:preprocessor", "/Zc:__cplusplus", "/PD", "/c"] {
            assert!(arguments.iter().any(|argument| argument == required), "{required} is missing: {arguments:?}");
        }
        assert!(
            arguments.iter().any(|argument| argument == "/std:c++20"),
            "{arguments:?}"
        );
        assert!(
            arguments.iter().any(|argument| argument.starts_with("/Fo")),
            "the object file does not go next to whatever directory the server was started in: {arguments:?}"
        );
        assert!(
            arguments.last().is_some_and(|argument| argument.ends_with("predefined.cpp")),
            "cl needs a real file: {arguments:?}"
        );
        assert_eq!(
            environment.len(),
            1,
            "the search list is handed over in the environment, which is what a developer prompt does"
        );
        assert_eq!(environment[0].0, OsString::from("INCLUDE"));

        // Sorted and deduplicated: the compiler prints them in hash order, and two runs must agree.
        let names: Vec<&str> = macros.iter().map(|define| define.name.as_ref()).collect();
        assert_eq!(names, ["_MSC_VER", "_WIN32"]);
    }

    #[test]
    fn a_compiler_that_could_not_be_asked_is_not_a_compiler_with_no_macros() {
        // The failure mode the fact sheet calls the most dangerous one: exit 1, zero output, and a caller that
        // read emptiness as "predefines nothing" would configure the analysis with an empty macro table.
        let msvc = Msvc {
            toolset: Toolset {
                root: PathBuf::from("/VS/VC/Tools/MSVC/14.35.32215"),
                version: "14.35.32215".to_string(),
                cl: PathBuf::from("/VS/VC/Tools/MSVC/14.35.32215/bin/Hostx64/x64/cl.exe"),
            },
            sdk: None,
            include: vec![PathBuf::from("/VS/VC/Tools/MSVC/14.35.32215/include")],
            from_environment: false,
        };

        let runner = Scripted::new(vec![(
            "cl.exe",
            Output {
                stdout: String::new(),
                stderr: String::new(),
                status: Some(1),
                succeeded: false,
            },
        )]);

        assert_eq!(predefined_macros(&runner, &msvc, None), None);

        // And the exit status is kept, so that a caller can tell the three failures apart.
        let status = Output {
            status: Some(4),
            succeeded: false,
            ..Output::default()
        };
        assert_eq!(status.status, Some(4));
    }
}
