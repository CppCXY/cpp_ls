# MSVC toolchain discovery — measured fact sheet

## What was measured

Read-only reconnaissance for MSVC discovery. Every command below was actually run; outputs are pasted raw (trimmed only where marked `...`). No repository code was modified.

- All commands run via `pwsh -Command`, so PowerShell quoting rules apply to `cmd /c "..."` arguments. This matters — see *Failure modes*.
- Host locale is **Chinese (Simplified)**; `chcp` = **936**. Load-bearing: `cl` diagnostics and the `/showIncludes` marker are **localized**, and English message resources are **not installed**.
- Fixtures under `%TEMP%\msvc_recon\`. Pre-given and re-confirmed: `INCLUDE` empty in the plain shell, `cl.exe` not on `PATH`. mingw-w64 g++ 15.1.0 is present but was not exercised (out of scope).

## Locating the toolset

All three `vswhere` argument sets return the same install; they differ in how much they tell you.
```powershell
& "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -property installationPath
# → C:\Program Files\Microsoft Visual Studio\2022\Community
& "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe" -latest -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
# → C:\Program Files\Microsoft Visual Studio\2022\Community
& "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe" -latest -format json
```
`-format json` returns a 1-element array; useful fields (trimmed from ~45 lines):
```json
"instanceId": "3d1f0b4c", "installationVersion": "17.5.33424.131",
"installationPath": "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community",
"productId": "Microsoft.VisualStudio.Product.Community",
"isComplete": true, "isLaunchable": true, "isPrerelease": false,
"catalog": { "productDisplayVersion": "17.5.1", "productLineVersion": "2022" },
"description": "<localized text, mojibake in a cp936 console>"
```
**Which answers what:** `-property installationPath` is the cheapest way to get the root (one bare line). `-requires ...VC.Tools.x86.x64` is the one to use when you must know the C++ toolset — not just the IDE — is installed. `-format json` is the only form giving the version, and costs the same (see *Timing*), so one JSON call can serve all purposes. `description` is localized; never parse it.

Toolset versions present — exactly one: `Get-ChildItem 'C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC' -Directory` → **`14.35.32215`**.

`cl.exe` (`Get-ChildItem ... -Recurse -Filter cl.exe`) — four, one per host/target pair:
```
C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\bin\Hostx64\x64\cl.exe
C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\bin\Hostx64\x86\cl.exe
C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\bin\Hostx86\x64\cl.exe
C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\bin\Hostx86\x86\cl.exe
```
`bin\` holds exactly `Hostx64` and `Hostx86`; each holds exactly `x64` and `x86`. **No ARM64 toolset — N/A — not present on this machine** (no `Host*\arm64`, no ARM64 `cl.exe`).

`VC\Auxiliary\Build\` contains `vcvars32.bat`, `vcvars64.bat`, `vcvarsall.bat`, `vcvarsamd64_x86.bat`, `vcvarsx86_amd64.bat` — so **`vcvarsall.bat` does exist**, alongside the x64 shortcut `vcvars64.bat`. Windows SDK `Include` and `Lib` both contain `10.0.22000.0` and `10.0.22621.0`.

## Getting a usable environment

**Yes — a process outside a developer prompt can obtain the MSVC environment by calling `vcvars64.bat`.** The exact invocation that works from a plain PowerShell is the *doubled-quote* form:
```powershell
cmd /c "call ""C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"" >nul 2>&1 && cl /nologo /Bv"
```
```
编译器扫描遍数:
 C:\Program Files\...\bin\HostX64\x64\cl.exe:        版本 19.35.32215.0
 C:\Program Files\...\bin\HostX64\x64\c1.dll:        版本 19.35.32215.0
 C:\Program Files\...\bin\HostX64\x64\c1xx.dll:      版本 19.35.32215.0
 ... then c2.dll, c1xx.dll again, link.exe (14.35.32215.0), mspdb140.dll
 C:\Program Files\...\bin\HostX64\x64\2052\clui.dll: 版本 19.35.32215.0
