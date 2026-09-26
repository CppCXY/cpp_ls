# 语言服务器架构（`crates/cpp_ls`）

这份文档是**架构本身**：这个 crate 从一套成熟的 Lua 语言服务器（`emmylua-analyzer-rust` 的 `emmylua_ls`）里
继承下来的骨架，哪些层是我们的、哪些层是抄来的、每个接缝的契约是什么、以及**接下来要按什么顺序把它接上
`cpp_parser` / `cpp_code_analysis`**。

> 现状（**这个壳已经能跑**）：`src/` 从 **169 个 .rs 文件**删到 **47 个**（`bin` 1、`server` 6、`context` 13、
> `handlers` 18、`logger` 2、`util` 5，加上 `cmd_args.rs` 与 `lib.rs`），engine 引用清零，
> `cargo build -p cpp_ls` 与 `cargo clippy --workspace --all-targets` 都是零警告；
> `crates/cpp_ls/tests/handshake.rs` 用一个**真的客户端**跑完 `initialize → initialized → didOpen →
> publishDiagnostics → definition → hover → shutdown/exit`（0.6 秒，见 §5 末尾）。
> 手上有三个能力：**诊断（push + pull）**、**definition**、**hover**；再往上加一个能力是 §6 第 2 条的三处编辑。

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
| `AnalysisState` | 分析会话 + 读写锁 + 阻塞池 + **索引泵的唤醒信号** | `cpp_code_analysis::Session<DiskFiles>`（§3） |
| `WorkspaceManager` | 工作区根、打开的文件、`compile_commands.json` 变更后的重载（防抖 + 代次） | 我们的 `WatchFilter`/`PathPattern` |
| `DiagnosticService` | push 模式诊断的调度：每文件防抖 + 全工作区一遍 | 诊断来源 = `FileView::errors()` |
| `StatusBar` | `$/progress` 的进度任务（加载工作区、索引） | **无**（纯 LSP） |
| `RequestManager`（pull_cache） | 同 key 请求串行化 + latest-wins 取消（现在用在 pull 诊断上） | **无** |
| `update_queue` | **一个** worker 串行消费 `Opened/Changed/Saved/Closed/WatchedFilesChanged` | **无**（见下） |
| `ServerContext::task` / `cancel` | 每个请求一个 `CancellationToken`，`$/cancelRequest` 取消它 | **无** |

**`update_queue` 是这套架构里最值得保留的一件事**：协议允许客户端把 `didChange` 连续推过来，而任何分析引擎
都需要"一次只改一处"。Lua 那边让通知处理器**只入队**，一个 worker 串行地做
`workspace.sync_open_file` → `analysis.update(…)` → 排诊断任务。我们的 `Session` 恰恰是 `&mut self` 的**单写者**
模型（`did_open` / `did_change` / `did_close` / `changed` / `advance`），所以这条队列与它是**天然吻合**的，
不需要改架构。

**索引泵**（`handlers/initialized/mod.rs` 的 `index_in_background`）是同一件事的另一半：引擎**故意**不带线程、
不带调度——`Session::did_change` 把失效的摘要**丢掉并入队**，然后在下一个 `advance` 里重读（"查询在它被重读
之前必须回答'还没读'，而不是拿用户已经改过的文本作答"）。所以"谁来一直调用 `advance`"是**服务器**的事：
一个任务不停地 `advance(16)`，队列空了就等 `AnalysisState::wake()`（编辑、关闭、客户端文件事件都会唤醒它），
一秒一次的兜底超时只是保险。没有这个泵，用户一打字，那个文件的摘要就永久消失——这正是 handshake 测试第一次
跑出来的 bug（见 §5）。

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
    └─ update_tx.send(UpdateEvent::Changed(params))
        └─ spawn_update_queue 的**唯一 worker**
            └─ process_did_change_text_document
                ├─ workspace.sync_open_file(uri, text)
                ├─ analysis.update_session(|session| session.did_change(path, text))  ← 写锁 + block_in_place
                ├─ analysis.wake()                                    ← 叫醒索引泵：那份摘要刚被丢掉
                └─ 若客户端不支持 pull 诊断：file_diagnostic.add_diagnostic_task(path, 500ms)
                      └─ 诊断算好后 ClientProxy::send_notification("textDocument/publishDiagnostics")

