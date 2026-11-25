# 语言服务器并发与调度重构计划

**状态**：设计冻结，待实施  
**基线**：`5ddef66b refactor schedule model`  
**输入**：`docs/review.md` 的审核结论、`docs/open-questions.md` 的实测、`channel.c` 的真实 LSP 运行记录  
**目标**：重做 `cpp_ls` 的“文档更新—语义准备—请求读取”边界，而不是继续在当前锁路径上增加超时和特判。

---

## 1. 结论先行

当前实现把四类生命周期完全不同的状态放进了同一把全局锁：

```text
Session
├─ 文档文本 / LineIndex       微秒级写入，必须即时可读
├─ 单文件摘要 / 局部作用域     毫秒级，文本变化后必须重建
├─ 预处理 / RenderedCooked     几十到几百毫秒，绝不能进请求路径
└─ 全项目索引 / 宏环境 / 队列  后台持续变化，必须快照化暴露
```

因此每次 `didChange` 都可能触发：

```text
写锁 → 应用文本 → catch_up（解析 + 读 include）→ 释放
  → 所有读请求排在这把锁后面
  → 补全、CodeLens、诊断、InlayHint 互相拖慢
```

最新提交 `5ddef66b` 的 `process_batch` 已经证明了这个模型的直接后果：

- 每个 `didChange` 在 `update_session` 写锁内调用 `session.catch_up(path)`；
- `Session::catch_up` 会解析当前文件并同步读取最多 8 个未读 include；
- 它仍然不生成 `RenderedCooked`，因此刚编辑后 `session.view(path)` 仍为 `None`；
- `completion`、`signature_help` 要么拿到 raw/incomplete，要么直接 `null`；
- 大文件编辑时，无关文件的补全也必须等这把全局写锁。

**重构方向**：

> 内容更新独占 writer；writer 在锁外准备语义快照；请求只读快照，永不改索引。  
> 锁只保护指针发布，不保护解析、I/O、cook 或索引构建。

---

## 2. 目标与非目标

### 2.1 目标

1. **请求路径零写入**：所有 `on_*` 请求只读；禁止 `prepare`、`catch_up`、`cook`、`queue_modules`、`update_session`。
2. **`didChange` 快速完成**：文本和版本在毫秒级应用；解析、include 读取、cook 不在写锁内。
3. **读写锁语义正确**：拿到读锁/快照时，看到的文本和语义都是该快照声明准备好的内容；没有 500 ms 猜测超时。
4. **编辑函数体后快速恢复语义**：复用 Preamble/MacroEnvironment，只重解析 body，使宏声明局部变量在编辑后仍可补全。
5. **请求按语义分级**：completion 不阻塞；诊断等强一致请求显式对齐版本；inlay/codeLens/semanticTokens 稳定结果 + refresh。
6. **取消按文档/依赖粒度**：编辑 `a.cpp` 不取消 `b.cpp` 的请求；编辑头文件按 include 依赖传播。
7. **可测量**：每个阶段都有锁持有、p95、CPU、错误答案的回归测试。

### 2.2 非目标

- 不追求一次性重写整个 `Session`。
- 不在这一轮重写 parser/type engine。
- 不通过“更短的超时”“更多 fallback card”掩盖语义未就绪。
- 不允许为了 latency 返回明知道过期的 hover/rename 结果。
- 不把 `parking_lot` 当成架构修复；锁类型只是实现细节。

---

## 3. 当前状态与证据

### 3.1 当前调用链

```text
LSP notification
  → UpdateInbox::push
  → update queue worker
  → process_batch
     → workspace_manager 修改文本
     → analysis.update_session("apply client content")  [WRITE LOCK]
        → session.did_open_as
        → session.catch_up(path)
     → schedule diagnostics
  → applied sequence 发布
  → request barrier (catch_up_upto) 释放
  → handler
     → snapshot_query / run_blocking  [READ LOCK]
     → session.view / type / member
```

### 3.2 已测到的问题

