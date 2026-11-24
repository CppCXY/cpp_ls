我将先查看最近的 Git 提交记录（尤其是最新一次关于锁的修改），并梳理 `cpp_ls` 与会话调度层的并发模型与请求处理流程。

Ran terminal command: git --no-pager log -n 10 --oneline

Ran terminal command: git --no-pager show --stat HEAD

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\mod.rs"
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\analysis_state.rs"
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\query_runner.rs"
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\snapshot.rs"
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\server\\message_processor.rs"
}

Ran terminal command: git --no-pager show 5ddef66b -- crates/cpp_ls/src/context/analysis_state.rs crates/cpp_ls/src/context/update_queue.rs crates/cpp_ls/src/handlers/mod.rs crates/cpp_ls/src/handlers/completion/mod.rs crates/cpp_ls/src/handlers/text_document/text_document_handler.rs

Read , lines 1 to 300
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\analysis_state.rs",
  "view_range": [
    1,
    300
  ]
}

Read , lines 301 to 650
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\analysis_state.rs",
  "view_range": [
    301,
    650
  ]
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\update_queue.rs"
}

Read , lines 1 to 260
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\workspace_manager.rs",
  "view_range": [
    1,
    260
  ]
}

Read , lines 1 to 250
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\diagnostics.rs",
  "view_range": [
    1,
    250
  ]
}

Read , lines 651 to 1200
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\analysis_state.rs",
  "view_range": [
    651,
    1200
  ]
}

Read , lines 1201 to 1800
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\analysis_state.rs",
  "view_range": [
    1201,
    1800
  ]
}

Read , lines 1 to 260
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\update_queue.rs",
  "view_range": [
    1,
    260
  ]
}

Read , lines 261 to 600
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\update_queue.rs",
  "view_range": [
    261,
    600
  ]
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\request_handler.rs"
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\notification_handler.rs"
}

Read , lines 1 to 250
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\initialized\\mod.rs",
  "view_range": [
    1,
    250
  ]
}

Read , lines 251 to 600
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\initialized\\mod.rs",
  "view_range": [
    251,
    600
  ]
}

Read , lines 1072 to 1260
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\context\\analysis_state.rs",
  "view_range": [
    1072,
    1260
  ]
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\text_document\\text_document_handler.rs"
}

Read , lines 1 to 160
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\hover\\mod.rs",
  "view_range": [
    1,
    160
  ]
}

Read , lines 1 to 160
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\semantic_token\\mod.rs",
  "view_range": [
    1,
    160
  ]
}

Read , lines 1 to 160
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\inlay_hint\\mod.rs",
  "view_range": [
    1,
    160
  ]
}

Read , lines 1 to 120
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\definition\\mod.rs",
  "view_range": [
    1,
    120
  ]
}

Read , lines 450 to 800
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\initialized\\mod.rs",
  "view_range": [
    450,
    800
  ]
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\diagnostic\\document_diagnostic.rs"
}

Read , lines 1 to 220
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\diagnostic\\mod.rs",
  "view_range": [
    1,
    220
  ]
}

Read , lines 1 to 220
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\completion\\mod.rs",
  "view_range": [
    1,
    220
  ]
}

Read , lines 1 to 220
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\mod.rs",
  "view_range": [
    1,
    220
  ]
}

Read , lines 1 to 260
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\lifecycle.rs",
  "view_range": [
    1,
    260
  ]
}

Read , lines 1 to 280
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\pump.rs",
  "view_range": [
    1,
    280
  ]
}

Read , lines 1 to 260
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\views.rs",
  "view_range": [
    1,
    260
  ]
}

Read , lines 762 to 1050
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\initialized\\mod.rs",
  "view_range": [
    762,
    1050
  ]
}

Read , lines 1 to 320
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\units.rs",
  "view_range": [
    1,
    320
  ]
}

Read , lines 321 to 650
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\units.rs",
  "view_range": [
    321,
    650
  ]
}