cl: 命令行 error D8003 :缺少源文件名
```
Exit code **2**, but only because `/Bv` was given no source file — the compiler itself ran. (`版本` = "version"; `编译器扫描遍数` = "compiler scan passes"; `缺少源文件名` = "missing source file name".)

The most robust form has no nested quoting at all — write a `.bat` and run `cmd /c "<bat>"`:
```
@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
cl /nologo /Bv
```
What it sets, printed from *inside* that `cmd`. The banner goes to **stdout** unless suppressed:
```
** Visual Studio 2022 Developer Command Prompt v17.5.1
** Copyright (c) 2022 Microsoft Corporation
[vcvarsall.bat] Environment initialized for: 'x64'
INCLUDE=C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\include;C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\ATLMFC\include;C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\VS\include;C:\Program Files (x86)\Windows Kits\10\include\10.0.22621.0\ucrt;C:\Program Files (x86)\Windows Kits\10\\include\10.0.22621.0\\um;C:\Program Files (x86)\Windows Kits\10\\include\10.0.22621.0\\shared;C:\Program Files (x86)\Windows Kits\10\\include\10.0.22621.0\\winrt;C:\Program Files (x86)\Windows Kits\10\\include\10.0.22621.0\\cppwinrt
LIB=C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\ATLMFC\lib\x64;C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\lib\x64;C:\Program Files (x86)\Windows Kits\10\lib\10.0.22621.0\ucrt\x64;C:\Program Files (x86)\Windows Kits\10\\lib\10.0.22621.0\\um\x64
```
`PATH` gains `...\VC\Tools\MSVC\14.35.32215\bin\HostX64\x64` as its **first** entry, then several VS/SDK tool dirs, then the pre-existing entries unchanged. Also set: `VSCMD_ARG_TGT_ARCH=x64`, `VSCMD_ARG_HOST_ARCH=x64`, `WindowsSdkDir=C:\Program Files (x86)\Windows Kits\10\`, `WindowsSDKVersion=10.0.22621.0\`, `VCToolsInstallDir=...\VC\Tools\MSVC\14.35.32215\`, `VCINSTALLDIR=C:\Program Files\Microsoft Visual Studio\2022\Community\VC\`.

Two things to note in that raw `INCLUDE`: separators are doubled in the SDK portion (`Windows Kits\10\\include\10.0.22621.0\\um`) because `WindowsSdkDir` already ends in `\` — Windows tolerates it, but a consumer that canonicalizes paths must too. And the SDK chosen is **10.0.22621.0**, the newer of the two installed.

**Fast path — `vcvars64.bat` is not actually required.** With only `INCLUDE` set in the parent environment and `cl.exe` invoked by absolute path, a TU including both `<cstdio>` and `<windows.h>` compiles cleanly (`/nologo /c /Fo<obj> <src>` → `inc1.cpp`, exit 0). No `vcvars`, and no `LIB` needed (linking was not attempted, `/c`).

## Include search list

**`/v` does not exist for `cl` — N/A — not present on this machine.** It is unknown and ignored:
```powershell
cl /nologo /E /v plain.cpp
```
```
cl: 命令行 warning D9002 :忽略未知选项“/v”
plain.cpp
#line 1 "plain.cpp"
int x;
```
Exit code **0** — the warning is non-fatal, so `/v` silently yields no search list. The real switch is `/Bv` (prints tool versions, shown above), which is *not* a search-list dump.

**`/showIncludes` works and does reveal the include chain.** It works with `/E` and with `/c` (measured with `/c`, exit 0). Notes go to **stdout**; stderr was empty. Shape, from `cl /nologo /showIncludes /c inc1.cpp` where `inc1.cpp` includes `<cstdio>` and `<windows.h>`:
```
inc1.cpp
注意: 包含文件:  C:\Program Files\...\VC\Tools\MSVC\14.35.32215\include\cstdio
注意: 包含文件:   C:\Program Files\...\VC\Tools\MSVC\14.35.32215\include\yvals_core.h
注意: 包含文件:    C:\Program Files\...\VC\Tools\MSVC\14.35.32215\include\vcruntime.h
注意: 包含文件:     C:\Program Files\...\VC\Tools\MSVC\14.35.32215\include\sal.h
注意: 包含文件:      C:\Program Files\...\VC\Tools\MSVC\14.35.32215\include\concurrencysal.h
```
**237 marker lines** for that TU; stdout 239 lines total. Depth is encoded as extra spaces after the marker, so indentation is not fixed-width-safe to parse blindly.

The marker is **localized with no English fallback**. Measured:

- Prefix in Unicode is `注意: 包含文件:` + two spaces (then per-depth indent): `U+6CE8 U+610F U+003A U+0020 U+5305 U+542B U+6587 U+4EF6 U+003A U+0020 U+0020`; its bytes in that capture (UTF-8) were `E6 B3 A8 E6 84 8F 3A 20 E5 8C 85 E5 90 AB E6 96 87 E4 BB B6 3A 20 20`.
- `findstr /C:"Note:"` over the same output matched **0** lines; the string `Note:` occurs nowhere.
- `VSLANG=1033` — set both before and after the `vcvars64.bat` call — had **no effect**; output stays Chinese.
- Reason: `bin\Hostx64\x64\` contains only a `2052\` satellite dir; there is no `1033\clui.dll`. **English message resources: N/A — not present on this machine.**
- A second capture held the identical text in **cp936** byte encoding, so consumers must not assume one byte encoding either. Decode defensively or force one.

**Locale-independent alternative:** `/E` emits `#line` directives that are not localized and give the same resolved paths (doubled backslashes are C escapes, not literal separators):
```
#line 1 "t.cpp"
#line 1 "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Tools\\MSVC\\14.35.32215\\include\\cstdio"
#line 1 "C:\\...\\include\\yvals_core.h"
```
**Plain directory layout** — the 8 entries of `INCLUDE` after `vcvars64`, all confirmed to exist on disk, in order: (1) `...\VC\Tools\MSVC\14.35.32215\include`, (2) `...\VC\Tools\MSVC\14.35.32215\ATLMFC\include`, (3) `...\VC\Auxiliary\VS\include`, (4) `C:\Program Files (x86)\Windows Kits\10\include\10.0.22621.0\ucrt`, (5) `...\10.0.22621.0\um`, (6) `...\10.0.22621.0\shared`, (7) `...\10.0.22621.0\winrt`, (8) `...\10.0.22621.0\cppwinrt`.