```text
cpp_ls 进程 6 分钟 CPU 330 s，内存 459 MB
index slice 112 次
写锁持有超过 50 ms 告警 82 次
prepare 超过 50 ms 告警 136 次
index slice: prepared 0 / committed 0 / write lock held 230–390 ms
codeLens 请求 150–610 ms
diagnostic 请求 600–981 ms
completionItem/resolve 等待 1.3 s
```

真实 `channel.c`：

```text
kvec_t(int64_t) ids = ...;
输入 ids  → 列表里没有 ids
ids.items → hover 依赖的 rendering 在编辑后不可用
```

### 3.3 根因

| 问题 | 根因 |
|---|---|
| 编辑后补全瘫痪 | `didChange` 在写锁内 catch_up；请求等锁 |
| 宏局部变量消失 | `buffer_changed` 丢弃 rendering，writer 没有重建/cook |
| hover 成员丢失 | rendering 不存在或被映射到错误 occurrence |
| CodeLens 闪烁 | 编辑后 drain 就 refresh；没有稳定版本/去抖 |
| 全项目抖动 | 全局 `ContentModified` 取消所有在途请求 |
| 未打开文件返回 null | 删除了请求 prepare，但没有只读 VFS 读路径 |
| 超时后错误答案 | `wait_until` 到期继续查询，没有“未就绪”语义 |

---

## 4. 目标架构

### 4.1 分层

```text
┌─────────────────────────────────────────────────────────────┐
│ LSP message loop                                             │
│  notifications → UpdateInbox                                │
│  requests      → snapshot read only                         │
└─────────────────────────────────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────────────────┐
│ DocOverlay (per document, lock-free / ArcSwap)              │
│  text, version, language, directive signature,              │
│  last stable preamble, semantic version                     │
└─────────────────────────────────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────────────────┐
│ Writer (single actor / queue worker)                        │
│  1. apply text (no parse, no I/O)                           │
│  2. classify edit: preamble changed?                        │
│  3. focus worker: body reparse or full re-cook, outside lock│
│  4. project pump: prepare in background, short commit       │
│  5. publish new immutable AnalysisSnapshot (ArcSwap)        │
└─────────────────────────────────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────────────────┐
│ AnalysisSnapshot (immutable, Arc)                           │
│  IndexSnapshot, MacroEnvironment, renderings, summaries,    │
│  per-file semantic versions                                 │
└─────────────────────────────────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────────────────┐
│ Requests                                                    │
│  read Arc snapshot → query → return                         │
│  no write lock, no session mutation, no parse/cook          │
└─────────────────────────────────────────────────────────────┘
```

### 4.2 五条铁律

1. **请求零写入**  
   `crates/cpp_ls/src/handlers/**` 不得调用任何 `&mut Session` 方法。  
   用测试或 CI 检查禁止符号：`update_session`、`catch_up`、`prepare(`、`cook(`、`queue_modules`。

2. **写锁禁止 I/O / parse / cook / include 读取**  
   writer 的短提交只允许：替换文本指针、插入已准备好的 summary/fact、更新版本号、发布快照指针。

3. **读锁/快照即就绪**  
   没有 `wait_until`、没有 500 ms、没有“超时后继续”。  
   快照要么声明该文件语义版本已就绪，要么明确返回 incomplete/旧稳定结果 + refresh。

4. **单写者**  
   文本更新、索引构建、快照发布都由同一 writer 顺序执行；后台 worker 只能准备 detached 数据，不能直接改共享索引。

5. **每文件语义版本**  
   `text_version` 与 `semantic_version` 分离。请求携带它读到的版本；返回内容必须能说明自己是哪一个版本。

### 4.3 建议的数据结构

```rust
struct DocState {
    text: Arc<str>,
    line_index: Arc<LineIndex>,
    version: i32,
    semantics: SemanticState,
    directive_signature: DirectiveSignature,
    preamble: Option<Arc<Preamble>>,
    last_stable: Option<Arc<FileView>>,
}

struct SemanticState {
    version: u64,
    status: Ready | Preparing | Failed,
}

struct AnalysisSnapshot {
    index: Arc<IndexSnapshot>,
    macros: Arc<MacroEnvironment>,
    summaries: Arc<SummarySnapshot>,
    renderings: Arc<Renderings>,
    versions: Arc<VersionMap>,
}
```