索引泵（同一个 context，另一个任务）
  循环：advance(16) → 队列空？→ 是：推第一遍全工作区诊断（只做一次），然后等 wake（兜底 1 秒）
        └─ 工作区版本号变了（重载）：这个泵退出，新的泵接手
```

**两条规则**（从 Lua 那边继承，也与我们引擎的假设一致）：

1. **通知永远不在消息循环里干活**：入队即返回，所以 `$/cancelRequest` 永远能被及时处理。
2. **锁序固定**：`workspace_manager`（读/写）→ `analysis`（写）。跨锁的工作（比如"先判断这个文件要不要管"）
   先做判断再取写锁，避免把两把锁叠起来。

---

## 3. 引擎接缝：`cpp_code_analysis::Session` 怎么放进来

`Session` 已经是**面向语言服务器**的：它自带增量队列、overlay（打开缓冲区优先于磁盘）、跨文件索引与光标查询。

```text
Session::open(root, SessionFiles<DiskFiles>, WatchFilter) -> Session<DiskFiles>   ← 拥有 provider，无生命周期
session.did_open / did_change / did_save / did_close
session.changed([FileEvent]) / respond(batch) / advance(steps) / index_everything() / pending() / is_idle()
session.view(path) -> Option<FileView>            { path, source, tree, root, scopes, open: bool }
session.text(path) -> Option<String>              只要文本不要解析（hover 显示别的文件里的声明用）
view.errors() -> &[CppParseError]                 容错解析器的诊断（正在编辑的文件通常非空）
session.definition(&view, offset) -> Known<ProjectDefinition>        { file: PathBuf, fact: DeclFact }
session.macro_definition / macro_references / member_completions / name_completions / members_of
```

`Known<T>` 是 `Yes(T) | No | Unknown(reason)`——**这套 API 天生区分"没有"和"不知道"**，正好对应
`RequestOutcome::{Ready, Missing, Cancelled}`：`Yes` → `Ready`，`No` → `Missing`，
`Unknown` → `Missing`（并把 reason 记进日志，而不是编一个空答案给客户端）。

### provider 的归属：**引擎改成拥有它**（这一节记的是结论，不是选项）
上一版 `Session` 是 `Session<'a, F>`，字段 `files: &'a SessionFiles<F>`，`SummaryStore` 也一样借用
（`store: SummaryStore<'a, SessionFiles<F>>`）。于是"一个活到进程结束的服务器同时持有 provider 与借用它的
session"就是自引用结构，当时的候选方案是 `Box::leak`。

**这个方案被否掉了，改的是引擎**：`SummaryStore<F>` 与 `Session<F>` 现在**按值拥有** provider，
`Session::open(root, files, filter)` 收的是 `SessionFiles<F>` 而不是 `&SessionFiles<F>`。理由不是"绕开借用检查"，
而是**原来那个签名描述错了所有权**：provider 全是**句柄**而不是数据——

```text
OpenDocuments   Arc<RwLock<HashMap<String, String>>>   克隆 = 同一个 map 的第二个句柄
DiskFiles       零大小类型                              克隆不复制任何东西
OverlayFiles    两个句柄的组合                          Clone 派生，语义就是"同一批文件"
```

所以 `SessionFiles` 的克隆**就是**"同一批文件的第二个 owner"，正好是编辑器需要的：会话读的时候，
客户端还能往里写缓冲区。今天的形状是：