Installed SDK versions: `10.0.22000.0` and `10.0.22621.0` (Include **and** Lib). `vcvars64` selects **10.0.22621.0**; `10.0.22000.0` is never referenced in `INCLUDE`/`LIB`. `...\include\10.0.22000.0\ucrt` does exist, confirming that SDK is installed but simply not chosen.

## Predefined macros

**Which flag works: `/PD`, but only together with `/Zc:preprocessor`.** `/PD` alone is a trap — `cl /nologo /PD t.cpp` gives:
```
cl: 命令行 warning D9007 :“/PD”需要“Zc:preprocessor”；选项被忽略
t.cpp
```
Exit code **0** and **no macro output at all** ("warning D9007: '/PD' requires '/Zc:preprocessor'; option ignored"). A caller that ignores stderr sees a successful run that produced zero macros.

The working incantation, on an **empty** file — prints the compiler's own predefined macros and nothing else: **58 `#define` lines**, 59 lines total, exit 0:
```powershell
cl /nologo /Zc:preprocessor /PD /c empty.cpp
```
First 20 of the 58 lines, raw:
```
#define _CPPRTTI 1
#define __cpp_ref_qualifiers 200710L
#define __cpp_init_captures 201304L
#define __STDCPP_DEFAULT_NEW_ALIGNMENT__ 16ull
#define __cpp_rtti 199711L
#define _M_X64 100
#define __cpp_decltype_auto 201304L
#define _MSC_EXTENSIONS 1
#define _MSVC_LANG 201402L
#define __cpp_binary_literals 201304L
#define __cpp_constexpr 201304L
#define _MSC_BUILD 0
#define __cpp_attributes 200809L
#define __cpp_inheriting_constructors 200802L
#define __cpp_generic_lambdas 201304L
#define __cpp_variable_templates 201304L
#define __cpp_nsdmi 200809L
#define _NATIVE_WCHAR_T_DEFINED 1
#define __BOOL_DEFINED 1
#define _WIN64 1
```
Shape notes, all measured:

