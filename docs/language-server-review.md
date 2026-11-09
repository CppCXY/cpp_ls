# C++ 语言服务器评审：编辑延迟与架构

> 评审基线：提交 `2d39794`，2026-11-09。范围为 `cpp_ls` 的 LSP 生命周期、请求调度、
> 文档同步及其调用的 `cpp_code_analysis` 索引与查询路径；不是对全部 C++ 语义、格式化器或安全性的完整审计。
> 下文保留基线时的观察；后续实现变化和未完成项见“修复状态”。修复后本地文件行号可能移动，
> 需要复核历史证据时请以该基线提交为准。

## 修复状态

以下状态针对评审后的工作树，不改变后面的基线数据或结论：

| 问题 | 状态与范围 |
|---|---|
| L1、L3、S1 | **部分改善。** LSP 支持按 UTF-16 坐标应用增量更改；打开文件解析、诊断渲染及后台 cook 的主要计算已移出全局写锁。仍会复制文本、重建行索引和完整解析；部分依赖准备和查询仍持有全局读锁。 |
| L2 | 后台模块处理不再在写锁内同步清空全局工作队列，改用分离准备和提交的 program-read 路径。 |
| C1、C4 | wave、文件、program、drain 和 cook 的提交校验 session/输入版本；重载在锁外构造 session，校验 epoch 并在切换时重放最新缓冲区，关闭时取消待执行重载。版本校验目前是保守的全局粒度，细粒度依赖快照和准备阶段共享缓存副作用仍值得进一步审计。 |
| C2、C3 | 请求形成合并屏障；较新变更开始时返回 `ContentModified`，显式取消返回 `RequestCanceled`，初始化期间的取消也会保留。当前并非不可变 snapshot；同步分析仍缺少协作式取消，已经开始的阻塞操作可能继续持锁或延迟取消响应。 |
| C5 | 对旧客户端提供带版本校验和防抖的 push 诊断，支持的客户端继续使用 pull 及合并刷新；无法生成报告时返回错误而非伪造空结果。 |
| S2、S3 | 已加入版本、同步、监听及 release 延迟回归；跨文件编辑卡顿明显下降，但同文件尾延迟仍未达到最初建议的 150 ms 目标。 |
| S4 | 无动态注册能力的客户端使用原生文件监听，并在监听失败时提示；无根工作区使用首个打开文件的父目录。当前仍只分析第一个工作区，未实现完整多根工作区或动态文件夹切换。 |

`Session::commit_a_unit_reading` 的返回类型现为 `Option<UnitReading>`：`None` 表示准备结果已过期，
调用方不得将其视为已提交，应在仍需要该结果时重新准备。

原生监听依赖操作系统事件；网络文件系统及通知丢失等平台限制仍然存在。监听器报告事件丢失或识别到目录结构
变化时会防抖后重新加载工作区，但不能保证所有平台都准确报告每个目录事件，也不是轮询替代方案。
客户端若声称支持动态监听、但随后拒绝注册，目前也不会自动切换到原生监听。传输队列背压、逐文件/依赖版本、
真正的增量解析以及协作式取消仍是后续工作。

修复过程运行了 `cargo test --offline --workspace --all-features --quiet -- --test-threads=4`，
以及 CI 对应的 `cargo clippy --offline --workspace --all-targets --all-features -- -D warnings`；
两者均通过。后续对监听器复用和请求补齐路径的调整，又单独重跑了版本发布测试及服务器库、握手和空闲测试。
被标记为 `ignored` 的真实工程或手工探针没有自动运行。

### 修复后的延迟抽样

在同一台 Windows 测试机上使用 `--release`、stdio 和 168,972 字节的合成文件（10,000 个声明），
每次只通过增量 `didChange` 修改文件头部等长注释，再立即发送请求。补全校验了预期成员，诊断校验了完整报告的
`items` 形状：

| 场景 | 样本数 | P50 | P95 | 最大值 |
|---|---:|---:|---:|---:|
| 编辑大文件后补全另一个小文件 | 20 | 7 ms | 41 ms | 62 ms |
| 编辑大文件后补全该文件 | 20 | 529 ms | 676 ms | 858 ms |
| 编辑大文件后拉取诊断 | 10 | 615 ms | 643 ms | 643 ms |