Read , lines 1 to 260
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\cook.rs",
  "view_range": [
    1,
    260
  ]
}

Read , lines 1 to 200
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\diagnostics.rs",
  "view_range": [
    1,
    200
  ]
}

Read , lines 1 to 200
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\tests\\suite\\latency.rs",
  "view_range": [
    1,
    200
  ]
}

Read , lines 161 to 320
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\semantic_token\\mod.rs",
  "view_range": [
    161,
    320
  ]
}

Read , lines 161 to 340
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\hover\\mod.rs",
  "view_range": [
    161,
    340
  ]
}

Search for `pub struct Session|pub fn catch_up|pub fn did_open_as|pub fn prepare_backgrou...`

Read , lines 220 to 400
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\mod.rs",
  "view_range": [
    220,
    400
  ]
}

Read , lines 320 to 450
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\semantic_token\\mod.rs",
  "view_range": [
    320,
    450
  ]
}

Read , lines 1 to 120
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\references\\mod.rs",
  "view_range": [
    1,
    120
  ]
}

Read , lines 1 to 130
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\rename\\mod.rs",
  "view_range": [
    1,
    130
  ]
}

Read , lines 1 to 120
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\code_lens.rs",
  "view_range": [
    1,
    120
  ]
}

Read , lines 1 to 260
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\indexing.rs",
  "view_range": [
    1,
    260
  ]
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\edits.rs"
}

Read , lines 401 to 620
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\mod.rs",
  "view_range": [
    401,
    620
  ]
}

Read , lines 281 to 560
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\pump.rs",
  "view_range": [
    281,
    560
  ]
}

Read , lines 260 to 420
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\indexing.rs",
  "view_range": [
    260,
    420
  ]
}

Read , lines 1 to 220
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\edits.rs",
  "view_range": [
    1,
    220
  ]
}

Read , lines 620 to 820
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\mod.rs",
  "view_range": [
    620,
    820
  ]
}

Read , lines 221 to 420
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\edits.rs",
  "view_range": [
    221,
    420
  ]
}

Read , lines 560 to 920
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\pump.rs",
  "view_range": [
    560,
    920
  ]
}

Read , lines 420 to 560
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\edits.rs",
  "view_range": [
    420,
    560
  ]
}

Read , lines 260 to 520
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\views.rs",
  "view_range": [
    260,
    520
  ]
}

Read , lines 921 to 1150
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_code_analysis\\src\\session\\pump.rs",
  "view_range": [
    921,
    1150
  ]
}

Read , lines 220 to 340
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\completion\\mod.rs",
  "view_range": [
    220,
    340
  ]
}

Ran terminal command: git --no-pager show 5ddef66b -- crates/cpp_ls/src/handlers/hover/mod.rs crates/cpp_ls/src/handlers/references/mod.rs crates/cpp_ls/src/handlers/rename/mod.rs crates/cpp_ls/src/handlers/signature_help/mod.rs crates/cpp_ls/src/handlers/semantic_token/mod.rs crates/cpp_ls/src/handlers/definition/mod.rs

Read , lines 1 to 100
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\signature_help\\mod.rs",
  "view_range": [
    1,
    100
  ]
}

Read 
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\server\\main_loop.rs"
}

Read , lines 1 to 120
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\inactive_regions\\mod.rs",
  "view_range": [
    1,
    120
  ]
}

Read , lines 1 to 160
Completed with input: {
  "path": "C:\\Users\\zc\\Desktop\\github\\cpp_ls\\crates\\cpp_ls\\src\\handlers\\mod.rs",
  "view_range": [
    1,
    160
  ]
}

Search for `pub struct AnalysisState|pub async fn completion_from_a_rendering|pub async f...`

Search for `pub struct Session<|pub fn catch_up|pub fn view\(|pub fn view_of_the_file|pub...`



**Task completed:**

