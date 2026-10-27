# LSP 可用性：每个功能在 `EmmyLuaCodeStyle` 上的答案

```text
$env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
cargo test --release -p cpp_ls --test lsp_status -- --ignored --nocapture
```

`lsp_status` 打开一个真实工程的两个文件（一个 `.cpp` 和它旁边的头文件），等索引收敛，然后**每个已注册的功能问一个
有形状的问题**（一个数量、一个文件、一个拼写），打印一行。这里记的是 2026-10-28 的结果，机器是同一台
（release，`.cppls` 缓存已存在；冷启动另见 `architecture.md` §14.3）。

判读规则：**表里说 `no`，或说了 `0` 而文本显然有答案的地方，就是缺陷**；`note` 写的是"答案本来应该是什么"。
毫秒数只用于比较同一行在不同提交之间的变化，不与别的机器比。

## 结果（全部 `yes`）

| 功能 | n | 时间 | 说明 |
|---|---|---|---|
| `hover` | 2 | 9 ms | `_diagnosticStyle = LuaDiagnosticStyle();` → markdown 卡片 |
| `hover`（类型名） | — | 8 ms | ``` ```cpp class LuaDiagnosticStyle ``` ``` |
| `hover`（`.cpp` 里的使用） | 2 | 9 ms | 同上，跨文件 |
| `hover`（`.` 之后的成员） | 2 | 8 ms | 成员声明 |
| `hover`（成员的成员） | 2 | 2 ms | `bool code_style_check` |
| `typeDefinition`（`auto`） | 3 | 10 ms | `basic_string.h:94` |
| `typeDefinition` | 1 | 2 ms | `LuaDiagnosticStyle.h:10` |
| `definition`（成员） | 1 | 2 ms | `codeformat.h:56` |
| `definition`（头文件） | 1 | 0 ms | `codeformat.h:56` |
| `implementation` | 1 | 4 ms | `CodeFormat.cpp:31` |
| `references` | 6 | 2 ms | `codeformat.cpp:32` 起 |
| `documentHighlight` | 6 | 3 ms | |
| `prepareRename` | 1 | 8 ms | 占位符 `_diagnosticStyle` |
| `rename` | 2 | 9 ms | 头 1 处 + 源 6 处 |
| `documentSymbol` | 14 | 2 ms | `GetInstance`、`CodeFormat`、`UpdateCodeStyle` |
| `workspaceSymbol` | 512 | 8 ms | |
| `foldingRange` | 40 | 2 ms | |
| `selectionRange` | 5 | 1 ms | 5 层嵌套 |
| `semanticTokens` | 521 | **90 ms** | 修前 526 ms，见下 |
| `inlayHint` | 9 | **15 ms** | 修前 58 ms，见下 |
| `documentLink` | 5 | 2 ms | |
| `diagnostic` | 0 | 66 ms | 该文件本来是干净的 |
| `completion`（成员） | 2 | 3 ms | `Editorconfig`、`Workspace` |
| `signatureHelp` | 1 | 0 ms | `SetupStyle(LuaStyle &style, const FormattingOptions &options)` |
| `codeAction` | 0 | **2 ms** | 修前 56 ms，见下 |
| `formatting` | 0 | 2 ms | 文件已经是格式化的 |

## 这次的两个发现（都修了）

1. **`semanticTokens` 526 ms → 90 ms**（同一份文本、同样 521 个 token）。原因不是编码，是**着色这一遍对每个拼写都
   走了一次 include 图**（`macro_environment(name, file)`）。现在整份文件走一次
   （`ProjectIndex::macro_names_in_force`），每个 token 只做一次哈希查找。提交 `d086046`。