```rust
// context/analysis_state.rs
struct AnalysisState { files: SessionFiles<DiskFiles>, inner: RwLock<Option<Session<DiskFiles>>>, … }

state.files().overlay.open(path, text);        // 客户端写（会话通过同一个 map 读）
state.open(root, filter).await;                // Session::open(root, files.clone(), filter)
```

调用侧的账也一起付了：`examples/` 与测试里 `Session::open(&root, &files, …)` 全改成传值（想留着 `files`
自己用的地方 `files.clone()`），`Session<'_, DiskFiles>` 这种类型注解全部消失。引擎自己的测试
（1091 → 1092）一个没少，读数不变（§5）。

**`AnalysisState` 还要处理"还没有 session"**：`ServerContext::new` 发生在 `initialize`（此时不知道 root），
真正的 `Session::open` 在 `initialized`（此时才知道 workspace folder）。所以字段是
`RwLock<Option<Session<DiskFiles>>>`：没有 session 时所有查询回答 `Missing`——这正是它该说的话
（"没人说过"，不是"没有"）。`initialize` 与 `initialized` 之间到达的请求就落在这里，日志里会留一行。

### 文件层：VFS 与 `FileView`（`cpp_code_analysis::file`）

```text
file/paths.rs   provider 链（DiskFiles / MemoryFiles / OverlayFiles / OpenDocuments）、FileId、路径规范化
file/vfs.rs     **持有的文件**：文本 + 那一段文本的 LineIndex，两者一起建立、一起替换
file/view.rs    FileView = VfsFile（file id + text + line_index + open）+ 解析（tree/root）+ scopes
file/mod.rs     FileAnalysis / FileTokens（单文件事实与 token 流）
```

三条规矩：

1. **文本与行索引同生同死**：`Vfs::insert` 同时写文本和它算出来的 `LineIndex`，没有第二条路径能改其中一半。
   一个落后一次编辑的行索引比没有行索引更糟——编辑之后的每个位置都会**静默**映射到错的偏移。
2. **`FileId` 是永久稳定的密集下标**：表是 `Vec<Option<VfsFile>>`，`files[id.index()]` 是数组访问；id 用
   `files.len()` 铸造、**永不复用也永不重排**。关闭文件 = `close` 往那个槽写 `None`（id 保留，其余 id 不受影响），
   所以"关闭"是可回答的状态：`None` 是"不再持有"，与"空文件"是两回事。洞的代价是每个读过的文件一个 `Option`。
3. **VFS 里没有锁**：会话那一把锁才是同步点。改表的方法取 `&mut self`（`load`/`insert`/`close`），只看的方法取
   `&self`（`held`/`get`）。给 VFS 加 `RwLock` 会让**单线程调用方**（批处理、测试、直接持有 `Session` 的程序）
   为一个它没有的问题付费，而且会暗示"两个线程可以一边改文件表一边被查询读"——那恰恰是必须禁止的
   （文本和行索引要一起换代）。`FileView` 仍是 VFS 条目的引用（`Arc<str>` + `Arc<LineIndex>` 共享），
   所以 `offset_at`/`position_at` 是索引里的二分查找——这正是之前错的地方：原来每调用一次就
   `LineIndex::parse` 一遍（**一次全文件扫描**），而"行"只在文本变化时才变。

### 谁把文件放进表里：**写者预加载**，查询只读

因为改表要 `&mut Session`，**查询**（跑在 `AnalysisState` 读锁下、与其他查询并行）只能看已经持有的文件。
填表是写者的事，现在有三个写者，覆盖了全部情形：

```text
didOpen / didChange      客户端的缓冲区（本来就是写锁路径）
advance（索引泵）         分析读过的每个文件——"分析读过的文件都被持有"是不变量
AnalysisState::prepare   请求到达前把"这个请求要问的文件"读进来：写锁只持有读一个文件那么久，
                         snapshot_query 仍然是读锁下的并行查询
```