基于对 `cpp_ls` 与 `cpp_code_analysis::session` 的全面代码审核，以及对最新提交 `5ddef66b`（`refactor schedule model`）的逐行分析，下面从**现状诊断**、**最新提交为何体验变差**、以及**推荐的新调度/并发架构方案**三个层面给出详细结论。

---

## 一、对当前语言服务器部分（`cpp_ls`）的审核

### 1. 已经做得很好的设计
- **请求分发与可取消性清晰**：`dispatch_request!` 配合 `ServerContext::task` 统一处理了参数校验、序列号屏障（`record_request`）、`CancellationToken`、panic 捕获与写回响应，handler 完全不碰连接层。
- **单写者有序事件队列（方向正确）**：`UpdateInbox` 保证 `didOpen`/`didChange`/`didSave`/`didClose`/`Configuration` 严格保序；`request_boundary` 防止跨越请求边界错误合并 `didChange`；`push` 仅在入队时持短锁。
- **后台泵的读写分离拆分思路**：`index_in_background` 已经把耗时的波次解析（`prepare_background_wave`）、整程序读取（`read_the_program`）、缓存解码（`a_stored_cook`）和 `render_cooks` 搬到了写锁之外，并在 `commit_a_wave` 里设置了 20 ms 时间预算（`COMMIT_MS`）。

### 2. 为什么最新提交 `5ddef66b` 体验不好？（核心问题定位）

`5ddef66b` 想实现的模型是：**“写入者负责在释放写锁前把编辑影响同步就绪，读请求只需获取读锁即可无条件安全查询”**。但实现中同时踩中了 5 个具体问题：

1. **把同步重活直接塞回了 `RwLock` 写锁内部（严重拖慢所有并发读与输入响应）**
   - 在 `process_batch` 中，每次 `didChange` / `didSave` 都会在 `update_session` 的**写锁内**同步调用 `session.catch_up(path)`。
   - 而 `Session::catch_up` 不仅会在写锁里解析当前文件（必要时还会触发 `the_timeline_to_read_through` 走完整闭包的 timeline），还会**同步读取最多 8 个未读 `#include`**（`INCLUDES_A_REQUEST_READS = 8`）！
   - 此前特意在 `AnalysisState::catch_up` 做过的“先无锁 `gather` + `spawn_blocking` 读文件 + 短写锁 `commit`”的优化被 `process_batch` 绕过，重新退化成“写锁里做 N 次 I/O + 解析”。
   - 同时，`process_did_open_text_document` 在 `UpdateInbox::applying` 锁内 `.await` 异步 `catch_up`，会阻塞后续所有文件的 `didChange`/`didClose`/`Configuration` 应用。

2. **“做了一半的同步准备”，导致即时性丢了、正确性也没拿到**
   - `session.catch_up(path)` 只更新了**未展开源码的 summary**（`index_one`），**并没有生成 cooked rendering**（`cook`），也没有读取 `import std;` 等模块单元。
   - 但 `completion_from_a_rendering` 在刚编辑完时，由于 `buffer_changed` 已经清除了旧的 `rendering`，`session.view(&path)` **必定返回 `None`**！于是：
     - 补全要么退回 `raw`（宏定义变量、`_STD` 命名空间宏等丢上下文），要么打上 `incomplete = true`；
     - `on_signature_help` 调用 `session.view(&path)?` 直接返回 `null`（因为没有任何地方触发 `cook`，且新提交把 `read_the_modules` 也删了）；
     - 所有请求删掉了 `prepare(&path)` 后，**未通过 `didOpen` 打开的文件**（如直接 Peek / 跳转查某个未索引头文件、或客户端未先发 `didOpen` 的文档请求）在 `session.view_of_the_file(&path)` 时因未进 `Vfs` 而直接返回 `null`。