命令为 `cargo test --release --offline -p cpp_ls --test latency
an_unrelated_completion_stays_responsive_after_a_large_incremental_edit -- --nocapture --test-threads=1`。
这次测试使用增量同步和 `debug` 文件日志；下方基线使用 FULL 同步和 `warn` 日志，文件内容也略有差异，
所以不是严格的同条件 A/B。诊断只有 10 个样本，P95 即最大值。跨文件等待在本次样本中显著降低，
**同文件补全和诊断仍有明显卡顿，不能视为已达到 150 ms 目标。** 测试中的 1,000 ms 同文件阈值只是
严重卡顿保护，不是该目标的验收门禁。

## 结论

当前设计有清晰的分层，也做了不少正确的响应性工作：未保存缓冲区覆盖磁盘、打开文件优先索引、请求不等待整个项目
完成、补全可以表示不完整，以及部分重计算已拆成准备和提交两步。**但这些机制尚未形成端到端的延迟和版本一致性保证。**

最需要优先处理的是三个相互关联的问题：

1. 编辑和部分后台工作仍在全局 session 写锁中执行完整解析或批处理；一个文件变慢，其他文件的交互也会等待。
2. 锁外准备的结果没有统一携带并验证工作区、文档及依赖版本，旧结果可在新编辑之后提交。
3. 请求队列、取消和所谓 snapshot 的边界没有真正绑定到同一份文档版本；旧请求既可能继续消耗资源，也可能看到较新的文本。

这里的 **P0** 表示建议优先处理的全局卡顿或错误结果风险，**P1** 表示明确的交互/一致性问题，**P2** 表示
可扩展性、兼容性或可观测性风险。证据分为“本次实测”“确定性反例”和“代码推断”；代码注释里的历史数字不作为
本次测量结果。

## 本次测量与验证

### 编辑到响应的端到端测量

环境为 Windows 10 x64、Intel i5-9400、6 逻辑处理器、Rust 1.99.1，`--release` 服务器，通过本机 stdio
发送真实 LSP 消息；日志级别为 `warn`。每种大小使用新建的合成工作区、一个不含 `#include` 的 C++ 文件。
先等到补全包含 `Widget::member`，再等待 1 秒，随后分别测量无编辑请求和每次 `didChange` 后立即发出的请求。
诊断只校验返回了带 `items` 数组的完整报告，并未逐条验证诊断语义；补全每次都校验了预期成员存在。

| 场景 | 样本数 | P50 | P95 | 最大值 |
|---|---:|---:|---:|---:|
| 985 字节，未编辑，补全 | 25 | 0.5 ms | 1.0 ms | 1.2 ms |
| 985 字节，编辑后补全 | 25 | 3.3 ms | 4.5 ms | 4.9 ms |
| 985 字节，编辑后诊断 | 10 | 8.4 ms | 9.0 ms | 9.0 ms |
| 168,985 字节，未编辑，补全 | 25 | 13.7 ms | 14.4 ms | 16.4 ms |
| 168,985 字节，编辑后补全 | 25 | 466.7 ms | 542.0 ms | 609.2 ms |
| 168,985 字节，编辑后诊断 | 10 | 694.0 ms | 1,063.7 ms | 1,063.7 ms |
| 大文件不变时，另一个小文件的补全 | 20 | 0.8 ms | 2.2 ms | 9.0 ms |
| 先编辑大文件，再补全另一个小文件 | 20 | 470.5 ms | 938.1 ms | 1,076.3 ms |

大文件由 10,000 个简单全局变量声明、一个 `Widget` 和一次 `w.` 成员访问组成；每次编辑仅改变文件开头的
`// revision N` 注释。计时从发送完整文本的 `didChange` 开始，到收到紧随其后的响应为止，包含客户端序列化、
传输、排队和服务端处理。P95 用最近秩计算；**10 个诊断样本的 P95 就是最大值**，不能据此估计稳定的尾延迟。
这是单机、单次的合成场景，不代表任意真实项目或编辑器的 UI 卡顿时间。