`prepare` 是这条设计的补丁而不是绕路：definition/hover/pull 诊断三个 handler 在查询前各调一次
（客户端打开的文件其实已经被 `didOpen` 持有，这一步给"从没打开过的文件"兜底）。
**替代方案**是让查询自己加载——那需要 `&mut Session`，等于把所有请求串行化在一把写锁后面，
`prepare` 存在的唯一理由就是避开那个代价（用户 2026-09-26 拍板走这条）。

顺带在这一层修掉一个真 bug：`LineIndex::get_offset` 原来把列号**对整个文本**做 clamp，于是
`get_offset(0, 99)` 在一个三行文件上会返回**后面某一行里**的偏移——一个不在调用者所问文件位置上的位置
（现在 clamp 到该行的行尾，这也正是协议对超长 character 的规定）。

---

## 4. 初始化顺序（谁在什么时候做什么）

```text
initialize        server/mod.rs：应答能力表（handlers::server_capabilities）——此时**没有**任何引擎
                  handlers::initialized_handler 在同一个 spawn 里就跑起来了（Lua 骨架如此）：
initialized       ① 工作区根（workspace_folders，退化用 root_uri）；多根只分析第一个，其余在日志里
  ② client_id 与客户端配置（`cppls` section；VS Code 另外读 `files.exclude` 的 glob）
  ③ AnalysisState::open(root, filter)   ← 工具链发现在这里（一次子进程，几十毫秒）
  ④ 客户端已经打开的缓冲区再标一次（Session::did_open）——重启后未保存的内容因此不会丢
  ⑤ 索引泵：advance(16) 直到队列空 → 推第一遍全工作区诊断（客户端不支持 pull 时）
  ⑥ register_files_watch：把 watch glob 交给客户端（动态注册）；**没有** notify 兜底（见 §1 的 update_queue 旁注）
之后              一切请求走 snapshot_query / analysis_query，一切编辑走 update_queue → wake 索引泵
shutdown/exit     主循环退出 → ServerContext::close()（取消所有诊断与挂起的重载）→ 进程退出
                  **不 join IO 线程**：读线程会一直阻塞在 stdin 直到客户端关管道，join 会让"客户端说了 exit"
                  反而挂住（handshake 测试抓到的第二个 bug）
```

`compile_commands.json` 变了（客户端通过 `workspace/didChangeWatchedFiles` 告诉我们）→ `WorkspaceManager`
防抖 2 秒 → 清掉旧诊断 → **重走上面 ③–⑤ 一遍**（重载与首次打开是同一条代码路径，差别只是"之前有什么"），
并用代次号保证两次重载不会都跑。客户端配置变了（`workspace/didChangeConfiguration`）走同一条重载。

---

## 5. 这一遍做了什么（**壳现在是活的**）

### 引擎侧（`cpp_code_analysis` / `cpp_parser`）

| 改动 | 为什么 |
|---|---|
| `SummaryStore<'a, F>` / `Session<'a, F>` → `SummaryStore<F>` / `Session<F>`，provider **按值拥有** | §3：原来那个借用描述错了所有权，服务器没法持有；调用侧（examples/tests/`cpp_ls`）全部改成传值或 `clone()` |
| `Session::text(path)` | hover/definition 要读**别的文件**的文本（显示声明、算名字所在行），不该为此解析一棵树 |
| `FileView::position_at(offset)` + `LineIndex::position_of(offset, text)` + `CppParseError::offsets()` | 协议要 (line, column)，引擎给的是字节偏移；`TextSize` 是 rowan 的类型，转换点收敛到 `cpp_parser` 内部一处 |
| `WatchFilter::ignore_pattern(PathPattern)`、`PathPattern`（新类型，`glob` 是私有依赖） | 客户端的 `exclude` 是 glob，不是目录；引擎原来的 `ignore(dir)` 表达不了。模式同时按**整条路径**和**相对根**匹配（`**/build/**` 与 `build/*.h` 两种写法都有人用）。解析失败由调用方报告（引擎不带 logger） |
| `PathPattern` 的匹配规则：`*` 不跨分隔符，`**` 跨；大小写跟随文件系统 | glob 默认 `*` 跨 `/`，那会忽略比写出来的更多的东西 |