3. **读写锁与 `gate` 的“写优先饥饿 + 线程池占满”放大效应**
   - `AnalysisState::run_blocking` 先拿 `blocking_permits` 和 `gate.read_owned().await`，再进 `spawn_blocking` 拿 `inner.read()`；`AnalysisState::update` 先拿 `gate.write_owned().await`，再调用 `block_in_place` 拿 `inner.write()`。
   - tokio 的 `RwLock` 是**写优先**的：只要一次 `didChange` 或后台泵把写锁挂在 `gate.write_owned().await` 上等待某个慢查询（例如 `on_hover` 里还保留着最长 2 秒的 `wait_for_the_closure` 或慢渲染），**所有新来的读请求都会立刻卡在 `gate.read_owned()` 上**，同时已经进入 `spawn_blocking` 的读线程卡在 Windows `SRWLOCK`（`inner.read()`）上，耗尽 `blocking_permits`。

4. **全局粒度 `ContentModified` 过度取消**
   - `UpdateInbox::apply_batch_upto` 只要消费任意事件就会更新全局 `started_change`，进而触发 `ServerContext::task` 取消**所有正在执行的请求**（返回 `-32801 ContentModified`）。
   - 编辑 `a.cpp` 时，正在进行的 `b.cpp` 语义着色 / InlayHint / 诊断 / WorkspaceSymbol 全部被取消重来；在连续打字时，补全或悬停极易在等锁的过程中被下一个按键直接 `ContentModified` 打断。

5. **同一批事件路径存在语义分裂**
   - `process_batch` 把 `Opened` 扔回 `others` 逐条跑 `process`，如果同一批里有 `[Changed(A), Opened(B), Changed(B)]`，`Changed(A)` 先跑（带 `catch_up`），而 `Changed(B)` 走了旧的 `process_did_change_text_document`（不带 `catch_up`），行为不一致。

---

## 二、为什么会出现“即时性 vs 正确性”两难？

根源在于：**`Session` 把四类生命周期与锁需求完全不同的状态揉进了同一把锁里**：

1. **编辑文本层**（当前 buffer、行号表、`languageId`）——需要**微秒级写入、零阻塞读取**；
2. **文件本地快速语法/局部符号层**（单文件 CST、局部变量/成员函数、前一次可用的头文件宏环境）——**只依赖当前文件文本 + 只读旧快照的宏表**，可纯函数并发计算；
3. **重度预处理/编译器视图层**（Closure timeline walk、`RenderedCooked`、Cooked AST）——**百毫秒到数秒级**，绝不能阻塞 `didChange` 或请求写锁；
4. **全项目索引层**（`ProjectIndex`、`SummaryStore`、工作队列、后台泵状态）——需要后台异步增量推进，且应通过**不可变/版本化快照**暴露给查询。

只要“应用一次按键”必须和“更新全项目索引/重建渲染/阻塞读锁”共用一把 `&mut Session` 锁，就必然在 **“等锁卡顿（无即时性）”** 与 **“直接读旧状态/返回 null（无正确性）”** 之间来回摇摆。

---

## 三、更好的设计方案：双轨快照 + Per-File 独立管线 + 无阻塞提交

参考 **rust-analyzer 的不可变快照（Snapshot Isolation）** 与 **clangd 的 TUScheduler（每文件异步管线 + Preamble 复用）**，结合 `cpp_ls` 已经拥有的 `OpenDocuments` / `RenderedCooked` / `prepare+commit` 基础，建议采用如下方案，改造成本可控且能同时解决即时性与正确性：

### 核心架构图

