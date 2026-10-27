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
| `hover` | 2 | 15 ms | `_diagnosticStyle = LuaDiagnosticStyle();` → markdown 卡片 |
| `hover`（类型名） | — | 8 ms | ``` ```cpp class LuaDiagnosticStyle ``` ``` |
| `hover`（`.cpp` 里的使用） | 2 | 15 ms | 同上，跨文件 |
| `hover`（`.` 之后的成员） | 2 | 8 ms | 成员声明 |
| `hover`（成员的成员） | 2 | 8 ms | `bool code_style_check` |
| `typeDefinition`（`auto`） | 3 | 11 ms | `basic_string.h:94` |
| `typeDefinition` | 1 | 2 ms | `LuaDiagnosticStyle.h:10` |
| `definition`（成员） | 1 | 8 ms | `codeformat.h:56` |
| `definition`（头文件） | 1 | 7 ms | `codeformat.h:56` |
| `implementation` | 1 | 12 ms | `CodeFormat.cpp:31` |
| `references` | 6 | 2 ms | `codeformat.cpp:32` 起 |
| `documentHighlight` | 6 | 10 ms | |
| `prepareRename` | 1 | 8 ms | 占位符 `_diagnosticStyle` |
| `rename` | 2 | 16 ms | 头 1 处 + 源 6 处 |
| `documentSymbol` | 14 | 2 ms | `GetInstance`、`CodeFormat`、`UpdateCodeStyle` |
| `workspaceSymbol` | 512 | 8 ms | |
| `foldingRange` | 40 | 7 ms | |
| `selectionRange` | 5 | 1 ms | 5 层嵌套 |
| `semanticTokens` | 521 | **100 ms** | 修前 526 ms，见下 |
| `inlayHint` | 9 | 67 ms | |
| `documentLink` | 5 | 9 ms | |
| `diagnostic` | 0 | 74 ms | 该文件本来是干净的 |
| `completion`（成员） | 2 | 5 ms | `Editorconfig`、`Workspace` |
| `signatureHelp` | 1 | 0 ms | `SetupStyle(LuaStyle &style, const FormattingOptions &options)` |
| `codeAction` | 0 | 57 ms | 光标处没有可用的修复 |
| `formatting` | 0 | 2 ms | 文件已经是格式化的 |

## 这次的三个发现

1. **`semanticTokens` 526 ms → 100 ms**（同一份文本、同样 521 个 token）。原因不是编码，是**着色这一遍对每个拼写都
   走了一次 include 图**（`macro_environment(name, file)`）。现在整份文件走一次
   （`ProjectIndex::macro_names_in_force`），每个 token 只做一次哈希查找。提交 `d086046`。
2. **`codeAction` 57 ms 返回 0 个动作**，是这张表里唯一"代价和收益不匹配"的行：它每次光标移动都被请求。下一步要么
   让"没有可修复的东西"更快地回答，要么把 `include_fix` 之外的 provider 明确列出来。
3. **`diagnostic` 74 ms / 0 条**（同一行别的时候是 4 ms、110 ms）—— 冷热差三个数量级，说明它主要花在把索引和渲染
   热起来，而不是在检查；`latency.rs` 的稳态预算才是它的指标。

## 怎么重跑、怎么判读

* 全表：上面的命令。它**不断言**，只打印 —— 断言在 `handshake.rs`（34 个用例）和 `latency.rs`（稳态预算）里。
* 单点问题：`CPPLS_PROJECT_ROOT` / `CPPLS_FILE` / `CPPLS_NEEDLE` 加 `cpp_code_analysis` 的 `inference_probe`；
  `CPPLS_TYPE=1` 打印某处的类型，`CPPLS_DIAGNOSTICS=1` 打印整个文件的检查结果。
* 一个功能"答得出"和"答得对"是两件事：这张表管前者，`docs/remaining.md` §A/§C 记的是后者的已知缺口。