### 服务器侧（`cpp_ls`）

| # | 文件 | 做了什么 |
|---|---|---|
| 1 | `context/analysis_state.rs` | `Session<DiskFiles>` + `OpenDocuments` 句柄；`open`（**替换**语义，重载走同一条路）、`run_blocking`/`query_blocking`、`update_session`、`wake`/`wait_for_work`（索引泵的信号）；窗口期内查询答 `Missing` |
| 2 | `context/query_runner.rs` | 只把引擎类型换掉（`Known` → `RequestOutcome` 的映射在 handler 里） |
| 3 | `context/workspace_state.rs` | 根目录（`PathBuf`）+ 打开的缓冲区 + 客户端配置；`contains` 直接用 `WatchFilter::is_ignored`，**"这文件是不是我们的"只有一个实现** |
| 4 | `context/workspace_manager.rs` | `compile_commands.json` 变更 → 2 秒防抖（新事件**取消**旧的等待）→ 代次号保证只跑一次 → 重载 = 重走初始化 |
| 5 | `context/diagnostic_service.rs` | 诊断 = `view.errors()`；每文件防抖 + 取消；全工作区一遍（带进度条、可取消）；**pull 与 push 两条路共用 `diagnose_file`** |
| 6 | `handlers/initialized/mod.rs` | 打开会话、再标缓冲区、**索引泵**（编辑后继续读，见 §1）、第一遍全工作区诊断、日志 |
| 7 | `handlers/configuration/mod.rs` | 重新问客户端配置，**相同就不重载** |
| 8 | `handlers/text_document/*` | `didOpen/didChange/didSave/didClose` 只入队；worker 里 `session.did_*` + `wake` + 排诊断；watched files → `session.changed(…)`；watch 注册改成**纯客户端**（删掉 `notify` 依赖） |
| 9 | `handlers/definition/mod.rs` | `Session::definition` + `DeclFact::name_range` → **真实 range**（名字那一段，不是文件开头） |
| 10 | `handlers/hover/mod.rs` | 重写（原 Lua 版 85 KB 删掉）：宏优先 → 声明的**原文**+限定名+类型/返回值+基类+条件/恢复标记+所在位置；`Hover.range` 故意不填（见该文件文档） |
| 11 | `handlers/diagnostic/*` | push/pull 共用 `diagnose_file`；pull 走 `analysis_query`（同 key latest-wins） |
| 12 | `lib.rs` / `util/` | 去掉 `meta_text`、`rust-i18n`；新增 `util/position.rs`（UTF-16 列 ↔ 字符列 ↔ 字节偏移，唯一实现）；`util/uri.rs` 修了两个真 bug（少 `file://` 前缀、`file:///p` 被当成相对路径），UNC 也走通 |
| 13 | 删掉 | `handlers/workspace/did_rename_files.rs`（Lua 的 `require` 改写，C++ 没有对应物）、`handlers/hover/build_hover.rs`、`server/mod.rs` 里的 `threads.join()` |

### 抓到并修掉的两个真 bug（handshake 测试的价值）

1. **编辑后索引永久失效**：`Session::did_*` 故意只丢摘要+入队，而服务器只在启动时 `advance` 过一次——
   用户一打字，那个文件就从索引里消失了，`definition`/`hover` 对它永远答 `null`。修法是索引泵（§1）。
2. **`exit` 之后不退出**：`run_ls` 结尾 `threads.join()`，而 IO 读线程阻塞在 stdin 上——
   客户端说了 `exit` 却不关管道时，服务器反而挂住。

### 5.1 工作区发现与配置（`cpp_code_analysis::project` + `include::{msvc, system_headers}`）