要求：

- `IndexSnapshot` / `SummarySnapshot` 内部必须结构共享（`Arc<FileSummary>`、分片 map、`im::HashMap` 或 epoch delta），不能每次复制全表；
- `ArcSwap<AnalysisSnapshot>` 只交换指针；
- `DocState` 的文本更新使用每文档 `ArcSwap`/分片锁，不经过全局 `Session` 写锁；
- writer 构建新快照时持有旧快照的 `Arc`，读者继续读旧版本，不会被阻塞。

---

## 5. 请求语义分级

| 请求 | 策略 | 不允许 |
|---|---|---|
| **Completion** | 最新文本 + 最近稳定 Preamble/MacroEnvironment 快速解析；宏局部符号可用；语义未就绪返回 raw + `isIncomplete=true` | 请求内 cook、等锁、返回一个“完整”的错误列表 |
| **SignatureHelp** | 同 completion；无 rendering 时用 raw + 旧宏环境；不要直接 `?` 返回 null | 每次编辑后签名提示消失 |
| **Hover** | 文本/语义版本匹配才返回确定结果；不匹配返回 null 或明确“未就绪” | 不返回过期类型当最终答案 |
| **InlayHint** | 返回上一稳定版本结果；语义版本前进后 `workspace/inlayHint/refresh` | 把空列表当最终结果且不 refresh |
| **SemanticTokens** | 可用时返回完整；否则只返回词法级或 null，并 refresh | 长期缓存旧的错误 token |
| **CodeLens** | 缓存每语义版本结果；结果变化才 refresh；编辑后 debounce | 每次 drain 都清空/重画 |
| **Diagnostics** | 正确性优先：等该文件语义版本；可取消、异步；不占写锁 | 用旧版本产生误报 |
| **Definition / References** | 目标文件未加载时走只读 VFS；索引不完整时可返回 partial/明确 unavailable | 请求内 prepare 写锁 |
| **Rename** | 依赖不完整必须拒绝或返回明确错误 | 半套 rename |

### 5.1 `isIncomplete` 的精确含义

- completion：当前快照没有足够语义，但返回了可用子集；客户端下一键应重问；
- 不再用 `isIncomplete` 表示“我超时了”；
- 不在请求里轮询/等待；refresh 是后台职责。

---

## 6. Preamble / Body 复用（最高优先级）

### 6.1 目标

编辑函数体时：

```text
旧 Preamble（#include 闭包、宏环境、头文件渲染部分） → 复用
新 body 文本                                              → 单文件重解析
```

只有 `#include` / `#define` / `#if` 等指令区变化时才重建 Preamble。

### 6.2 已有基础

- `Session::directive_signatures` 已存在；
- `Session::buffer_changed` 已计算 `DirectivesMoved { environment, layout }`；
- `Session::buffer_changed` 目前仍会 `store.forget` / 丢弃 cooked，这是要改的部分。

### 6.3 实施要点

1. 从 `directive_signature` 生成 `PreambleKey`；
2. `DocState` 保存 `Option<Arc<Preamble>>`；
3. **Case A：指令签名未变**
   - 复用旧 `MacroEnvironment`、旧 include 闭包、旧头文件渲染；
   - 用新 body 和旧 Preamble 做一次本文件增量预处理；
   - 重建本文件 `RenderedCooked` + `FileView`；
   - 更新 summary；
   - 发布新 `semantic_version`。
4. **Case B：指令签名变化**
   - 标记旧 Preamble 失效；
   - focus worker 在锁外重建完整 Preamble；
   - 期间 completion 用 raw + 旧宏环境；诊断/hover 明确未就绪；
   - 完成后发布新版本并 refresh。
5. 不允许复用跨越不同 `#if` 分支的宏环境；`PreambleKey` 必须包含环境条件。

### 6.4 验收

- 在 `channel.c` 函数体编辑后，`ids`、`ids.`、`KV_` 的 completion 在 20–50 ms 内正确；
- 编辑 `#include` 后允许一次较慢重建；
- 测试断言“函数体编辑不触发 full cook”。

---

