# LSP 功能自测：哪些能用，哪些要改（2026-10-24）

这份表来自 `crates/cpp_ls/tests/lsp_status.rs`：起真服务器、开真项目（`E:\EmmyLuaCodeStyle`）的
`CodeFormatCLib/src/CodeFormat.cpp` 和 `codeformat.h`，等索引排空（80 次 pull、22 s），然后每个功能问一个
**有形状的**问题（条数、落在哪个文件哪一行），打印一行。

```text
$env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
cargo test --release -p cpp_ls --test lsp_status -- --ignored --nocapture
```

## 表（一次真实运行，2026-10-24 21:2x）

```text
feature                answered n     time   note
hover                  yes      3     22 ms  `_diagnosticStyle = LuaDiagnosticStyle();` → `LuaDiagnosticStyle _diagnosticStyle`
definition (member)    yes      1     2 ms   codeformat.h:56
definition (header)    yes      1     1 ms   codeformat.h:56
implementation         yes      1     5 ms   CodeFormat.cpp:31
references             yes      6     13 ms   codeformat.cpp:32
documentHighlight      yes      0     1 ms   （成员不是"local"，见下）
prepareRename          no       0     10 ms  null
rename                 no       0     10 ms  null
documentSymbol         yes      14    1 ms   GetInstance, CodeFormat, UpdateCodeStyle
workspaceSymbol        yes      512   7 ms
foldingRange           yes      39    1 ms
selectionRange         yes      5     1 ms   5 nested range(s)
semanticTokens         yes      507   31 ms
inlayHint              yes      5     25 ms
documentLink           yes      5     2 ms
diagnostic             yes      0     33 ms
completion (member)    yes      2     27 ms   Editorconfig, Workspace
signatureHelp          no       0     0 ms   null for the first 8 call sites
codeAction             yes      0     1 ms
rangeFormatting        yes      0     2 ms
formatting             yes      0     2 ms
```

## 合理（能用，且有测试或这次实测支撑）

```text
hover                 类型 + 名字，答案里带 LuaDiagnosticStyle；22 ms
definition            成员只给本类那一条（今天的收窄规则），头文件里的声明也算一条
implementation        **今天修好的**：头文件声明 → CodeFormat.cpp:18/31（见"这次发现的缺陷"）
references            6 条、跨文件、含头文件声明；按类过滤 + 接收者类型过滤
documentSymbol        14 个顶层符号，1 ms
workspaceSymbol       512 条（"Format" 前缀），7 ms
foldingRange          39 段，1 ms
selectionRange        5 层嵌套，1 ms
semanticTokens        507 个 token，31 ms（同步、全量）
inlayHint             5 条，25 ms
documentLink          5 条 include，2 ms
diagnostic            pull 模式，0 条（见下"数字对不上"一节）
completion            成员补全 2 条（`Editorconfig`/`Workspace`）——`_configs` 的成员，正确
formatting / rangeFormatting / codeAction   都回答（空数组合法：这份文件已经格式化过、区间里没有诊断）
```

## 需要改（按影响排序）

### P1 ① `rename` / `prepareRename` 只支持宏

`prepareRename` 和 `rename` 在成员上回答 `null`。它们只调 `macro_at`（宏的重命名路径），普通符号没有实现。
F2 在 VSCode 里是最常用的操作之一，而对一个成员改名正是"按类过滤的引用"已经能支撑的事 —— 引用列表有 6 条、
归属明确，改名就是把这些位置逐个替换。缺的是：`prepareRename` 回答名字的 range（可重命名），`rename` 用
`name_level_locations` 的结果生成 `WorkspaceEdit`，并把宏的 `MacroReferences::rename` 保持原样。

判据：`_diagnosticStyle` 上 `prepareRename` 回答一个 range；`rename` 到 `_style` 只改本类那 6 处（跨 2 个文件）。

### P1 ② `documentHighlight` 只支持 local

