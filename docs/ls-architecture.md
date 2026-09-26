# 语言服务器架构（`crates/cpp_ls`）

这份文档是**架构本身**：这个 crate 从一套成熟的 Lua 语言服务器（`emmylua-analyzer-rust` 的 `emmylua_ls`）里
继承下来的骨架，哪些层是我们的、哪些层是抄来的、每个接缝的契约是什么、以及**接下来要按什么顺序把它接上
`cpp_parser` / `cpp_code_analysis`**。

> 现状（一次大清理之后）：`src/` 从 **169 个 .rs 文件**删到 **49 个**，其中**还引用 `emmylua_*` 的只剩 16 个**
> （`context/` 5 个、`handlers/` 10 个、`util/uri.rs` 只在注释里提了一句），逐个列在 §5 的表里。
> **现在还不能编译**——这是有意的：先把架构留下，再把引擎换掉。`cargo build -p cpp_ls` 已经能读到 manifest
> 与全部模块，剩下的错集中在两类：**未解析的 `emmylua_*` crate** 与**仍然引用旧依赖（`dirs`）的地方**；
> 把 §5 的 1–5 做完这两类就都消失了。

---

## 1. 三层，一条直线

```text
bin/cpp_ls.rs        一个全局分配器 + clap 解析 + run_ls（进程入口）
  └─ server/         连接、初始化握手、主循环、消息分发（**与语言无关**）
       └─ handlers/  每个 LSP 方法一个模块：handler 函数 + 能力注册（**每加一个能力就加一个模块**）
            └─ context/  服务容器：客户端、工作区、分析会话、诊断、进度、取消、更新队列（**语言相关的是引擎**）
                 └─ cpp_code_analysis::Session   ← 换引擎只换这一处
```

### `server/` — 与语言无关，**一行都不用改**

| 文件 | 契约 |
|---|---|
| `mod.rs` | `run_ls(CmdArgs)`：stdio/TCP 连接 → `initialize_start/finish`（能力表来自 `handlers::server_capabilities`）→ `AsyncConnection` → `main_loop` |
| `connection.rs` | `AsyncConnection`：同步 `lsp_server::Connection` 外面套一层 tokio 通道；`recv()`/`send()`/`handle_shutdown()` |
| `lsp_server.rs` | `LspServer { connection, server_context, processor }`：**初始化等待**（只放行 `initialized` 与 `$/cancelRequest`，其余排队）→ 正常循环 → `close()` |
| `message_processor.rs` | `ServerMessageProcessor`：初始化闸门 + 待处理队列 + 按消息种类转给 `on_request_handler` / `on_notification_handler` / `on_response_handler` |
| `error.rs` | `ExitError` |

**这一层之所以能原样留下**，是因为它只依赖 `lsp_server` / `lsp_types` 与我们自己的 `context` 类型名：
"哪些消息在初始化期间可以处理"、"cancel 怎么转成 `CancellationToken`"、"每个请求都必须有响应"这些是协议的事，
不是语言的事。

### `handlers/` — 每个能力一个模块

一个模块要满足**两个**契约，缺一个就会被客户端忽略或者永远不被调用：

```rust
// ① 请求（pull）：返回 RequestOutcome<T>，由 ServerContext::task 变成响应
pub async fn on_goto_definition_handler(
    context: ServerContextSnapshot,
    params: GotoDefinitionParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<GotoDefinitionResponse>

// ② 通知（push）：不需要响应，panic 被 catch_unwind 接住
pub async fn on_did_open_text_document(
    context: ServerContextSnapshot,
    params: DidOpenTextDocumentParams,
) -> Option<()>

// ③ 能力：`initialize` 要告诉客户端"我会什么"
impl RegisterCapabilities for DefinitionCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) { … }
}
```

派发是**一张宏表 + 一张方法表**，两处都只有一处实现（`request_handler.rs` / `notification_handler.rs`）：

```text
dispatch_request!(req, context, { HoverRequest => on_hover, … })
  ① 按 <Req>::METHOD 匹配（用类型常量，不手写字符串）
  ② extract::<Req::Params>()：params 串型不对不是给客户端看的错——跳过这一行，落到 "handler not found"
  ③ context.snapshot()：一个克隆得起、活得过这次请求的服务句柄
  ④ context.task(id, |cancel_token| handler(…))：**spawn**，所以消息循环不等它；
     Ready→result、Missing→null、Cancelled→cancel 错、panic→internal error
```