## 7. 取消、版本与 refresh

### 7.1 版本

```rust
struct FileVersion {
    text: u64,
    semantic: u64,
}

struct VersionMap {
    global_epoch: u64,          // workspace/config change
    files: HashMap<PathBuf, FileVersion>,
    dependencies: IncludeGraph, // header → files
}
```

### 7.2 取消

- 请求只关心自己读取的版本；
- 文档文本变化：取消**同一文档**尚未开始的旧位置请求；
- 头文件变化：按 include 依赖图取消/重发受影响文件请求；
- 配置/workspace 变化：全局 epoch，取消全部；
- 不再让 `a.cpp` 的编辑取消 `b.cpp` 的 semantic tokens。

### 7.3 Refresh

只在以下条件发：

```text
semantic_version 前进
且 结果集合实际变化（diff）
且 距离上次 refresh > debounce（建议 150–300 ms）
```

CodeLens 额外要求：

- 返回上一稳定列表，不返回空占位；
- 结果未变不发 refresh；
- client 不支持的 refresh 能力时不发。

---

## 8. 实施阶段

### Phase 0：仪表与护栏（先做）

目标：先让“写请求”无处藏身，再改行为。

任务：

- [ ] 在 `docs/refactor-plan.md`/CI 增加 grep 检查：handlers 中禁止 `update_session`、`catch_up`、`prepare(`、`cook(`、`queue_the_modules`。
- [ ] 增加 `writer only` 的 API 命名/模块边界，例如把写方法放到 `writer` 模块，请求模块不导入。
- [ ] 记录每请求阶段：等待 applied、read lock、snapshot query、自身工作；记录 writer hold。
- [ ] 加并发压力测试：50 次快速编辑 + completion/codeLens/diagnostic/semanticTokens/inlay 同时请求。
- [ ] 删除所有 `wait_until` / 超时 readiness 代码。

验收：

- 任何请求 handler 的代码搜索不到写 API；
- 压力测试可复现当前 CPU/延迟；
- 没有“超时后继续查询”的路径。

### Phase 1：writer 短提交 + 只读请求

任务：

- [ ] `process_batch` 的 `update_session` 只做文本/LineIndex/版本更新，不调用 `catch_up`、不读 include、不 cook。
- [ ] `batch` 结束后发布 `text applied`；语义工作在 focus worker。
- [ ] 请求全部删除 `prepare`/`catch_up`/`read_the_modules` 写调用。
- [ ] `Vfs` 增加只读路径：未加载文件可由 `OverlayFiles`/磁盘读取临时文本，不抢写锁。
- [ ] `completion_from_a_rendering` 只读快照，不 `update_session`。
- [ ] `signature_help` 无 rendering 时回退 raw，不返回 null。
- [ ] 更新 handshake 测试：未打开文件、打开后首请求。

验收：

- `didChange` writer 文本提交 p95 < 5 ms；
- 请求不再出现 `prepare`/`catch_up` 锁等待；
- completion 不再因编辑返回错误完整列表。

### Phase 2：Preamble / Body 复用

任务：

- [ ] 引入 `PreambleKey` / `Preamble` 缓存。
- [ ] `buffer_changed` 区分 Case A/B。
- [ ] Case A 复用 MacroEnvironment + include 闭包，只重解析 body。
- [ ] Case B 后台重建 Preamble。
- [ ] 更新 `completion`/`signature_help`/`hover` 的无 rendering 降级。
- [ ] 测试 `ids`、`ids.`、`KV_`、宏成员、`ids.items`（宏参数）在编辑后立即正确。

验收：

- `channel.c` 函数体编辑后的首个 completion 含 `ids`；
- 函数体编辑不触发 full program read；
- Preamble 重建有明确状态和 refresh。

### Phase 3：稳定结果与 refresh

任务：

- [ ] CodeLens 按 semantic_version 缓存，结果 diff 才 refresh。
- [ ] InlayHint/SemanticTokens 缓存最后稳定结果。
- [ ] 编辑后 debounce（150–300 ms）再发 refresh。
- [ ] 去掉 CodeLens/SemanticTokens 的空列表“最终答案”。