- A real file is required, and **`/c` is required**: without it the run proceeds into the linker and appends `LINK : fatal error LNK1561: 必须定义入口点`.
- Output is **unsorted** (hash-table order) — sort and dedupe yourself. One `#define NAME value` per line, no continuation lines, no source echo, printed to stdout.
- Contents: `_MSC_VER 1935`, `_MSC_FULL_VER 193532215`, `_MSC_BUILD 0`, `_M_X64 100`, `_M_AMD64 100`, `_WIN32 1`, `_WIN64 1`, `_MSVC_LANG 201402L`, `__cplusplus 199711L`, `_MSVC_TRADITIONAL 0`, `_MT 1`, `_INTEGRAL_MAX_BITS 64`, `_MSVC_WARNING_LEVEL 1L`, `__STDC_HOSTED__ 1`, `__STDCPP_THREADS__ 1`, `_CPPRTTI 1`, `_MSC_EXTENSIONS 1`, … plus `_MSVC_EXECUTION_CHARACTER_SET 936`, mirroring this machine's code page.
- **Function-like macros: 0** in the empty-file (predefined-only) dump. They *are* printed once the TU includes headers: a `#include <cstdio>` TU gives 1540 `#define` lines including e.g. `#define __DEFINE_CPP_OVERLOAD_SECURE_FUNC_1_2(_ReturnType, ...) extern "C++" { ... }`. So the dump is the whole preprocessed macro table, not only the builtin set.
- **Builtins are NOT included.** Measured count 0 for `__FILE__`, `__DATE__`, `__TIME__`, `__LINE__`, `__COUNTER__`, `__STDC__`. `_DEBUG` and `NDEBUG` are also absent (not defined by default).

**Candidate flags that do NOT dump macros:**

- `cl /nologo /EP /d1PP t.cpp` — 357001 bytes / 13349 lines, but this is ordinary preprocess-to-stdout (`/EP` strips `#line`); measured **0** occurrences of `_MSC_VER`. Same for `/E /d1PP` (461987 bytes / 13371 lines, 0 occurrences).
- `cl /nologo /P /d1PP t.cpp` — writes `t.i` (259662 bytes) to disk, prints nothing to stdout.
- `cl /nologo /PD` with no file — `warning D9007` then `error D8003 :缺少源文件名`, exit 2.

**stdin does not work: N/A — not supported.** `-` is not a source-file designator. `cl /nologo /EP /d1PP -` → `warning D9002 :忽略未知选项“-”` then `error D8003 :缺少源文件名` (exit 2); `cl /nologo /EP /d1PP /Tc-` → `c1: fatal error C1083: 无法打开源文件: “-”: No such file or directory` (exit 2). A real file on disk is required — use a temp file.

**`__cplusplus` and `/std:c++20`**, measured via `/Zc:preprocessor /PD` on an empty file:

| flags | `_MSVC_LANG` | `__cplusplus` |
|---|---|---|
| (default) | `201402L` | `199711L` |
| `/std:c++20` | `202002L` | `199711L` |
| `/std:c++20 /Zc:__cplusplus` | `202002L` | `202002L` |
| `/std:c++17 /Zc:__cplusplus` | `201703L` | `201703L` |

Confirmed: `/std:c++20` alone does **not** change `__cplusplus`; add `/Zc:__cplusplus` to get `202002L`. Without it, `_MSVC_LANG` is the real language-version signal.

## Timing

`Measure-Command`, 5 runs each, ms.