```text
LSP Input (didOpen / didChange / didClose)
   │  (0 ms: 仅更新版本号与文本快照，不解析、不 catch_up、不拿分析重锁)
   ▼
[1. 轻量文本状态]  DocOverlay (ArcSwap / Per-File RwLock) + Global/Per-File Version
   │
   ├─► 交互请求 (Completion / Hover / Inlay / SemanticTokens / Definition)
   │      │  拿一份不可变 AnalysisSnapshot (Arc指针拷贝，0 ms，永不与写者互斥)
   │      ├─ 快轨 (Fast Track, < 5~20 ms):
   │      │    直接用 RequestSnapshot.text + 上一次稳定 Preamble/MacroEnv 增量解析当前文件
   │      │    结合 IndexSnapshot 查询成员/类型/符号 → 立即返回高质量结果！
   │      └─ 精确轨 (Cooked Track, 仅按需):
   │           若当前已有匹配 content_hash 的 RenderedCooked → 直接用；
   │           否则根据请求语义决策：
   │             - Completion / Hover / Inlay：不阻塞！用“最新文本 + 上次稳定宏环境/Preamble”立即回答
   │               (对 Completion 标记 isIncomplete=true，对 Hover/Inlay 返回可用结果)
   │             - SignatureHelp：不再无脑 return None，无渲染时先降级到 raw/上次宏环境视图，就绪后补全
   │             - Diagnostics / SemanticTokens / References / Rename：走“版本对齐或等本文件就绪”
   │
   └─► 异步后台调度 (Actor / Worker 拆分)
          ├─ [Fast Lane — 焦点文件 Worker] (最高优先级，受 Debounce & Cancel 控制)
          │     对当前活跃文件做：1) 本文件 summary 更新；2) 复用缓存 Preamble / 按需增量 cook；
          │     全程无锁计算，算完后通过 1 ms 级别的原子指针切换发布新 Snapshot；
          │     发布后推送 `inactiveRegions` / `codeLens/refresh` / `inlayHint/refresh` / `diagnostic/refresh`。
          └─ [Background Lane — 项目索引泵] (低优先级，可随时让路)
                永远只做无锁 `prepare` + 极短批量写锁 `commit`（< 10 ms），绝不饿死读请求。
```

---

### 五项具体落地改造点（按收益从高到低）

#### 1. 彻底把 `Session` 的“只读快照”从全局 `RwLock` 中解放出来（消除等锁）
- **现状问题**：`AnalysisState` 用 `gate` + `RwLock<Option<Session>>` 保护整个 `Session`，后台每 `commit` 一次、或 `didChange` 调 `catch_up`，所有请求都在等锁；且写优先 `gate` 会把后续所有读请求一起冻住。
- **改进设计**：
  - 将 `ProjectIndex`、`SummaryStore` 内部的大表（`summaries`、`facts`、`macros`、`include_graph`）或外层 `SessionState` 拆分为 **持久化/写时复制快照（或 `ArcSwap<IndexSnapshot>` + 分片无锁/独立缓存）**：
    - 后台泵和写线程在**私有可写副本**或 **epoch-stamped delta** 上准备完毕后，只需极短时间合并并**原子替换 `Arc<Snapshot>`**（< 1 ms，甚至对正在跑的查询 **0 阻塞**）。
    - **即时效果**：任何查询拿到 `Arc<Snapshot>` 后**立刻释放任何共享状态锁**，查询执行期间**完全不会阻挡 `didChange` 或后台 `commit`**；后台 `commit` 也**永远不会让按键或补全等待**！
  - 如果短期内无法把 `ProjectIndex` 改成全持久化数据结构（`im` / 分块 `Arc<FileSummary>`）：
    - **最小可行替代方案**：去掉 `tokio::sync::RwLock`（`gate`）这个写优先全局门闩，改用 **“固定 1 个写者通道 + 极短 `parking_lot::RwLock`（纯内存指针/HashMap 插入，禁止在写锁内做任何 parse/IO/timeline walk）”**。**严格执行一条铁律：写锁内绝对不允许调用 `catch_up`、`index_one`、`parse`、`stat` 或磁盘读取！**