验收：

- 连续编辑时 CodeLens 不消失再出现；
- Inlay 不出现空结果长期停留；
- refresh 次数与语义版本变化匹配。

### Phase 4：结构化快照

任务：

- [ ] `ProjectIndex`/`SummaryStore` 的 map 改为结构共享；
- [ ] `AnalysisSnapshot` 通过 `ArcSwap` 发布；
- [ ] 写者不再持有全局 `RwLock<Session>` 做查询相关的准备工作；
- [ ] 读请求 `ArcSwap::load` 后完全 lock-free（或只拿极短分片锁）；
- [ ] 内存预算和淘汰策略（renderings/summaries）。

验收：

- 编辑大文件时，无关文件 completion p95 < 100 ms；
- writer 发布快照时最长停顿 < 10 ms；
- 无长写锁告警。

### Phase 5：依赖级取消与最终压测

任务：

- [ ] `ContentModified` 按 `FileVersion` + include 依赖图；
- [ ] 全局配置/workspace epoch 只影响全局请求；
- [ ] 压测：快速编辑 + 所有 provider；
- [ ] 对照 `review.md` 的五项问题逐项关闭。

验收：

- 编辑 `a.cpp` 不影响 `b.cpp` 在途请求；
- completion p95 预算达标；
- 30 分钟真实 neovim 编辑无 CPU 持续 25%、无 CodeLens 抖动、无空补全。

---

## 9. 测试与基准

必须新增/保留的测试：

1. **锁边界测试**  
   `writer_only` API 搜索；测试断言 req handler 不产生写锁持有。
2. **编辑风暴**  
   `crates/cpp_ls/tests/suite/latency.rs`：
   - 快速 50 次编辑；
   - 同时请求 completion / codeLens / diagnostic / semanticTokens / inlay；
   - 断言 p95、无锁等待、结果版本一致。
3. **宏局部变量**  
   真实 `channel.c` 回归：
   - `ids` 补全；
   - `ids.` 成员；
   - `KV_` 宏；
   - `ids.items` hover。
4. **未打开文件**  
   直接对头文件/未 `didOpen` 文件请求 definition/hover，不能因无 Vfs 返回 null 而崩溃或永久空。
5. **超时不存在**  
   禁止测试里出现“等 500 ms 后继续”；用事件/版本断言。
6. **refresh 稳定性**  
   CodeLens/Inlay 连续编辑不闪烁；结果不变不 refresh。
7. **Preamble Case A/B**  
   body-only 编辑不触发 full cook；指令编辑触发重建。

性能预算（release，单文件 3k 行，i5/同机）：

```text
didChange 文本提交 p95          < 5 ms
body-only semantic ready p95    < 50 ms
completion p95（编辑后）         < 100 ms
completion p95（稳态）           < 50 ms
writer lock hold p95            < 10 ms
writer lock hold max            < 50 ms
无关文件 completion p95          < 100 ms
CodeLens 不阻塞 completion       0 次锁等待
```

---

## 10. 风险与开放问题

1. **ProjectIndex 结构共享成本**  
   `im::HashMap`/分片 `Arc<FileSummary>` 的内存与 CPU 需要基准；不能假设“原子交换 1 ms”。
2. **Preamble 复用的正确性**  
   宏环境、条件编译、include 路径、偏移映射都必须由 `DirectiveSignature` 覆盖；不能只比较 `#include` 文本。
3. **头文件编辑的依赖扇出**  
   一个 header 变化可能影响数百个 TU；需要 include 依赖图和 debounce，不能只按 URI。
4. **客户端缓存**  
   LSP 的 `isIncomplete`、refresh 能力、CodeLens 清除行为各客户端不同；以 VS Code/Neovim 实测为准。
5. **旧 rendering 的内存保留**  
   Preamble 复用会保留更多 rendering；必须有 LRU 和内存上限。
6. **快照发布与 GC**  
   旧快照被在途请求持有；需要确认 `Arc` 生命周期和内存峰值。
7. **测试环境**  
   并行跑 LSP suite 会放大 CPU/锁竞争；性能断言要单独串行或带环境门控。

---