| Operation | min | avg | max |
|---|---|---|---|
| `vswhere -latest -property installationPath` | 21 | 24 | 32 |
| `vswhere -latest -format json` | 22 | 24 | 26 |
| `cmd /c <bat>`: `vcvars64.bat` only | 1291 | 1383 | 1588 |
| `cmd /c <bat>`: `vcvars64.bat` + `cl /Bv /c` | 1293 | 1321 | 1355 |
| `cmd /c <bat>`: `vcvars64.bat` + `cl /Zc:preprocessor /PD /c` | 1292 | 1316 | 1359 |
| inline doubled-quote: `vcvars64.bat` + `cl /Bv` | 1282 | 1319 | 1352 |
| direct `cl.exe` + `INCLUDE`: `/Zc:preprocessor /PD /c` (empty file) | 42 | 50 | 71 |
| direct `cl.exe` + `INCLUDE`: `/Bv /c` | 45 | 62 | 116 |
| direct `cl.exe` + `INCLUDE`: `/showIncludes /c` (cstdio + windows.h) | 338 | 367 | 403 |

`vswhere` is ~20 ms and negligible. **`vcvars64.bat` costs ~1.3 s and completely dominates** — adding a `cl` invocation on top is within noise (1316–1321 ms vs 1383 ms for `vcvars` alone; the gap is smaller than run-to-run variance). Skipping `vcvars` makes the macro dump a ~50 ms operation, roughly **26x faster**; the include-chain dump's ~370 ms is dominated by actually parsing `cstdio` + `windows.h`, not by discovery.

## Failure modes

**1. Wrong quoting fails silently — exit 1, zero output.** The most dangerous case. From PowerShell the backslash-escaped form `cmd /c "call \"C:\Program Files\...\vcvars64.bat\" >nul 2>&1 && cl /nologo /Bv"` (a common way to write it) does not pass quotes through to `cmd`. The run produced no output at all: `exit=1  output lines=0  bytes=0`. Because `>nul 2>&1` is attached to the failed `call`, `cmd`'s "not recognized" complaint is swallowed *and* `&&` short-circuits `cl`. Identical silent exit-1 behaviour was measured for a nonexistent `vcvars64.bat` path, for an unquoted path containing spaces, and for the doubled-quote form pointed at a nonexistent path. **Zero output plus exit 1 means "could not ask the compiler", not "the compiler predefines nothing".**

Exit codes are the discriminator: **1 = `vcvars`/`call` failed, 2 = `cl` ran and rejected the command line, 4 = `cl` started but could not load resources, 0 = success.**

**2. Unquoted path with spaces** — raw `cmd` text, with the redirect removed so it is visible:
```
'C:\Program' is not recognized as an internal or external command,
operable program or batch file.
```
exit 1. Same class as (1), but it *does* announce itself if you don't suppress stderr.

**3. `cl.exe` by absolute path, no `vcvars`, no `INCLUDE`** — the compiler runs but has no search paths:
```
inc1.cpp
C:\Users\zc\AppData\Local\Temp\msvc_recon\inc1.cpp(1): fatal error C1034: cstdio: 不包括路径集
```
exit **2** (`不包括路径集` = "no include path set"). Notably a *trivial* file compiles with exit **0**, so "cl.exe works" is not evidence that the environment is correct — probe a file that includes a header.