**能力表在 `handlers/mod.rs` 底部的 `capabilities!` 宏里**，现在是六项：`text_document`、`workspace`、
`definition`、`hover`、`diagnostic`、`configuration`。删掉的那二十来个能力（completion / semantic_token /
rename / references / code_action / inlay_hint / …）回来时的形状是**三处编辑**：加模块、加派发行、加能力行。

### `context/` — 服务容器（**唯一需要换引擎的地方**）

`ServerContext`（`context/mod.rs`）在启动时把服务建好，`snapshot()` 把它们打成一份可克隆的句柄发给每个 handler：

| 服务 | 作用 | 与我们引擎的关系 |
|---|---|---|
| `ClientProxy` | 唯一的出口：`send_response` / `send_notification` / `send_request`（配置要问客户端） | **无**（纯 LSP） |
| `ClientId` | 是哪个编辑器（vscode/neovim/…），决定配置问不问 | **无** |
| `LspFeatures` | 客户端能力的**布尔目录**（pull 诊断？动态注册？work-done 进度？） | **无** |
| `AnalysisState` | 分析会话 + 读写锁 + 阻塞池 | **换引擎**（`EmmyLuaAnalysis` → `cpp_code_analysis::Session`） |
| `WorkspaceManager` | 工作区根、打开的文件、watch 匹配、客户端配置 | 半换（`Emmyrc`/`WorkspaceFolder` → 我们的配置） |
| `DiagnosticService` | push 模式的诊断调度（`publishDiagnostics`）与去重 | 半换（诊断来源换成 `FileView::errors()`） |
| `StatusBar` | `$/progress` 的进度任务（加载工作区、索引） | **无**（纯 LSP） |
| `RequestManager`（pull_cache） | 同 key 请求串行化 + latest-wins 取消 | **无** |
| `update_queue` | **一个** worker 串行消费 `DidOpen/DidChange/DidClose/Watched/Rename` | **无**（见下） |
| `ServerContext::task` / `cancel` | 每个请求一个 `CancellationToken`，`$/cancelRequest` 取消它 | **无** |

**`update_queue` 是这套架构里最值得保留的一件事**：协议允许客户端把 `didChange` 连续推过来，而任何分析引擎
都需要"一次只改一处"。Lua 那边让通知处理器**只入队**，一个 worker 串行地做
`workspace.sync_open_file` → `analysis.update(…)` → 排诊断任务。我们的 `Session` 恰恰是 `&mut self` 的**单写者**
模型（`did_open` / `did_change` / `did_close` / `changed` / `advance`），所以这条队列与它是**天然吻合**的，
不需要改架构。

---

## 2. 一次请求、一次编辑的完整路径

```text
请求（pull）
  client ──"textDocument/definition"──> connection.recv()
    └─ message_processor                 ← 初始化闸门
        └─ on_request_handler            ← dispatch_request! 挑 handler
            └─ context.task(id, closure) ← spawn + CancellationToken
                ├─ context.snapshot()    ← 句柄
                └─ handler(snapshot, params, token)
                     └─ snapshot_query(context.analysis(), token, |session| { …Some(答案) })
                          └─ AnalysisState::run_blocking → spawn_blocking + 读锁 → Session 查询
                └─ RequestOutcome → Response::new_ok/new_err → ClientProxy::send_response

编辑（push）
  client ──"textDocument/didChange"──> on_did_change_text_document   ← **只入队**，不碰锁
    └─ update_tx.send(UpdateEvent::DidChange(params))
        └─ spawn_update_queue 的**唯一 worker**
            └─ process_did_change_text_document
                ├─ workspace.sync_open_file(uri, text)
                ├─ analysis.update(|session| session.did_change(path, text))   ← 写锁 + block_in_place
                └─ 若客户端不支持 pull 诊断：file_diagnostic.add_diagnostic_task(path)
                      └─ 诊断算好后 ClientProxy::send_notification("textDocument/publishDiagnostics")
```

**两条规则**（从 Lua 那边继承，也与我们引擎的假设一致）：

1. **通知永远不在消息循环里干活**：入队即返回，所以 `$/cancelRequest` 永远能被及时处理。
2. **锁序固定**：`workspace_manager`（读/写）→ `analysis`（写）。跨锁的工作（比如"先判断这个文件要不要管"）
   先做判断再取写锁，避免把两把锁叠起来。

---

## 3. 引擎接缝：`cpp_code_analysis::Session` 怎么放进来

`Session` 已经是**面向语言服务器**的：它自带增量队列、overlay（打开缓冲区优先于磁盘）、跨文件索引与光标查询。