## 11. Definition of Done

- [ ] 请求 handler 零写入，CI 可检查。
- [ ] 写锁内零 I/O/parse/cook，日志/测试可证明。
- [ ] 编辑后首个 completion 含宏声明局部变量。
- [ ] CodeLens/Inlay 无闪烁，refresh 只在版本/结果变化时发生。
- [ ] 大文件编辑时无关文件 completion p95 < 100 ms。
- [ ] 真实 neovim 30 分钟编辑：CPU 不持续高位、无空补全、无卡死。
- [ ] 所有 `wait_until`/超时 readiness 代码删除。
- [ ] 文档更新：`docs/open-questions.md` 记录剩余风险，`docs/refactor-plan.md` 标记完成项。

---

## 12. 对 `docs/review.md` 的取舍

**采纳**：

- `catch_up`/I/O/parse 不得在写锁内；
- `didChange` 应是 O(1) 文本更新；
- Preamble/MacroEnvironment 复用；
- 请求语义分级与 refresh；
- VFS 只读路径；
- 按文件/依赖的取消；
- 快照隔离大方向。

**修正**：

- 不是简单换 `parking_lot`；必须有“写锁内禁止重活”的强制边界。
- 不是“删 prepare 就行”；必须有只读加载路径。
- Hover 不无条件返回旧结果；过期结果不能当确定答案。
- Preamble 95% 是假设，必须先测量。
- 结构化快照需要结构共享，不能每次复制全索引。
- 诊断/rename 仍需要版本对齐；不是所有请求都能“立即返回”。

---

## 附录 A：当前关键文件

| 文件 | 角色 |
|---|---|
| `crates/cpp_ls/src/context/analysis_state.rs` | gate + `RwLock<Option<Session>>`、query/update |
| `crates/cpp_ls/src/context/update_queue.rs` | 单写者队列、`process_batch` |
| `crates/cpp_ls/src/context/snapshot.rs` | `ServerContextSnapshot` |
| `crates/cpp_ls/src/handlers/request_handler.rs` | 请求分发、`record_request`、取消 |
| `crates/cpp_ls/src/handlers/completion/mod.rs` | completion 当前读 rendering |
| `crates/cpp_ls/src/handlers/text_document/text_document_handler.rs` | didOpen/change/save/close |
| `crates/cpp_code_analysis/src/session/edits.rs` | `buffer_changed`、摘要/rendering 失效 |
| `crates/cpp_code_analysis/src/session/pump.rs` | 后台索引/ cook |
| `crates/cpp_code_analysis/src/session/indexing.rs` | `catch_up`、include 读取 |
| `crates/cpp_code_analysis/src/session/views.rs` | view/view_of_file/rendering |
| `crates/cpp_ls/tests/suite/latency.rs` | 延迟与编辑风暴 |
| `crates/cpp_ls/tests/suite/handshake.rs` | 真实 LSP 行为 |

## 附录 B：实施记录

每次只推进一个 Phase，提交必须包含：

1. 变更内容；
2. 锁持有/延迟前后数据；
3. 新增回归测试；
4. 尚未关闭的风险。

**不要**在一个提交里同时改锁、cook、Preamble 和 provider policy。

---

## 实施记录

### 2026-11-25：Phase 0 + Phase 1 的第一段

已完成：

- `process_batch` 按客户端顺序应用文本：`Opened` / `Closed` / 非内容事件是批处理边界，不会再把
  `Changed(A)` 排到 `Opened(B)` 前面。
- `didChange` 的 `update_session` 只做文本替换；`session.catch_up` 已移出写锁。
- 每个受影响文件（以及当前所有 open 文档，作为 header 依赖的保守集合）由 focus worker 异步
  `catch_up`，完成后发布 per-file semantic ready 事件。
- completion / inlay 等待的是**该文件的 semantic ready 事件**，不是全局锁、不是超时；其他文件的请求不等待。
- writer 只把“文本已应用”作为 request barrier；语义工作由 focus worker 发布。
- `Closed` 仍走有序的 side-effect 路径（清诊断、清 inactive regions、回退到磁盘）。

验证：