"这个工程是什么、怎么编译"原来散在三处（会话读数据库、服务器读客户端设置、没人读工程自己的文件），
现在是一层：**四个来源，每个都比前一个弱，每个答案都带来源标签**。

```text
.cppls.toml              人说的（唯一能覆盖其它一切的来源）
compile_commands.json    工程自己的构建（根目录 + 有界扫 build 目录，深度 ≤3、≤2000 个目录、排序保证可复现）
CMakeCache.txt           CMake 配置时留下的（编译器、CMAKE_CXX_FLAGS、标准、构建类型；只取 CMAKE_BUILD_TYPE 那一组）
工具链                   问编译器自己（GNU 系 -dM -E -v；MSVC 见下）
系统头目录               兜底：没有任何编译器能问时的约定目录，**标为猜测**
```

**`.cppls.toml` 的三条已定决策**（用户拍的）：

| 决策 | 内容 |
|---|---|
| `[compile].args` **赢** | 写了 `args` 就完全忽略数据库的 flags；`extra_args`/`remove_args` 在任何情况下都作用在"赢了的那份"上，一趟过：先删后加（所以"删掉数据库的 `-std=c++17`、加上 `-std=c++20`"结果是 c++20） |
| 一个文件管到 LSP 层 | `[diagnostics] on_change_ms`、`[hover] enable` 与引擎的键在**同一个文件、同一次解析**里：引擎解析并校验，LSP 只读同一个结构体（`Session::project_config()`），未知键报问题——不允许两个 crate 各读一半 |
| 逐节解析 | 一个坏键**不能**带走整份配置：每节单独解码，失败的节报问题并保持默认，其它节照常生效；未知的**节名**也报（那是最安静的一种错字） |

**工具链顺序**（`toolchain::discover`）：数据库 → `CXX`/`CC` → **平台默认（Windows 上 MSVC）** → `PATH` →
系统候选目录。`ToolchainSource` 记录哪一步答的（`CompileDatabase` / `Environment` / `PlatformDefault` /
`Path` / `SystemHeaders`），`Toolchain::note` 记下"宏环境是未知的"这类话——**猜可以，把猜说成事实不行**。

**MSVC：实测过，不是照文档猜**（证据在 [`msvc-notes.md`](msvc-notes.md)，子代理在本机跑的）：

```text
vswhere -latest -format json        24 ms，一次调用拿到安装目录 + 版本
VC\Tools\MSVC\14.35.32215           toolset 从磁盘枚举（四个 Host×target，本机无 ARM64）
INCLUDE 自己拼                       8 个目录，顺序与 vcvars64 一致（不存在的目录不写进去）
cl /nologo /Zc:preprocessor /PD /c  58 个预定义宏；**/PD 少了 /Zc:preprocessor 会在退出码 0 下什么都不打**
退出码 1/2/4/0                      "拿不到环境"/"命令行被拒或无头文件"/"资源加载失败"/"成功"——**空输出不等于空结果**
```

**不跑 `vcvars64.bat`**：它 1.3 s，而自己拼 `INCLUDE` + 绝对路径调 `cl` 只要 ~50 ms（实测 26×）。
所有 `cl` 诊断都是本地化的（本机只有中文资源，`VSLANG=1033` 无效），所以**只认退出码和 `D####`/`C####` 码，不认文本**。

本机跑 `examples/discover.rs` 的结果：MSVC 14.35.32215 + Windows SDK 10.0.22621.0、8 条 include、
58 个宏、方言 `Msvc`、来源 `PlatformDefault`。

**一条纪律**：上面这个默认值会改变**普查的口径**——455/128 两份清单是 mingw 的 libstdc++ 闭包，
而默认工具链现在在这台机器上是 MSVC。所以普查命令要**钉住工具链**（`$env:CXX = <mingw g++>`），
两份读数才可比；不钉的那一遍是**另一个测量**（用 MSVC 的 STL 读同一个清单），不要混着报。