```text
Session::open(root, &SessionFiles, WatchFilter)   开项目：compile_commands.json → 发现工具链 → 扫描源文件
session.documents() / did_open / did_change / did_save / did_close
session.changed([FileEvent]) / respond(batch) / advance(steps) / index_everything() / is_idle()
session.view(path) -> Option<FileView>            { path, source, tree, root, scopes, open }
view.errors() -> &[CppParseError]                 容错解析器的诊断（正在编辑的文件通常非空）
session.definition(&view, offset) -> Known<ProjectDefinition>        { file: PathBuf, fact: DeclFact }
session.macro_definition / macro_references / member_completions / name_completions / members_of
```

`Known<T>` 是 `Yes(T) | No | Unknown(reason)`——**这套 API 天生区分"没有"和"不知道"**，正好对应
`RequestOutcome::{Ready, Missing, Cancelled}`：`Yes` → `Ready`，`No` → `Missing`，
`Unknown` → `Missing`（并把 reason 记进日志，而不是编一个空答案给客户端）。

### 唯一的生命周期难题（**先决定，再写代码**）

`Session<'a, DiskFiles>` **借用** `&'a SessionFiles<DiskFiles>`（`OverlayFiles<OpenDocuments, DiskFiles>`），
而 `OpenDocuments` 是 `Arc` 句柄、可以克隆：

```rust
// examples/open_project.rs 的写法
let documents = OpenDocuments::new();
let files = SessionFiles::new(documents.clone(), DiskFiles);   // ← 被 session 借用
let mut session = Session::open(&root, &files, WatchFilter::new(&root));
```

一个活到进程结束的服务器要**同时**持有 `files` 和借用它的 `session`，这在安全 Rust 里是自引用结构。可选方案：

| 方案 | 代价 | 建议 |
|---|---|---|
| `Box::leak(Box::new(files))` 拿 `&'static`，放进 `AnalysisState` 的字段 | 一个 provider 的内存**永不回收**（里面只有 Arc 与零状态，不含文本） | **先用这个**，在字段文档里写清为什么 |
| 把 `Session` 的 `files` 换成 `Arc<SessionFiles<DiskFiles>>`（改 `cpp_code_analysis`） | 动的是引擎的签名，收益是消掉 leak | 若 §5 做完还想收尾，就做这个 |
| `ouroboros`/`self_cell` 之类的自引用 crate | 新依赖 + 生命周期噪音 | 不建议 |

**`AnalysisState` 还要处理"还没有 session"**：`ServerContext::new` 发生在 `initialize`（此时不知道 root），
真正的 `Session::open` 在 `initialized`（此时才知道 workspace folder）。所以引擎字段是
`RwLock<Option<Session<'static, DiskFiles>>>`：没有 session 时所有查询回答 `Missing`——这正是它该说的话
（"没人说过"，不是"没有"）。

---

## 4. 初始化顺序（谁在什么时候做什么）

```text
initialize        server/mod.rs：应答能力表（handlers::server_capabilities）——此时**没有**任何引擎
initialized       handlers/initialized/mod.rs：
  ① 工作区根（workspace_folders，退化用 root_uri）
  ② 客户端配置（get_client_config：vscode 走文件、其他走 workspace/configuration 请求）
  ③ AnalysisState::open(root, files, filter)   ← 工具链发现在这里（一次子进程，几十毫秒）
  ④ status_bar 的 LoadWorkspace 进度 + 后台索引（Session::advance / index_everything）
  ⑤ register_files_watch：把 watch 交给客户端（动态注册）或落到 notify
之后              一切请求走 snapshot_query，一切编辑走 update_queue
shutdown/exit     ServerContext::close()（停 watcher）→ 主循环退出
```

---

## 5. 这次清理做了什么 / 还剩什么

### 删掉的（25 个目录/文件、110 个 .rs）

- **整棵 Lua 能力的 handler 树**：`completion`（含 `providers/` 与 `providers_legacy/` 两套共 30+ 文件）、
  `semantic_token`、`code_actions`、`code_lens`、`command`（`emmy_*` 命令）、`call_hierarchy`、`fold_range`、
  `rename`、`references`、`implementation`、`inlay_hint`、`inline_values`、`signature_helper`、`workspace_symbol`、
  `document_*`（color/link/highlight/formatting/range_formatting/selection_range/symbol）、`hover/{desc,keyword_hover,render}`、
  `definition/{goto_label,goto_module_file,goto_string}`、`diagnostic/workspace_diagnostic`、`common/`、`initialized/{locale,std_i18n}`、
  `context/workspace_manager/tests.rs`。
- **留下的**：`server/`（6）、`context/`（13）、`handlers/`（派发 3 + 文档生命周期 4 + 初始化 4 + 配置 1 +
  工作区 2 + 能力模板 4）、`logger/`（2）、`util/`（4+1）、`cmd_args.rs`、`bin/`、`lib.rs` = **59 个**。