#### 2. `didChange` 回到“纯文本更新 + 触发异步焦点管线”，废除 `process_batch` 里的写锁 `catch_up`
- **现状问题**：`process_batch` 里 `session.catch_up(path)` 把解析 + 8 个头文件读取放进了写锁。
- **改进设计**：
  - `process_batch` 在写锁里**只做 3 件 O(1) 的事**：
    1. 更新 `OpenDocuments` / `Vfs` 文本与 `LineIndex`（注意：如果是当前正在编辑的超大文件，连 `LineIndex` 也可以延迟或在锁外算好传进来）；
    2. **保留上一次稳定的 `MacroEnvironment` / Preamble 给该文件复用**（见第 3 点），而不是清空后让所有功能归零；
    3. 把旧 `summary` 标脏（或标记 `dirty = true` 但保留旧 `summary` 作为 fallback），并将 `path` 投入 **Focus File Worker** 最高优队列。
  - **收益**：`didChange` 永远在 **< 1 ms** 内完成并推进 `applied` 序列号，彻底消除“输入时请求卡在 `catch_up_upto`”的问题。

#### 3. 借鉴 clangd Preamble：区分“头文件导语（Preamble）变更”与“函数体内部编辑”
- **关键洞察**：C++ 编辑时 **95% 的按键发生在函数体或类成员内部，根本没有增删 `#include` 或顶层宏**！
- **现状问题**：`Session::buffer_changed` 每次按键都会丢弃当前文件的 `summary` 和 `rendering`。于是每次打字，`completion_from_a_rendering` 和 `signature_help` 立刻丢失渲染视图。
- **改进设计**：
  - 为每个打开的文档提取并缓存 **Preamble（从文件头到最后一个预处理指令 `#include`/`#define`/`#if` 结束）的签名/哈希**（其实你们已有 `directive_signatures` 的雏形！）：
    - **Case A：只改了非 Preamble 区域（普通写代码）**
      - 它的 `#include` 闭包、宏环境 `MacroEnvironment`、以及“被它展开后的头文件部分”**100% 没变**！
      - 只需要用**已缓存的 Preamble 宏环境 + 新的 body 文本**对本文件做一次单文件增量重解析（或只重预处理本文件 body，几十微秒到几毫秒），就能立即得到**与编译器完全一致的本地作用域 + 渲染偏移 + 类型**！根本不需要重新走 `TranslationUnit::walk` 或重 cook 几百个头文件。
    - **Case B：修改了 `#include` 或条件编译指令**
      - 只有这时才在 Focus Worker 中异步重建 Preamble / 闭包与 `RenderedCooked`；在重建完成的 100~300 ms 窗口内，请求先用旧 Preamble + 新文本尽力服务。

#### 4. 请求按语义分级：不再“一刀切”取消或阻塞（解开“即时性 vs 正确性”死结）
不同 LSP 请求对“延迟”和“最新状态”的容忍度完全不同，应该**分级对待**：

| 请求类型 | 正确策略 | 当前做法的问题 |
|---|---|---|
| **Completion** | **绝对即时 + 有界降级**：拿到最新文本 + 复用上一次稳定宏环境/Preamble 立即 parse & 回答（< 20ms）；若本文件仍在 Focus Worker 中更新或索引未就绪，设置 `isIncomplete = true` 让客户端下一键再问。**禁止**在请求内等闭包或触发重渲染。 | 当前删掉了准备步骤，导致刚改完就 `view == None`，只能返回 incomplete raw 或缺失宏局部符号。 |
| **Hover / InlayHint** | **即时先给，就绪刷新**：不要在 `on_hover` 里死等 `2000ms`（`wait_for_the_closure`）！直接用当前已知最佳快照回答；对 `InlayHint` / `CodeLens`，后台 Focus Worker / 泵算完后发 `workspace/inlayHint/refresh` 和 `workspace/codeLens/refresh`。 | `on_hover` 仍可能阻塞 2 秒；删掉 `prepare` 后未打开的文件直接返回 `null`。 |
| **SignatureHelp** | **可用性优先**：当 `session.view(&path)` 尚未就绪时，回退使用 `view_of_the_file` + 上次 `MacroEnvironment` 查找函数名，不要直接 `?` 返回 `null` 导致提示框消失。 | `5ddef66b` 后每次 `didChange` 都会因 `session.view` 为 `None` 而直接返回 `null`。 |
| **Diagnostics (Pull/Push)** | **正确性优先（去抖 + 单文件就绪等待）**：只针对当前文件等待 Focus Worker 把最新的 `RenderedCooked` 和闭包的 summary 算完再出诊断（避免中途误报），但**绝不占用写锁去阻塞 Completion/Hover**。 | 目前 `prepare_for_a_diagnostic` 会自己循环做 24 次 `read_queued_file` + 两次写锁。 |
| **SemanticTokens** | 若索引未完全就绪或本文件仍在 cook，返回 `null`（协议允许重试）或先只发词法高亮，等 Focus Worker 完成发送 `workspace/semanticTokens/refresh`。 | 删掉 `prepare`/`catch_up` 后，未打开文件或刚改完的文件长期拿不到完整语义 token。 |
| **Definition / References / Rename** | 若目标文件未加载到 `Vfs`，由只读路径按需 `OverlayFiles::read` 读取临时文本（无需 `mut Session`），或交给后台 worker；`Rename` 在闭包未完全就绪时明确返回错误信息或拒绝，避免半套重构。 | 删掉 `prepare` 后，未曾 `didOpen` 的文件直接因 `vfs.held == None` 失败。 |