**同一个道理，一条开着的回归（根因已查明）**：`std_query` 钉住 mingw 是 **9/9**（279 个文件的闭包），
不钉是 **0/9**——MSVC 的闭包只索引到 107 个文件，而原因是**宏展开出的 namespace**：

```text
MSVC 的 <vector>： `_STD_BEGIN` 出现 1 次，字面 `namespace std` 出现 0 次
yvals_core.h：     #define _STD_BEGIN namespace std {
```

我们的每文件事实是从 CST 上收的，`namespace std` 不是一个节点，于是 `std::vector` 这个事实不存在。
**不是"MSVC 不能用"，也不是工具链发现错了**——是作用域构建还看不穿"展开后是 namespace 的宏"，
属于 `grammar-gaps.md` 的宏族（设计级）那一档。登记在 [`roadmap.md`](roadmap.md) §4.2 ①。
（顺带记一个**不是**缺陷的：`xmm_func.h` 那条未解析 include 在 `#ifdef __ICL` 里，而 `configured = false`
让条件答 Unknown、Unknown 按"可能编译"跟进——代价是那一个文件不进缓存，设计如此。）

### 还剩什么（**不是待办清单，是缺口**）

| 缺口 | 现在的行为 | 位置 |
|---|---|---|
| 语义诊断（未解析的 include、没人声明的名字） | 只报解析错误；语义诊断要等索引完整（`Session::pending` 是那条线） | `handlers/diagnostic` + `docs/roadmap.md` |
| `document_symbol` / completion / references / rename | 没有能力行；引擎侧 `name_completions`/`member_completions`/`macro_references` 已经能用 | §6 第 2 条 |
| 多根工作区 | 只分析第一个根，其余记一行日志 | `handlers/initialized` |
| 工程配置文件（`.cppls.toml`） | 只读客户端的 `exclude`；不读自己的配置文件 | `client_config` |
| 非 UTF-8 文件 | `DiskFiles` 按 UTF-8 读，非 UTF-8 的文件读不出来 → 答 `Missing` | 引擎 `paths.rs` |
| 工作区级 pull 诊断 | 能力表里 `workspaceDiagnostics: false`（逐文件解析没有跨文件信息可加） | `handlers/diagnostic` |
| 成员访问的 `definition` 之外的查询（如 `w.size` 的 references） | 引擎有 `members_of`/`member_completions`；references 只做了宏 | `docs/roadmap.md` |

### 验收（这一遍跑过的门禁）

```text
cargo test --workspace                                 1092 通过（引擎 1064 + cpp_ls 28）
cargo test -p cpp_ls --test handshake                  1 通过（真进程、真 stdio、0.6 秒）
cargo clippy --workspace --all-targets                 0 警告
cargo doc --no-deps -p cpp_code_analysis               0 警告
cargo run -q -p cpp_parser --bin cpp_dump -- …/real_world.cpp     0 错误
cargo run -q -p cpp_code_analysis --example std_query  9/9
census：seeded 455 / seeded 128 / unseeded 455         455/0/0、128/0/0、454/1（与改动前逐文件一致）
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
7. **引擎不带的，服务器要带**：`Session` 没有线程、没有时钟、没有调度（这是它的设计），所以"谁一直调用
   `advance`""谁防抖""谁定时"都是这一层的职责。今天三处：索引泵（`initialized`）、诊断防抖
   （`DiagnosticService`）、配置重载防抖（`WorkspaceManager`）。**任何"改完引擎就完事"的想法都会漏掉它们。**
8. **`AnalysisState::wake()` 要跟着"入队"走**：任何让 `session.did_*`/`changed` 入队的地方，后面都要有一句
   `wake()`。漏了不会报错，只会让那个文件在索引里消失（这正是第一次端到端测试抓到的 bug）——
   加新的编辑类通知时，把这一句当成契约的一部分。