```text
cargo test --release -p cpp_ls --test suite
  118 passed, 0 failed, 11 ignored

large edit -> unrelated completion:
  p50 3 ms, p95 5 ms, max 13 ms

large edit -> same-file completion:
  p50 417 ms, p95 446 ms, max 580 ms

neovim channel.c, after edit:
  ids. members, ids.items hover, ids name, KV_ 全部通过
  completion min 90 ms, mean 94 ms, max 100 ms
```

结论：

- **无关文件请求的锁等待已经消失**（p95 5 ms）。
- 同一文件的请求仍要等该文件的 focus catch-up（p95 446 ms）；这是 Phase 2 Preamble/body-only
  复用要解决的目标，不是等待策略能解决的。
- 下一阶段不要把 `catch_up` 放回写锁，也不要用超时替换这个事件等待。


### 2026-11-25：1000 行 writer 实测，Preamble 的目标修正

严格单写多读 RwLock 下，1000 行 fixture 的一次编辑：

```text
catch_up                约 22 ms
cook gathered           0 ms
cook rendered           21 ms
cook committed          5 ms
cook total              27 ms
one content batch       50–57 ms
```

所以此前“1000 行 p95 < 30 ms”的目标不成立：即使 Preamble 把本文件 render 降到 0，仍有
`catch_up 22 + commit 5 = 27 ms` 的固定成本，再加查询与 LSP。

修正后的验收：

- 1000 行、无头文件闭包：same-file completion p95 < 50 ms（当前 76 ms）；
- 头文件闭包大的文件：Preamble 后 `cook rendered` 显著下降，而不是强行压到 30 ms；
- `preamble_end` 之后没有指令的文件才走 body-only 路径；指令区不变但 body 变化时复用
  preamble 的 source prefix、rendered text/spans 与 MacroEnvironment。
### 2026-11-25：Preamble 接线

`RenderedCook` 现在携带 `Arc<RenderedCooked>`；`commit_a_cooked` 验证 text hash 后用
`DirectiveSignature.preamble_end` 截取并缓存 prefix；`cooking_materials` 只在该 prefix 的源码和
`preamble_end` 都未变时提供它；`render_cook` 只把 `preamble_end` 之后的 tokens 交给
`cook_with_file_and_features`，再用 `RenderedCooked::join` 拼回完整 rendering，最后照常 index。

实测 `channel.c`（release，编辑包含大量 include 的文件）：

```text
before edit   min 93 ms, mean 155 ms, max 375 ms
after edit    min 86 ms, mean  88 ms, max  91 ms
```

1000 行、无 directive 的 fixture（Preamble 无前缀可复用，body 就是全文件）：

```text
unrelated completion  p95 59 ms
same-file completion  p95 73 ms
diagnostic            p95 65 ms
```
### 2026-11-25：neovim 多文件并发测量

被测文件：`channel.c`、`buffer.c`、`message.c`、`eval.c`。每个文件单独编辑一次，然后在编辑后同时发
completion、codeLens、inlayHint、semanticTokens/full、diagnostic 五个请求。

顺序基线（只测 completion）：

```text
channel.c   338 ms, 1 item
buffer.c    930 ms, 1000 items
message.c   277 ms, 0 items
eval.c     2151 ms, 1000 items
```

`channel.c` 五个 provider 顺序执行：

```text
completion        391 ms
codeLens          127 ms
inlayHint        1200 ms
semanticTokens   2753 ms
diagnostic        551 ms
五者合计        5022 ms
```

同样五个 provider 对同文件并发：

```text
p50 3812 ms, p95 6839 ms, max 7426 ms（20 个请求，四个文件）
provider 中位：codeLens 3889, inlayHint 4113, completion 5140,
               semanticTokens 3330, diagnostic 6477 ms
```

结论：**并发没有带来并行加速**；五个请求的总延迟接近顺序执行之和。它们分享同一套 analysis 工作，
而且在编辑后的 writer batch 上排队。writer 阶段本身：

```text
channel.c  catch_up 199 ms, cook 224 ms, batch 454 ms
buffer.c   catch_up 269 ms, cook 237 ms, batch 1471 ms
message.c  catch_up 251 ms, cook 240 ms, batch 1837 ms
eval.c     catch_up 303 ms, cook 238 ms, batch 1298 ms
```