大文件测试的服务端警告进一步显示：标记为 `didOpen` 的更新出现 56 条超过 50 ms 的写锁持有记录，范围
261–615 ms，中位数约 278 ms；该标签也被 `didChange` 路径复用。另有 10 条诊断准备写锁记录为
256–549 ms。这些是**超过阈值才输出的样本**，不是全部锁持有时间的分布。

复现时可按以下形状生成文件，光标放在 `w.` 的点之后；测试客户端应保留相同的预热、采样次数和结果校验：

```python
def source(count, version):
    declarations = "".join(f"int global_{i};\n" for i in range(count))
    return (
        f"// revision {version}\n"
        "struct Widget { int member; int method(); };\n"
        f"{declarations}"
        "void run() {\n    Widget w;\n    w.\n}\n"
    )
```

使用 `count=60` 和 `count=10000`，分别发送 `didOpen`，然后对每个递增的 `version` 发送 FULL
`didChange` 和紧随其后的 `textDocument/completion` 或 `textDocument/diagnostic`。跨文件场景中，在同一
workspace 打开一个含 `Small::small_member` 的小文件：先修改大文件，再立刻请求小文件的成员补全。可参照
现有 [LSP 测试客户端](../crates/cpp_ls/tests/latency.rs#L379) 实现消息收发。

### 已运行的现有测试

- `cargo test --offline --release -p cpp_ls --test latency -- --nocapture --test-threads=1`：2/2 通过。
  该次运行首个补全为 941 ms，其后 11 次为 4–8 ms；测试本身并未发送 `didChange`。
- `cargo test --offline --release -p cpp_ls --test handshake a_completion -- --nocapture --test-threads=1`：
  5/5 通过，包括紧随编辑和头文件变更后的补全。
- `cargo test --offline --release -p cpp_ls --test diagnostics_stable -- --nocapture --test-threads=1`：
  1/1 通过。
- 为验证下面的时序，临时加入过隔离探针并已删除；探针是对**当前实际行为**的断言，其通过不代表这些行为正确。
  下文保留调用顺序与观测结果。

## 合理的设计

1. **LSP 外壳与分析引擎分层。** handler 负责协议与能力注册，`Session` 负责 C++ 语义，避免把协议类型扩散到
   解析和索引中；请求通过统一分发和结果类型转换。见 [request_handler.rs](../crates/cpp_ls/src/handlers/request_handler.rs#L76)
   和 [query_runner.rs](../crates/cpp_ls/src/context/query_runner.rs#L39)。
2. **打开缓冲区优先于磁盘。** 编辑器文本作为 overlay，文件监听通知跳过已打开文件，避免保存时的磁盘事件覆盖
   未保存内容。见 [session/mod.rs](../crates/cpp_code_analysis/src/session/mod.rs#L335) 和
   [watched_file_handler.rs](../crates/cpp_ls/src/handlers/text_document/watched_file_handler.rs#L47)。
3. **通知先入队而不是在 LSP 读取循环中直接解析。** 顺序化应用 `didOpen`/`didChange`/`didClose` 是正确的，
   FULL 同步下相邻完整文本合并也能减少无用解析；只是当前合并与请求边界存在缺陷，见 C2。
4. **前台不等待整个项目索引。** 索引区分打开文件和其余文件，补全对未完成或截断的结果设置 `isIncomplete`，
   且延后解析单个 completion item 的文档；这比为了完整结果全局阻塞交互更合理。见
   [worklist.rs](../crates/cpp_code_analysis/src/index/worklist.rs#L1) 和
   [completion/mod.rs](../crates/cpp_ls/src/handlers/completion/mod.rs#L209)。
5. **昂贵工作与提交分离是正确方向。** 后台 wave、drain 和普通 program read 已有准备/提交分层；
   `commit_a_cooked` 还检查了目标文件文本哈希，说明项目已经有“旧工作不得覆盖新文本”的局部实践。见
   [analysis_state.rs](../crates/cpp_ls/src/context/analysis_state.rs#L350) 和
   [session/mod.rs](../crates/cpp_code_analysis/src/session/mod.rs#L7743)。
6. **已有资源上限与协议坐标转换。** 渲染和视图缓存同时有条目及权重上限，VFS 将文本和行索引一起保存，
   LSP 坐标按 UTF-16 转换；这些取舍分别针对内存和位置正确性。见
   [cache_lru.rs](../crates/cpp_code_analysis/src/cache/cache_lru.rs#L37)、
   [vfs.rs](../crates/cpp_code_analysis/src/file/vfs.rs#L131) 和
   [position.rs](../crates/cpp_ls/src/util/position.rs#L126)。

## 优先问题

### L1 · P0：单次编辑把整文件工作放在全局写锁内

**证据：本次端到端实测；调用链确定。**
[apply_buffer](../crates/cpp_ls/src/handlers/text_document/text_document_handler.rs#L164)
在 `AnalysisState::update_session` 写锁中调用 `did_open` 和 `advance(1)`。
`buffer_changed` 替换文本、重建 VFS 行索引、丢弃摘要并重新排队；
`advance(1)` 不只是“一次插入”：它可以解析文件，并在尾部同步执行
[`prepare_the_drain` 和提交](../crates/cpp_code_analysis/src/session/mod.rs#L3129)。
诊断慢路径又在同一写锁里执行 `advance(24)` 和 `cook`，见
[analysis_state.rs](../crates/cpp_ls/src/context/analysis_state.rs#L641)。

因此“文件数为 1/24”不是时间上限：大文件、复杂宏、索引修复和磁盘/缓存操作的成本都可能落在临界区。
前面的跨文件实验显示，这不仅是当前补全自身慢：无关文件的补全也从 P95 2.2 ms 上升到 938.1 ms。

**建议：** 写入缓冲区及其版本应尽量短；将解析、refinement 和 cook 从全局写锁移出，在隔离的输入快照上计算，
只在版本校验后做短提交。对确实依赖刚改头文件的请求仍要保证依赖就绪或明确返回可重试/取消，不能单纯跳过等待并
伪装成完整结果。优先为编辑和交互请求留出计算资源。

### L2 · P0：模块读取在后台持有同一全局写锁并清空整个队列

**证据：代码链与确定性探针。** 后台索引在
[initialized/mod.rs](../crates/cpp_ls/src/handlers/initialized/mod.rs#L373) 的 `update_session` 闭包内
调用 `read_a_module_unit`，最多调用四次。该方法不是一个短提交：它会在
[session/mod.rs](../crates/cpp_code_analysis/src/session/mod.rs#L7034) 中反复 `advance(256)`，
最多 2,000 轮、直到全局待处理队列为空，再同步 `read_the_unit`。

临时探针创建一个 `import report;` 模块及一个无关的 10,000 声明源文件：一次 `read_a_module_unit` 将待处理
数从 2 降到 0，也索引了无关文件，耗时 435 ms。该探针直接调用分析 API；“它会持有全局写锁”由上述服务器调用点
确认，不能把 435 ms 当成 `import std` 的真实项目测量。

自适应 wave 使用的 `held` 只覆盖 `commit_a_wave`，计时在模块读取**之前**结束，见
[initialized/mod.rs](../crates/cpp_ls/src/handlers/initialized/mod.rs#L380)。因此这个局部计时和 20/30 ms
目标无法约束模块工作；`AnalysisState::update` 的整体超时警告仍可能记录完整持锁时间。

**建议：** 模块按依赖队列分片准备，整程序解析使用已存在的锁外 program-read 路径或等价方案；提交校验版本，
不要在模块任务里顺手清空不相关项目队列。临时规避可使用已有的 `[index] modules = false`，但会失去被禁用模块的
语义信息，不能视为修复，见 [configuration.md](configuration.md#L154)。

### C1 · P0：准备与提交之间没有完整的版本校验

**证据：两个确定性反例。**

- **wave：** 排队旧磁盘文本 `int old_name;`，调用 `prepare_a_wave(1)`；在提交前把磁盘改为
  `int new_name;` 并调用 `session.changed([FileEvent::modified(...)])`；随后
  `commit_a_wave(wave, 1)`。本次探针观察到磁盘是 `new_name`，索引摘要仍是 `old_name`。
  准备和提交之间没有相应内容版本校验，见
  [commit_a_wave](../crates/cpp_code_analysis/src/session/mod.rs#L3279)、
  [index_one](../crates/cpp_code_analysis/src/session/mod.rs#L3923) 和
  [SummaryStore::commit](../crates/cpp_code_analysis/src/index/store.rs#L1128)。
- **program：** 对旧缓冲区 `#define OLD_MACRO 1\nint old_name;` 获取
  `gather_a_unit_read(...).read_the_program()` 的结果；在提交前 `did_change` 为
  `#define NEW_MACRO 1\nint new_name;` 并 `advance(1)`。此时摘要已是新文本，但再调用
  `commit_a_program` 后探针得到的 cooked declarations 为 `["old_name"]`，并且程序被标记为已读，
  不再处于待重新读取状态。见
  [commit_a_program](../crates/cpp_code_analysis/src/session/mod.rs#L3623) 和
  [commit_a_unit_reading](../crates/cpp_code_analysis/src/session/mod.rs#L6636)。

这两种顺序在服务器中是可发生的：准备时持读锁或不持锁，随后编辑/文件事件/工作区重载可以先取得写锁，
最后旧结果再提交。另一个需要检查的边界是
[`commit_the_re_read` 的 cooked 分支](../crates/cpp_code_analysis/src/index/store.rs#L1524)：它先插入
`plan.cooked`，后面的摘要哈希检查并不保护前面的插入。现有 `commit_a_cooked` 的目标文本哈希校验值得复用，
但仅检查目标文本仍不足以覆盖头文件宏环境、编译配置或工作区替换。

**建议：** 所有异步工作统一携带 `workspace_epoch`、目标文档版本/哈希、编译配置和依赖闭包版本；在同一提交
临界区核对后才插入、标记 `worked`/`units_read`。过期结果丢弃并在仍有需求时重新排队，不能因为一次失败提交
永久丢失工作。依赖键必须覆盖 C++ 宏、include 和 module 环境，而非只有文件自身哈希。

### C2 · P1：请求序号边界会被相邻编辑合并穿透

**证据：确定性反例。** 请求分发记录 `inbox.queued()` 并稍后执行 `catch_up_upto`，意图是处理请求之前的
通知。实际在 [UpdateInbox::push](../crates/cpp_ls/src/context/update_queue.rs#L163) 中，新的相邻同文件
`Changed` 只替换旧事件内容，保留旧事件的 `seq`：

```text
change(v1, "before") -> seq=1
request              -> 记录 upto=1
change(v2, "after")  -> 替换队尾内容，队尾 seq 仍为 1
apply_upto(1)        -> 实际应用 "after"
```

临时单元探针得到 `bound=1, text=["after request"]`。现有
[边界测试](../crates/cpp_ls/src/context/update_queue.rs#L422) 使用 `open`/`close`，没有覆盖跨请求合并的
同文件变更。

而且即使阻止上述合并，[后台 worker](../crates/cpp_ls/src/context/update_queue.rs#L290) 也可先以
`u64::MAX` 应用较新通知；`ServerContextSnapshot` 只是共享服务的 `Arc`，不固定分析版本。旧请求最终查询的是
活的 session，不能保证光标位置和查询文本属于同一版本。

**建议：** 请求成为合并屏障，并在处理屏障后绑定不可变文档/分析版本；或者在较新编辑到来时明确取消旧请求并返回
合适的取消/内容已修改结果。只更新队尾序号、只加一把锁，或仅保存请求时的 `upto` 都不能单独解决整个问题。

### C3 · P1：取消不能停止排队/计算，初始化期间的取消还可能丢失

**证据：确定性反例与代码路径。** 请求先运行 `catch_up_upto`，随后 handler 的 `prepare`、`catch_up` 等
可能执行工作，最后才进入 [snapshot_query](../crates/cpp_ls/src/context/query_runner.rs#L39)。
它只在计算前检查 token；`query_blocking` 既不接收 token，也不在等待 permit、锁或计算中协作取消，且返回
`Ready` 时不会再次检查。临时探针在阻塞查询开始后取消 token、再放行计算，得到 `Ready(42)`。
`spawn_blocking` 本身也不会因丢弃 Future 而中断已启动的同步计算。

初始化阶段的请求被放进 `pending_messages`，取消通知却允许立即处理；请求此时尚未在
[cancellations](../crates/cpp_ls/src/context/mod.rs#L74) 注册，取消可能找不到目标，初始化结束后旧请求仍会执行。
见 [message_processor.rs](../crates/cpp_ls/src/server/message_processor.rs#L27)。

**建议：** 在入队、等待锁/permit、准备、计算和发送结果前分别检查取消；同步分析循环增加协作检查点。
取消请求不应取消必须按序应用的文档通知。为初始化中尚未分发的请求保留取消状态，并保证每个请求只收到一次协议响应。
不要声称可以强行杀掉已开始的 `spawn_blocking`。

### C4 · P1：工作区重载的 epoch 交接存在竞态

**证据：代码可行时序，尚未做端到端复现。**
[重载任务](../crates/cpp_ls/src/context/workspace_manager.rs#L190) 先通过 `clear_workspace` 将版本加一，
随后 `start_analysis` 启动新的后台 pump，最后又将同一版本加一。
如果新 pump 恰好在最后一次递增之前读取版本，它的下一轮
[版本检查](../crates/cpp_ls/src/handlers/initialized/mod.rs#L304) 就会认为自己属于旧工作区并退出；
如果读取发生在递增之后则不会。旧 pump 的一次锁外准备也可能跨越 session 替换，C1 的提交校验同样需要覆盖它。

**建议：** 在启动新 pump 之前一次性发布新 epoch，并显式把该 epoch 传给 pump 和所有提交；重载完成不应再无条件
改变新 pump 的身份。增加受控调度测试，分别暂停在新 pump 读取 epoch 前后及旧工作提交之前。

### C5 · P1：诊断“何时刷新”和“没有结果”的语义不完整

**证据：代码行为；具体客户端上的表现取决于其拉取策略。**

- 服务器无条件声明 pull `diagnosticProvider`，但没有 push `publishDiagnostics` 回退。只支持旧式 push 的
  客户端不会因此自动得到诊断，见 [diagnostic/mod.rs](../crates/cpp_ls/src/handlers/diagnostic/mod.rs#L309)
  和 [client.rs](../crates/cpp_ls/src/context/client.rs#L160)。
- 代码声明 `inter_file_dependencies: true`，却有意不发送 `workspace/diagnostic/refresh`。头文件变化或
  后台索引完成时，已显示文件的内容可能没变、诊断却已改变；如果客户端只在本文件编辑时拉取，就可能继续显示旧报告。
  `workspace_diagnostics: false` 仅表示不提供整个工作区报告，**不意味着 document diagnostic 不需要刷新**。
  不应为修复刷新而重新引入无条件循环；现有代码中的循环风险是真实约束。
- [pull handler](../crates/cpp_ls/src/handlers/diagnostic/document_diagnostic.rs#L48) 忽略准备操作的
  `bool`，有 session 但 `diagnose_file` 为 `None` 时将其映射为空报告；没有 session 时统一 runner 则可能
  返回 `null`。这使“未准备好/无法读取”和“文件干净”难以区分，或产生客户端无法消费的报告形状。
- `[diagnostics] on_change_ms` 当前只被配置解析及测试读取，没有用于服务端调度；原配置文档将其描述为实际
  防抖时间是不准确的。pull 请求由客户端发起，不能通过该值降低当前服务端的编辑开销。

**建议：** 明确客户端能力契约：支持 pull 的客户端按版本报告并在依赖结果真正改变时合并、限频刷新；不支持 pull
的客户端若要求兼容，则走有版本校验和防抖的 push，避免对同一客户端混用两条通道。为暂时未就绪、失败和真正空
结果设计不同的可重试语义；若实现 `resultId`，其键必须包含文档、依赖和配置版本。

### L3 · P1：读锁并不等于不阻塞编辑，准备检查也可能做解析

**证据：代码推断，前述编辑实测不能单独量化每一部分。**
[run_blocking](../crates/cpp_ls/src/context/analysis_state.rs#L484) 虽然将计算移到阻塞线程，但闭包执行期间
仍持有 session 读锁；耗时查询会阻止需要写锁的编辑。更隐蔽的是
[`prepare_for_a_diagnostic` 的 `try_read` 快路径](../crates/cpp_ls/src/context/analysis_state.rs#L610)
调用 `session.view(...)`；后者在渲染存在但 view 缓存未命中时仍会
[解析渲染并填缓存](../crates/cpp_code_analysis/src/session/mod.rs#L4833)，并非注释所说的纯查表。
这个检查直接发生在 async handler 执行路径上，而不是 `spawn_blocking` 的查询闭包里。

**建议：** 用不解析、不填缓存的轻量状态检查判断是否就绪；重解析放到受控执行器并基于快照运行。
区分“准备时间、permit 等待、读/写锁等待、实际计算”和“响应序列化”分别观察，不要只按读锁/写锁名称推断延迟。

## 其他架构与可扩展性问题

### S1 · P2：FULL 同步与整文件失效让成本随文档大小增长

服务器广告 [`TextDocumentSyncKind::FULL`](../crates/cpp_ls/src/handlers/text_document/mod.rs#L24)，每个变更携带
全文；处理路径克隆文本到 workspace 状态，overlay 再持有一份，VFS 重新建立 `Arc<str>` 和完整行索引。
`didChange` 的 LSP `version` 没有保存在 workspace 缓冲区状态中，见
[text_document_handler.rs](../crates/cpp_ls/src/handlers/text_document/text_document_handler.rs#L70) 和
[workspace_state.rs](../crates/cpp_ls/src/context/workspace_state.rs#L98)。
FULL 的确简化相邻变更合并，但大文件上造成 O(文件大小) 的传输、复制、索引与解析压力。
本次测量证明了整体路径随该合成文档变慢，**不能将全部差距归因于传输或某一个副本**。

**建议：** 分阶段引入增量同步、共享文本/rope 或 piece table 和增量行索引；逐条按 LSP 变更顺序应用 UTF-16
range 并校验版本。仅把协议改成 incremental、却仍每次全量解析，不会自动解决主要计算成本。
C++ 的宏和 include 可能扩大失效范围，增量解析必须以正确性测试为前提。

### S2 · P2：后台调度、缓存和日志仍有尾延迟风险

- pump 等待整程序阅读的分支使用不可被 `wake()` 打断的
  [`sleep(wait + IDLE_WAIT)`](../crates/cpp_ls/src/handlers/initialized/mod.rs#L814)；刚进入等待时到来的
  编辑可以立即更新自身，但后续后台索引/cook 可能继续等这次睡眠结束。建议以通知和计时器竞争唤醒，并重新检查
  安静时间，避免忙循环。
- “无进展”判断把 `claimed_count > 0` 本身计为进展，即使没有成功构建或提交；失败项目有单独拒绝机制，但该
  watchdog 不能证明反复领取失败一定会被识别。见
  [initialized/mod.rs](../crates/cpp_ls/src/handlers/initialized/mod.rs#L590)。
- 前台阻塞查询许可数按可用 CPU 数设置，索引 wave 自己又最多使用 CPU 数量级的线程，整程序读取还绕开该许可。
  冷索引和多个交互请求叠加时可能过度竞争 CPU；这是代码推断，未在本次实验单独量化。应给后台和交互工作分配
  独立但受总预算约束的配额，并测量吞吐量与交互尾延迟。
- 每次索引插入会清空整个 `visibility_answers`、`type_answers` 和 `definition_answers` 缓存，可能使索引
  持续变化时的查询反复冷算；应按依赖/代数细分失效并用 profile 证明收益，见
  [project.rs](../crates/cpp_code_analysis/src/index/project.rs#L12352)。
- `debug` 日志在应用锁内格式化整个 `UpdateEvent`，其中 FULL `didChange` 含完整源文件；默认 `info` 不会输出
  该行，但诊断卡顿时开启 debug 会增加大文本格式化及日志 I/O，还会把源文本写入日志。建议只记 URI、版本、
  文本字节数和事件数，见 [update_queue.rs](../crates/cpp_ls/src/context/update_queue.rs#L229)。

### S3 · P2：性能预算目前主要是告警，测试不能代表逐次编辑

[`WRITE_BUDGET=50 ms`](../crates/cpp_ls/src/context/analysis_state.rs#L125) 是超时**记录**，不是调度器的硬
上限；`worst_write_hold` 只记录单次持锁，不包括排队在多个写操作、读锁、permit 及请求自身计算上的总时间。
wave 的单次提交检查发生在每个文件之间，一个昂贵文件本身也可以超时。

现有 [latency.rs](../crates/cpp_ls/tests/latency.rs#L54) 是有价值的真实协议回归，但其初始补全预算为
2,500 ms，索引更新预算为 1,000 ms；“稳态”只看最后 3 次，并允许其中 2 次超过 50 ms。它没有模拟
`didChange`，而 debug 构建直接跳过该测试，见 [latency.rs](../crates/cpp_ls/tests/latency.rs#L510)。
当前 [CI 测试命令](../.github/workflows/test.yml#L61) 使用默认 debug profile，所以不能把这些测试通过等同于
编辑 P95 达标。

建议记录按请求类型和文档版本分组的队列长度、`didChange` 应用耗时、permit/锁等待、计算时间、响应字节数、
取消后继续运行的工作及缓存命中/失效，并保留 P50/P95/P99，而不是只保留最大单次写锁时间。

### S4 · P2：工作区和客户端能力的边界需明确

当前只对第一个 workspace root 建立 session，多 root 有警告但其他根不被同等分析；
未找到 `workspace/didChangeWorkspaceFolders` 的分发；无 root 时也不会建立分析 session。见
[workspace_manager.rs](../crates/cpp_ls/src/context/workspace_manager.rs#L66)、
[workspace_state.rs](../crates/cpp_ls/src/context/workspace_state.rs#L54) 和
[notification_handler.rs](../crates/cpp_ls/src/handlers/notification_handler.rs#L59)。
文件监听完全依赖客户端动态注册；客户端不支持时已有警告，但外部文件变化不能自动进入索引，见
[register_file_watch.rs](../crates/cpp_ls/src/handlers/text_document/register_file_watch.rs#L53)。

这可以是第一阶段的产品限制，但需要和“任意 LSP 客户端”“多文件夹/单文件打开”分别明确兼容性；
若补全多 root，应使用按 root/配置隔离的 session 与 epoch，而不是让更多根共享当前单个可变状态。

## 建议实施顺序与验收

1. **先锁定正确性。** 为 C1/C2/C3/C4 加入可控暂停点测试：准备旧 wave/program 后插入编辑、文件监听或
   工作区重载，再放行提交；跨请求连续编辑；请求在初始化、等锁和计算中取消。断言旧产物不能覆盖新版本、旧请求
   要么绑定正确版本要么明确取消，并且需要的更新不能丢失。
2. **拆掉最长的写锁路径。** 先移出模块整队列 drain、编辑的 `advance` 尾部重计算及诊断的同步 cook；明确每项
   只处理目标文件/依赖所需工作，并在提交时验证版本。保留现有头文件变更后立即补全和诊断测试，防止把“更快”
   变成“更快地返回错误结果”。
3. **统一快照与调度契约。** 将缓冲区版本、工作区 epoch、依赖环境、请求取消与产物生命周期作为一套机制，
   而非在每个 handler 里各加一次检查。可参考已有 [locking.md](locking.md)：不可变快照方向合理，但必须计入
   多个在途版本的内存、索引发布成本和 C++ 宏环境依赖；`Arc` 本身不会自动保证零等待或正确失效。
4. **修复诊断与客户端兼容性。** 按能力选择 pull/push，按实际结果变化去重刷新，处理未就绪状态；测试头文件
   编辑、索引由不完整变完整、空诊断清除以及连续刷新不会形成循环。
5. **建立 release 性能门禁。** 在固定硬件或独立性能 runner 上，以至少 100 次采样覆盖小文件、约 170 KB
   高符号密度文件、跨文件编辑、含真实标准库的冷/热索引、持续输入和取消风暴；同时校验答案内容。建议先以
   本文合成场景为基线，逐步以“编辑后补全 P95 < 150 ms、无关文件补全 P95 < 50 ms、单次常规写提交
   P99 < 50 ms”作为**待确认的优化目标**，不是当前保证，也不应直接作为共享 CI 机器上的无条件阈值。

在改造前，排除明确不属于项目的生成目录、按需关闭 module 索引、避免为性能排查长期启用完整文本 debug 日志，
都只能缓解个别负载；不要把它们当作对版本一致性或全局锁问题的修复。