2. **`codeAction` 56 ms → 2 ms**，而且**这一处修好让整张表都快了一截**（`hover` 15→9、`documentHighlight` 10→3、
   `definition` 7→0、`completion` 5→3）。原因不是 handler 慢，是它**在排队**：`prepare` 无条件要**写锁**，而 pump
   分段持有读锁。加了一条只读快路（"这文件已经有渲染了 → `load` 没东西可读、`want_cooked_reading` 没东西可排"），
   见提交 `e3ffe03`。测量来自新加的探针 `CPPLS_TRACE_CODE_ACTION=1`：

   ```text
   [codeAction] file 55.196ms, rendering 879.5µs, fixes 13.9µs, 0 action(s)
   [codeAction] prepare 54.096ms, total 56.3657ms
   ```

## 经典 C 项目（neovim）

同一个探针的思路，换成一个 C 工程：`crates/cpp_ls/tests/c_project.rs` 打开 `src/nvim/arglist.c`（一个 800 行的
C 文件，`typedef struct` 到处都是，成员名有被宏改过的），等索引静默后逐功能提问。

```text
$env:CPPLS_REAL_PROJECT = "E:\neovim"
cargo test --release -p cpp_ls --test c_project -- --ignored --nocapture
```

| 功能 | n | 时间 | 说明 |
|---|---|---|---|
| `hover`（成员） | 1 | 8 ms | `int addr_count` |
| `hover`（宏成员） | 1 | 1156 ms / **9 ms** | `b_fnum` 是宏，展开成 `handle`；第二次走 memo |
| `definition`（成员） | 1 | 10 ms | `ex_cmds_defs.h:121` |
| `definition`（函数） | 1 | 9 ms | `arglist.c:207` |
| `references` | 9 | 8 ms | |
| `completion`（`eap->`） | 35 | 10 ms | 含 `addr_count` |
| `documentSymbol` | 47 | 10 ms | |
| `semanticTokens` | 2087 | **1296 ms** | 修前对每个文件都 `null`（见 §H ⑥） |
| `inlayHint` | 17 | 11 ms | |
| `diagnostic` | **2** | 9 ms | 两条都是项目本身缺的生成头（`auto/config.h`、`arglist.c.generated.h`），没有语法错误、没有风格误报 |
| `signatureHelp` | 1 | 19 ms | `do_argfile(exarg_T *eap, int argn)` |

这一轮从这里挖出八个缺陷（参数后的宏让整文件读不成声明、`struct` 被当成类型、链式 hover 取错接收者、宏成员被
说成"没有这个成员"、宏查询 1.19 s 无 memo、条件 include 让着色永不开始、项目的 `-std=` 没让 C 文件变成 C、
`CastExpr` 不算 `CppExpr` 导致实参被丢），都记在 `docs/remaining.md` §H。

## 还剩的行

* **`diagnostic` 66 ms / 0 条** —— 同一行别的时候是 4 ms、110 ms，说明它主要在把索引和渲染热起来而不是在检查；
  稳态预算的指标在 `latency.rs`。
* **`inlayHint` 83 ms → 15 ms**（9 条 hint）—— 不是"每条 hint 很贵"，而是**请求范围内每个调用表达式都被解析了一遍**，
  参数表、被调声明都问完了，才在下一行被"这个参数不在范围内"丢掉。把范围判断提到解析之前即可（提交 `a53d0ce`）。
  探针 `CPPLS_TRACE_INLAY=1` 一行一个调用，打印被调文件、解析耗时和参数表耗时 —— 它区分的是"查找慢"和"查找太多"。

## 怎么重跑、怎么判读

* 全表：上面的命令。它**不断言**，只打印 —— 断言在 `handshake.rs`（34 个用例）和 `latency.rs`（稳态预算）里。
* 单点问题：`CPPLS_PROJECT_ROOT` / `CPPLS_FILE` / `CPPLS_NEEDLE` 加 `cpp_code_analysis` 的 `inference_probe`；
  `CPPLS_TYPE=1` 打印某处的类型，`CPPLS_DIAGNOSTICS=1` 打印整个文件的检查结果。
* 一个功能"答得出"和"答得对"是两件事：这张表管前者，`docs/remaining.md` §A/§C 记的是后者的已知缺口。