batch 明显大于 catch_up+cook，说明编辑之外的 index/pump 工作也在同一 publication 边界内排队。

可用性：20 个请求全部有响应，没有崩溃/超时；但 `message.c` 选的基线位置 completion 为 0 项，
该位置不是可靠可用性样本。
### 2026-11-25：并发慢的根因不是 provider，而是写锁内的秒级提交

neovim 四文件并发测量中，server 日志给出了真正阻塞请求的 write hold：

```text
index slice: prepared 0 in 0 ms, committed 0, write lock held 1 ms,
             16 to cook, 189 left
index slice tail: re-read 0, cooked 16, write lock held 3520 ms
                 (re-read 812 ms, cooked 2708 ms)

index slice tail: re-read 0, cooked 11, write lock held 3991 ms
                 (re-read 400 ms, cooked 3591 ms)

index slice tail: re-read 0, cooked 8, write lock held 6207 ms
                 (re-read 1072 ms, cooked 5135 ms)
```

同一窗口还有：

```text
commit a diagnostic rendering held the analysis for 237–256 ms  (×4)
prepare a file a pull request named held the analysis for 231–265 ms  (×4 per edit)
apply client content held the analysis for 431–539 ms
```

所以并发 p95 5.6–6.8 s 的直接原因不是“provider 并行度不够”，而是所有 reader 都在等这些写锁：

1. `index slice` 的 tail 在同一写锁内提交 8–16 个 cooked rendering，单次 3.5–6.2 s；
2. `prepare a file a pull request named` 是 diagnostics 请求内的写路径，每个 230–265 ms；
3. `commit a diagnostic rendering` 是诊断请求内 cook/commit，每个 237–256 ms；
4. `apply client content` 的 writer catch_up+cook 本身 431–539 ms。

诊断请求内写路径不能直接删：删掉后 handshake 有 8 个测试失败，因为 didOpen 只 catch_up、不 cook，
未打开文件的诊断也没有 writer 先准备。要先把“打开/诊断所需的 rendering”交给 writer，再删请求内 prepare。

下一步按优先级：

1. 把 cooked rendering 的磁盘持久化移出 `index slice` 写锁，写锁只做内存/index insert；
2. 让 writer 在 didOpen 时 cook 当前文件，diagnostics 对已打开文件只读；
3. 未打开文件的诊断走 writer 请求，不在 reader 路径里写；
4. 重跑本节的四文件并发基准，验收 writer hold 和 completion p95。
### 2026-11-25：等待 45 s 后仍是 index 活跃期

把 settle 从 12 s 提到 45 s 再跑同一并发基准，索引仍有大量文件在 cook；日志继续出现
`index slice tail ... write lock held 1129–7342 ms`。本次 `channel.c` 的 edit：

```text
catch_up 206 ms, cook 236 ms, apply client content 447 ms
one content batch 8771 ms
```

也就是说 **8.3 s 不是在 writer 内做文本/语义，而是在等 index slice 的写入口**。

45 s settle 后的数据：

```text
baseline completion: channel 90 ms / 1 item, buffer 682 / 1000,
                     message 24 / 0, eval 1880 / 1000
channel 五 provider 顺序: completion 87, codeLens 134, inlayHint 1145,
                        semanticTokens 359, diagnostic 12 ms（合计 1737 ms）
20 个并发请求: p50 2636 ms, p95 5632 ms, max 6939 ms
provider 中位: completion 4815, diagnostic 2482, codeLens 2636,
               inlayHint 2913, semanticTokens 2167 ms
```

所以这不是“steady state 下 provider 并发差”，而是 **索引还没结束**：冷启动的
`index slice` 与编辑、五个 provider 抢同一个 publication 边界。并发测量必须再加一档
“索引完全空闲后”的数据，当前数字只能描述启动期。

这也解释了为何顺序五 provider 合计 1.7 s，而并发 20 请求 p95 5.6 s：并发请求在
index slice 的秒级写锁上排队，不是并行计算。