### 还必须改的（**这就是"初始不能编译"的那份清单**，按依赖顺序）

| # | 文件 | 要做什么 |
|---|---|---|
| 1 | `context/analysis_state.rs` | `EmmyLuaAnalysis` → `Session<'static, DiskFiles>`（§3 的 `Option` + leak 方案） |
| 2 | `context/query_runner.rs` | `|&EmmyLuaAnalysis|` → `|&Session|`（`Known::Yes → Ready`，`No/Unknown → Missing`） |
| 3 | `context/workspace_state.rs` | `Emmyrc`/`WorkspaceFolder`/`WorkspaceFileMatcher` → 我们的 `CompilerConfig` + `WatchFilter` + 一个 `uri_to_file_path` |
| 4 | `context/workspace_manager.rs` | 同上；`sync_open_file` → `session.did_open/did_change`（或 `documents().open`） |
| 5 | `context/diagnostic_service.rs` | 诊断来源换成 `session.view(path)?.errors()`；push/pull 两条路保留 |
| 6 | `handlers/initialized/mod.rs` | 去掉 `Emmyrc`/std lib/`collect_workspace_files`，换成 `Session::open` + 索引进度 |
| 7 | `handlers/configuration/mod.rs` | `ClientConfig` 保留，`add_reload_workspace_task` 换成"重读配置 + `session.config()` 更新" |
| 8 | `handlers/text_document/{text_document_handler,watched_file_handler,register_file_watch}.rs` | 把 `analysis.update(…)` 换成 `session.did_*`；`WorkspaceFileMatcher` → 我们的过滤器 |
| 9 | `handlers/workspace/did_rename_files.rs` | 同上（路径改名 → `session.changed([FileEvent::…])`） |
| 10 | `handlers/{definition,hover,diagnostic}/` | 用 `Session::definition` / `FileView` / `errors()` 写实（现在还是 Lua 版） |
| 11 | `lib.rs` + `Cargo.toml` | 去掉 `meta_text`、`rust-i18n`；补 `clap`/`mimalloc`（`lib.rs` 的 `i18n!` 也随之删） |
| 12 | `util/` | 加一个 `uri_to_file_path`/`path_to_uri`（`percent-encoding` + `lsp_types::Uri`），一处实现 |

### 接下来该按什么顺序做（建议）

```text
① 先让 §5 的 1–5 编译过（引擎接缝），哪怕 handler 全是 todo!()——**一个能跑的壳**比十个半成品模块值钱：
   `cargo run -p cpp_ls` 能应答 initialize、能收 didOpen/didChange、能把解析错误当诊断推回去
② 再做 definition（`Session::definition` 已经有了）+ hover（拿 `DeclFact` 的类型文本）
③ 然后是 pull 诊断（`textDocument/diagnostic`）与 push 诊断的取舍：`LspFeatures` 已经把两个都考虑好了
④ 之后按"值多少用户"排：document_symbol（我们的 CST 便宜）→ completion（`member_completions`/`name_completions`
   已经有）→ references/rename（`macro_references` 是现成的第一个）
⑤ parser/分析侧继续按 `docs/roadmap.md` 的队列推进（那边有它自己的门禁与普查）
```

---

## 6. 维护约定（这个 crate 的）

1. **`handlers/` 与 `context/` 的边界**：handler 里不许出现 `lsp_server::Connection`；一切出口走 `ClientProxy`，
   一切引擎访问走 `AnalysisState`。这是"能换引擎、能换客户端"的全部代价。
2. **加一个 LSP 能力 = 三处编辑**：模块（handler + `RegisterCapabilities`）、`request_handler`/`notification_handler`
   的一行、`capabilities!` 的一行。**不许**在别处再写第二张表。
3. **通知只入队**，`process_*` 才动锁（`update_queue`）。任何"在通知处理器里直接改引擎"的写法都会让
   `$/cancelRequest` 与编辑顺序同时失控。
4. **锁序**：`workspace_manager` → `analysis`。跨锁判断必须在取第二把锁之前做完。
5. **`Missing` 不是错误**：引擎答不出来（`Known::No`/`Unknown`、没有 session、没有这个文件）就回 `null`，
   并记一行日志；**不要**编一个空答案——那正是 `Known` 三态存在的理由。
6. **删掉的 Lua 模块不是"待办"**：能力回来时按第 2 条重写，不要从 git 历史里整棵拷回来——那会把
   `emmylua_*` 的依赖一起带回来。