#### 5. 精细化 `ContentModified` 取消范围（按“文件 URI + 语义依赖”，而非全局取消）
- **现状问题**：`ServerContext::task` 监听全局 `started_changes`，任何文档或配置变化都会取消所有文件上的所有请求。
- **改进设计**：
  - 将 `started_change` 拆分为 **Per-Document 序列号映射** + **Global Epoch**（仅配置重载 / workspace 变更 / 跨文件头文件改动时递增）：
    - 正在编辑 `a.cpp` 时，**只取消针对 `a.cpp` 的旧位置型请求**（如 `a.cpp` 上的旧 `completion`、`semanticTokens`、`diagnostic`），不取消 `b.cpp` 的请求，也不取消用户主动发起的跨文件跳转。
    - 对于刚进入计算阶段的短查询（例如已拿到不可变快照且只需 5 ms），与其取消再让客户端重发（白白增加一次往返与等待），不如只取消**尚未开始计算（还在等屏障/等锁）**或**针对同一文档新版本的耗时请求（诊断、语义着色、引用搜索）**。
  - 同时，对于未通过 `didOpen` 打开的文件，在 `Vfs` 或只读查询里允许直接通过 `SessionFiles` 读取磁盘文本并构造局部 `FileView`（`Vfs` 本身完全可以做成内部 `RwLock` / `DashMap` 并发读缓存，不必因为读一个未缓存的磁盘文件去抢全库写锁）。

---

### 总结：推荐的演进顺序
如果想以最小改动快速把体验拉回来并根治两难，建议按以下 3 步推进：
1. **立刻止血**：把 `process_batch` 中的 `session.catch_up(path)` 移出 `update_session` 写锁；去掉 `on_hover` 的 2 秒阻塞等待；把 `Vfs` 变成并发可读（消除非编辑请求因为“读磁盘文件”而抢写锁的 `prepare` 需求）。
2. **引入“Preamble 保留 + 局部重解析”**：`buffer_changed` 不再清空整个 `RenderedCooked` / `MacroEnvironment`，若 `#include` 与宏指令未变，直接复用上一次稳定宏环境做本文件快速重解析，使 `completion` 和 `signature_help` 在编辑后 10 ms 内既有完整宏语义又无需等重 cook。
3. **完成快照隔离与 Per-File 取消**：将 `ProjectIndex` 切换为基于 `Arc` 的只读快照发布（写锁只做原子交换），并将 `ContentModified` 取消精确到文档粒度，再由单文件 Focus Worker 异步完成重 cook 并主动发送 `refresh` 通知。这样就能彻底摆脱“等锁卡顿”与“返回空/错结果”的两难。