`documentHighlight` 在成员上回答 `[]`，这是**文档里写明的设计**（"a member, a global or a macro is resolved
through the index rather than through one file's scopes"），但对用户来说就是"光标放在成员上，VSCode 不高亮"。
现在 `local_references_of` 之外已经有 `narrowed_to_the_cursors_class` + 接收者规则，可以复用给同一个文件里的成员。

判据：光标在 `_diagnosticStyle` 上，同一个文件里的 6 处（或其中属于本类的）高亮出来。

### P1 ③ 位置换算：一部分 handler 拿"渲染"的行号当"文件"的行号

`definition`、`rename`、`completion` 用的是 `view_of_the_file`（文件自己的文本）再换算；而
`hover`、`signatureHelp`、`codeAction`、`documentHighlight`、`selectionRange` 直接
`session.view(&path)?`（渲染）+ `offset_at_position(&view, position)`。渲染里指令被展开、**行被合并**，
所以客户端的行号落在别处。

**这次实测到的第一个受害者是 `implementation`**：`codeformat.h` 的 `void UpdateCodeStyle(…)` 在渲染里解析到
"隔壁那个声明"，`definitions` 回答成类 `CodeFormat`，整个功能返回 `null`；改成 `view_of_the_file` 之后
回答 `CodeFormat.cpp:31`。`signatureHelp` 的 8 个调用点全部 `null`，最可能就是同一个原因（`offset_at_position`
在渲染上直接失败 → 闭包提前返回 → 连 `no signature yet` 的日志都没有）。

**改法是一致的**：先把协议位置换算到**文件自身文本**的偏移（`file_offset_at_position`），需要渲染时再用
`view.reading_offset_of(...)`。这五个 handler 逐个改，每个都用 `lsp_status` 表验证（answer 从 no → yes）。

### P2 ④ `diagnostic` 和批处理（`edit_loop`）对同一文件给出不同结果

服务器对 `CodeFormat.cpp` pull 出 **0 条**（settled true），而 `edit_loop` 在同一文件上报 1 条
`undefined-field _configs has no member erase`。那一条是**已知的假报**（`std::vector::erase` 没进事实，
见 `semantics-todo.md` ⑤），所以 0 反而是对的 —— 但它说明两条路径看到的**事实集不同**（cook 的闭包范围）。
这不是"要修成一致"，而是要**说清楚**：pull 该在什么状态下看到哪些事实，并用一个真报（`else-after-return`
之类）在两边对齐。

### P2 ⑤ `signatureHelp` 的真实行为未验证

见 ③：位置换算修好之前，这个功能测不出来。修好后要断言：调用点给出 1 个签名、`activeParameter` 是第几个逗号。

### P3 ⑥ 缺的功能

```text
callHierarchy            没有（LSP 3.16 的 call hierarchy；对 C++ 有用，能回答"谁调用了它"）
typeDefinition           没有（`textDocument/typeDefinition`，对 `auto` 很有用）
documentOnTypeFormatting 没注册（VSCode 的 C++ 一般不依赖）
workspace/diagnostic     **明确不支持**（`workspace_diagnostics: false`），只支持 pull
inlayHint/resolve        没有 resolve（可接受：答案很小）
semanticTokens 增量      只有 full；大文件每次全量（LuaSyntaxTree.cpp 507 个 token / 31 ms，可接受）
```

## 这次自测发现并修好的

**`implementation` 从来没能回答过一个真实项目的问题**（位置换算错误），修好后第一个回答是
`CodeFormat.cpp:31`。同一条规则的另一半——`.cpp` 里的类外定义以前根本不在索引里——是上一条提交修的
（`qualified_declarations`）。

```text
tests/lsp_status.rs           这张表本身（可重跑）
tests/implementation_real.rs  真实项目上的 implementation 断言（会话层，不需要服务器）
```

## 怎么用这张表

每改一项，重跑 `lsp_status`，把 `no` 变 `yes`、把 `0` 变成合理的数字，然后把新的一行贴回本文件。
表的价值在于它是**可重跑的**：不是"我记得某次测过"，而是"这一行现在还是这样"。