**4. `cl.exe` with its DLLs missing.** This does **not** happen when calling `cl.exe` by absolute path from the toolset dir: `c1.dll`/`c1xx.dll`/`c2.dll` are found next to the executable, and `/Bv` and a trivial compile both succeed with no `vcvars`. To reproduce a real load failure I copied `cl.exe` alone into an otherwise empty directory; it printed `fatal error C1510: Cannot load language resource clui.dll.` with exit code **4**, on **stdout** (stderr empty), and in **English** — because the `2052\` resource directory was absent. So `clui.dll` resolves relative to the executable: discovery must locate the real `bin\Host<host>\<target>\` directory and keep it intact, and a resource failure surfaces as exit 4, not as a missing-DLL load error.

**5. `/PD` without `/Zc:preprocessor`** — warning on stderr, exit **0**, no macros (see *Predefined macros*).

**6. `cl` with no source file** — `cl: 命令行 error D8003 :缺少源文件名`, exit 2. This is what a bare `cl /nologo /Bv` or a dropped filename produces: a *usage* error, not a toolchain failure.

**7. The `vcvars` banner contaminates stdout.** Unless suppressed with `>nul 2>&1`, four lines (a `****` box, two copyright/version lines, and `[vcvarsall.bat] Environment initialized for: 'x64'`) are prepended to whatever `cl` prints on the same stream. Any parser reading combined output must strip them.

**8. All diagnostics are localized (cp936), with no English resource installed.** Exact texts recorded: `warning D9002 :忽略未知选项“/v”` · `warning D9007 :“/PD”需要“Zc:preprocessor”；选项被忽略` · `error D8003 :缺少源文件名` · `fatal error C1034: cstdio: 不包括路径集` · `fatal error C1083: 无法打开源文件: “-”: No such file or directory` · `LINK : fatal error LNK1561: 必须定义入口点`. Match on the **codes** (`D9002`, `D9007`, `D8003`, `C1034`, `C1083`, `C1510`, `LNK1561`) and on exit codes, never on message text.

## What this implies for an implementation

Each conclusion is tied to the measurement above it.

1. **One `vswhere -latest -format json` call suffices to locate the install** (~24 ms) and is the only form giving the version; use `-requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64` to assert the C++ toolset is present. Do not parse the localized `description`.
2. **Enumerate toolsets from disk, not from `vcvars`**: `VC\Tools\MSVC\*` (one here, `14.35.32215`), then `bin\Host<host>\<target>\cl.exe`. Four host/target pairs exist; ARM64 does not — treat a missing target as "not installed", not as an error.
3. **Do not pay the ~1.3 s `vcvars64.bat` cost.** Setting `INCLUDE` yourself and invoking `cl.exe` by absolute path compiled a `<cstdio>` + `<windows.h>` TU with exit 0, and dumps macros in ~50 ms instead of ~1300 ms. Derive `INCLUDE` from the 8 directories listed in *Include search list*, and canonicalize the doubled `\\` in the SDK portion.
4. **If you do use `vcvars64.bat`, use the doubled-quote form** (`cmd /c "call ""...vcvars64.bat"" ..."`) or a `.bat` wrapper, and suppress its banner with `>nul 2>&1`. Never use `\"` escaping from PowerShell — it fails silently with exit 1 and no output.
5. **Dump predefined macros with `cl /nologo /Zc:preprocessor /PD /c <realfile>`.** `/PD` without `/Zc:preprocessor` is ignored at exit 0; without `/c` the linker appends `LNK1561`. Expect 58 `#define` lines for an empty file, unsorted, object-like only, with no `__FILE__`/`__DATE__`/`__LINE__` builtins. Sort and dedupe before use.
6. **stdin is not supported** — `-` and `/Tc-` both fail with exit 2. Always write a temp file to ask `cl` anything.
7. **Prefer `INCLUDE` (environment or replicated) over parsing `cl` output for the search list.** If you must parse, use `/showIncludes` and know that its marker is the localized `注意: 包含文件:` (no English resources exist here; `VSLANG=1033` does nothing), and that the same text appeared in both UTF-8 and cp936 byte encodings. `/v` does not exist; `#line` from `/E` is the locale-independent fallback.
8. **Use exit codes as the primary signal; never treat empty output as an empty result.** `1` = could not obtain the environment (bad quoting, missing `vcvars`), `2` = `cl` rejected the command line (`D8003`) or could not find headers (`C1034`, `C1083`), `4` = `cl` started but could not load resources (`C1510`), `0` = success. "Toolchain could not be asked" must be distinguishable from a successful empty query.
9. **Match diagnostic codes, not messages** — every `cl` message is localized here, and `clui.dll` resolves relative to `cl.exe`, so keep the real toolset `bin` directory intact rather than copying `cl.exe` out.
10. **`__cplusplus` is `199711L` regardless of `/std:` unless `/Zc:__cplusplus` is passed**; use `_MSVC_LANG` for the effective language level (`201402L` default, `202002L` for `/std:c++20`).
