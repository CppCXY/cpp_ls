# `cpp_ls` 实现审查

审查基线：当前工作区源码，以及 `cpp_ls` 的真实 stdio 协议路径。审查范围是
`crates/cpp_ls` 的 LSP 生命周期、消息调度、document sync、diagnostics、watcher、workspace
reload、client capability、formatter 与 handler wiring；C/C++/CUDA 的语义正确性不在本文重审，
`cpp_code_analysis` 的结论见 [code-analysis-review.md](./code-analysis-review.md)。

本文未修改实现代码。

## 总体评价

`cpp_ls` 是一个已经越过“LSP demo”阶段的工程：分层清楚、并发模型有意识地设计过、协议
转换集中、测试不是只覆盖纯函数，很多行为还被真实 stdio 集成测试固定。

最强的部分：

- request handler 统一返回 `RequestOutcome`，malformed params、cancel、panic、missing 都有
  明确出口；
- `didOpen/didChange/didClose` 通过单 writer queue 保序，request 按 sequence barrier 等待
  它之前的通知；
- 后续文档变更会用 `ContentModified` 取消旧 snapshot 请求，避免旧内容覆盖新问题；
- 分析读写、prepare/commit、blocking executor、写锁预算都有独立建模；
- UTF-16 position、rendered/written span、URI/path 的转换大多集中在边界层；
- diagnostics 同时支持 pull 和 push，版本检查避免旧 push 覆盖新 buffer；
- 有大量 raw-LSP、latency、watch、idle、真实项目 probe。

不足主要不是“功能少”，而是 **第二条状态机**：

- document event 被统一进 queue；
- configuration、workspace reload、client registration、progress、inactive-region push
  仍有各自的生命周期；
- 这些旁路没有全部遵守 document update queue 已建立的 sequence/version 约束。

因此当前适合作为“能力丰富、交互快速的编辑器壳”，但 client/workspace 管理还没有达到
analysis request path 同等的严谨程度。

### 建议评分

| 维度 | 评价 |
| --- | --- |
| Handler / analysis 边界 | 8.5 / 10 |
| Request 并发与取消 | 8.5 / 10 |
| Document sync / UTF-16 / version | 8 / 10 |
| Diagnostics 协议 | 8 / 10 |
| Workspace / client configuration | 6 / 10 |
| File watching / reload | 6.5 / 10 |
| Capability 与实现一致性 | 6.5 / 10 |
| Fault handling / lifecycle | 7 / 10 |
| 集成测试 | 8.5 / 10 |
| 可维护性 | 6.5 / 10 |
| 作为编辑器 LSP server | 适合继续投入 |
| 作为通用、严格 LSP 实现 | 还需收口 |

LS 自身的整体实现优于很多早期 C++ language server，但最终语义准确度仍受
`cpp_code_analysis` 的 scope/type/overload 能力上限约束。

## P1：会直接造成陈旧状态或错误编辑器行为

### P1-1：workspace configuration 的初次请求没有 workspace scope

[`initialized_handler`](../crates/cpp_ls/src/handlers/initialized/mod.rs#L57) 先调用
`get_client_config`，然后才把 `roots` 写入 `WorkspaceManager`。

[`get_client_config_default`](../crates/cpp_ls/src/handlers/initialized/client_config/default_config.rs#L38)
从 manager 取 root 作为 `scopeUri`。此时 manager 还没有 root，所以请求实际上是：

```json
{
  "method": "workspace/configuration",
  "params": {
    "items": [{ "section": "cppls" }]
  }
}
```

本轮 raw-LSP probe 已复现：客户端已经发送了带 `rootUri` 的 `initialize`，并发送了
`initialized`，服务端随后发出的 `workspace/configuration` 仍没有 `scopeUri`。

影响：

- 单 folder 下可能误读 global config，而不是该 folder 的 override；
- multi-folder 时更无法决定当前 project 的 configuration；
- `files.exclude` 又是另外一次 deliberately unscoped request，`cppls` 和 `files` 的作用域
  容易不一致；
- 首次启动读到的配置可能与后续 `didChangeConfiguration` reload 读到的不一致。

建议先安装 roots/client identity/config path，再请求配置，最后才 open Session。读取 client
configuration 不需要 Session 已存在。

### P1-2：配置文件变更没有统一走 request 的 sequence barrier

Document notifications 被集中到 `UpdateInbox`，而
[`workspace/didChangeConfiguration`](../crates/cpp_ls/src/handlers/notification_handler.rs#L63)
走 `tokio::spawn`：

```text
didChangeConfiguration
    -> detached task
    -> workspace/configuration
    -> replace client config
    -> schedule reload

same client's next request
    -> record only UpdateInbox.queued
    -> run immediately against old configuration/session
```

`record_request` 只看 update queue。配置变更没有 queue sequence，也不会触发
`started_change` 对 in-flight request 发 `ContentModified`。

因此以下顺序没有顺序保证：

```text
client: didChangeConfiguration
client: completion/definition/diagnostic
```

后一个请求可能基于旧 `exclude`、旧 Session 或旧 compile environment 回答。

建议给 `UpdateEvent` 增加 configuration variant，至少将：

1. 读取 client config；
2. 更新 manager；
3. invalidating/reload generation

作为一个可追踪 sequence。request barrier 和 ContentModified 应对所有会改变分析 snapshot
的 event 生效，而不只 document event。

### P1-3：`inactiveRegions` 的 version 和 spans 不是同一个 snapshot

[`inactive_regions::publish_all`](../crates/cpp_ls/src/handlers/inactive_regions/mod.rs#L34)
先从 `WorkspaceState` 取 `(uri, version)`，随后分别从 `AnalysisState` 取
`inactive_regions` 和 text，最后发送携带旧 version 的 notification。

而 `didChange` 的真实写入顺序是：

1. 更新 `WorkspaceState` 的 text/version；
2. 释放 manager lock；
3. `AnalysisState::update_session` 更新 session。

见 [`process_did_change_text_document`](../crates/cpp_ls/src/handlers/text_document/text_document_handler.rs#L92)。

这留下一个可交错窗口：

```text
workspace manager 已是 v2，session 仍是 v1
inactive publish 读取 v2
inactive publish 读取 v1 spans
client 收到 textDocument(v2) + v1 ranges
```

反方向交错也会发生：先保存 v1，再在发送前应用 v2，client 会把带 v1 的消息丢掉而保持旧
decoration。

push diagnostics 已经有正确做法：
[`DiagnosticService::finish`](../crates/cpp_ls/src/context/diagnostics.rs#L82) 同时检查
document version 和 `UpdateInbox::when_caught_up`。inactive-region 应使用同样的发布门，或者把
`(version, text, spans)` 做成一个可验证的 immutable snapshot。

### P1-4：打开中的配置文件不会触发 reload

[`process_did_change_watched_files`](../crates/cpp_ls/src/handlers/text_document/watched_file_handler.rs#L100)
先判断 URI 是否是 open buffer；对 `CREATED/CHANGED` 的 open file 直接 `continue`。

`continue` 发生在 configuration-file check 之前。因此当用户在编辑器中保存：

```text
.cppls.toml
compile_commands.json
CMakeCache.txt
```

客户端同时发送 `didSave` 和 watched-file change 时，事件会因为文件 open 被跳过，后面的
`add_update_config_task` 永远不执行。`didSave` 的 text 本来被 server 配置为不包含，因此 save
handler 也没有第二条触发路径。

结果是配置文件保存后，language server 仍使用旧 compile configuration，直到关闭文件、下一次
外部写事件或手动重启。

建议：

- 在 open-buffer skip 之前识别 configuration file；
- open 的普通源码仍以 buffer 为 authority；
- open 的 configuration file 在 `didSave`/watched change 后做一次合适的 reload/debounce。

### P1-5：`--config` 的 explicit path 没进入 watcher filter

`--config` 被保存进 `WorkspaceState::config_file`，Session open 时也会使用，但
[`WorkspaceState::watch_filter`](../crates/cpp_ls/src/context/workspace_state.rs#L277)
只创建 `WatchFilter::new(root)`，没有对 explicit config path 调用
`with_configuration`。

同时动态 watch registration 只声明固定 glob：

```text
**/.cppls.toml
**/compile_commands.json
**/CMakeCache.txt
```

因此自定义配置：

```text
cpp_ls --config D:\shared\project-cppls.toml
```

可能在打开时生效，却不会被 native/dynamic watcher 覆盖，也不会通过
`is_configuration_file` 被识别。这个 CLI 参数存在，但它只在启动和整个 project reload 时
生效，不能形成可持续的 live configuration。

建议让 `WorkspaceState` / `WatchFilter` 保存可被 watch 的配置路径集合，而不是把 project
config 和 compile database 共用成一个单一路径；同时为 explicit file 增加 watcher
registration。若不支持自动重载，应明确显示 “restart required”，而不是静默假装 auto
reload。

### P1-6：dynamic watcher 对 CUDA source 存在明确漏项

客户端动态 watch 的固定列表位于
[`register_file_watch.rs`](../crates/cpp_ls/src/handlers/text_document/register_file_watch.rs#L35)：

- 含 C/C++、module interface；
- 不含 `.cu`、`.cuh`。

analysis layer 在 CUDA project 中明确会把二者加入扫描范围
([`session/lifecycle.rs`](../crates/cpp_code_analysis/src/session/lifecycle.rs#L70))。
这违反 watcher 文件里“同一套 extension list”的注释。

支持 dynamic watcher 的客户端会使用客户端 watcher，而不是 native watcher。此时关闭
的 `.cu` / `.cuh` 被外部修改、创建或删除后，Session 可能长期保留旧 summary/include
resolution。

建议从 analysis 的 source-extension authority 生成 watcher glob；至少当前固定 glob 必须
加入 `cu,cuh`。

### P1-7：idle/progress invariant 当前是红的

release 全量运行时，已有 `idle::an_idle_session_sends_the_client_nothing` 稳定失败：

```text
an idle session sent 2 message(s) in 5s:
["$/progress", "workspace/diagnostic/refresh"]
```

单独重跑同样失败。服务器 log 显示 initial indexing 已完成并发出 program reading，随后又在
空闲窗口中 cook/refresh。

还有一个明确的 progress 生命周期问题：

1. first pass 调用 `finish_progress_task(LoadWorkspace)`；
2. background pump 后续仍可能发现 `pending_work`；
3. [`index_in_background`](../crates/cpp_ls/src/handlers/initialized/mod.rs#L928) 对同一 token
   发送 progress report，但不会重新 begin。

这不是性能数字问题，而是 token lifecycle 和“workspace 已 indexed”承诺不一致：

- client 可能收到已经结束 token 的 report；
- status bar 可能被错误恢复或显示空载消息；
- refresh 请求说明 idle 后仍发生可见 server→client 流量；
- 测试明确把这种消息当作回归。

建议定义 progress 的对象：

- token A 只覆盖启动索引，end 后绝不再 report；
- 后续 cook/program read 如果要显示，使用独立 token；
- “settled/idle” 必须有稳定判定，refresh 只在实际 revision 变化时发。

## P2：近期应修

### P2-1：`willSaveWaitUntil` 有 handler，没有 capability

request dispatcher 注册了
[`WillSaveWaitUntil`](../crates/cpp_ls/src/handlers/request_handler.rs#L176)，formatter 模块也
实现了 handler，但
[`TextDocumentSyncOptions`](../crates/cpp_ls/src/handlers/text_document/mod.rs#L26) 写成：

```rust
will_save: None,
will_save_wait_until: None,
```

raw-LSP capability 输出同样没有 `willSaveWaitUntil`。所以正常 client 永远不会走这条实现。
源码中的 “The save-time request is advertised” 注释已经与 wire capability 漂移。

同时 formatting handler 只使用 URI/range，忽略 LSP `FormattingOptions` 的 tab size、
insert spaces 等字段。项目 formatter config 可以覆盖，但 capability 应明确约束。

建议二选一：

- 有意支持：advertise `willSaveWaitUntil`，增加 end-to-end format-on-save test；
- 没打算支持：删除 handler/dispatch/comment，避免死能力。

### P2-2：多个 CLI 选项是 no-op，或只作用于半个系统

[`cmd_args.rs`](../crates/cpp_ls/src/cmd_args.rs#L33) 定义了：

- `--load-stdlib`；
- `--resources-path`；
- `--editor`。

当前源码中：

- `load_stdlib` 除声明外没有消费者；
- `resources_path` 除声明外没有消费者；
- `--editor` 只改变初始化时 client config 的 `ClientId`，但
  [`LspServer::new`](../crates/cpp_ls/src/server/lsp_server.rs#L39) 创建 `LspFeatures` 时仍
  使用真实 `clientInfo`。

这使 `--editor vscode` 在 VS Code-style config 和 semantic token / code lens command 选择
之间产生两个不同身份。

建议：

1. 在 `run_ls/main_loop` 入口统一 resolve effective client id；
2. `LspFeatures`、client config、code lens 使用同一个值；
3. `--load-stdlib` 连接 Session/Project config，或从 CLI/README 删除；
4. `--resources-path` 同样连接 logger/resources，或删除。

### P2-3：dynamic registration 和 progress create 不检查 client response

[`ClientProxy`](../crates/cpp_ls/src/context/client.rs#L76) 的
`client/registerCapability`、`client/unregisterCapability` 和 progress create 都走
`send_request_no_wait`。response 会被 `on_response` 丢弃，因为没有注册 waiter。

影响：

- client 说支持 dynamic registration，但实际拒绝 watcher registration 时，server 也不会启动
  native watcher fallback；
- watcher 可以静默从 “会收到 change event” 变成 “没有任何事件源”；
- progress `begin/report` 发在 `window/workDoneProgress/create` 的 result 到达之前；
- request id、registration error 没有日志。

refresh 请求可以 fire-and-forget；registration/progress token creation 不应采用同一种策略。
建议至少记录 `MethodNotFound`/error response，并在 watcher registration 失败时退回 native
watcher。

### P2-4：LSP shutdown/exit 是明显的非标准策略

[`handle_shutdown`](../crates/cpp_ls/src/server/connection.rs#L69) 回答 `shutdown` 后只等待
500ms 的 `exit`，超时仍返回成功并结束。

正常通知里的 `exit` 没有独立 handler；客户端先发 `exit`、没有发 `shutdown` 时会走
“Unhandled notification”，server 继续运行。EOF 路径最终也返回成功 exit code。

这个行为可能是为了避免坏客户端导致进程泄漏，测试也明确要求“只发 shutdown 也结束”。
它需要在文档里声明为 deliberate deviation，否则使用标准进程生命周期判断的 client wrapper
会把它误读为完整 LSP 合规。

建议至少：

- 收到 `exit` 时立即停止；
- 按 LSP exit code 区分 had-shutdown；
- 对 `shutdown` 后的其他消息明确拒绝；
- 把 500ms auto-exit 写进产品兼容性说明。

### P2-5：multi-root 是混合状态，不是单纯“只分析第一个”

[`WorkspaceManager::set_roots`](../crates/cpp_ls/src/context/workspace_manager.rs#L70) 只对
日志 warning，Session 只开第一个 root；但 `WorkspaceState::is_workspace_file/contains` 接受
所有 root，reload 时也会把所有 root 的 open buffers replay 给第一个 Session。

结果是：

- 第一个 root 参与项目 scan/config/cache；
- 其他 root 的 open buffers 可能参与 query/diagnostic；
- native watcher 只看第一个 root；
- dynamic glob 可能收到其他 root 的事件；
- `workspace/didChangeWorkspaceFolders` 没有处理，server 也没有广告
  workspace folder capability。

当前更适合明确称为 “single-root; additional folders are not analysed”。否则应统一 per-root
Session 和 shared client event routing。

### P2-6：malformed notification 和 panic 容错仍偏静默

request 的 `extract` 失败会返回 `InvalidParams`；notification dispatcher 的各个
`if let Ok(...)` 失败时什么都不发、也不 log。document notification 一旦丢失，后续 request
仍会按正常 sequence 看旧 buffer。

notification handler 虽然通过 `catch_unwind` 保住 main loop，但丢弃了错误；配置变更独立
spawn 的 panic 也没有统一记录。

建议至少：

- invalid didOpen/didChange/didSave/didClose 都在 log 中带 URI/method；
- panic 带 method 和 request/notification id（若有）输出；
- 对 stateful notification parse failure 发 `window/logMessage` 或进入 degraded state；
- `$/setTrace` 当前是纯 no-op，应明确记录为 unsupported，不能被悄悄吞掉。

### P2-7：部分 project result 没有统一的“不完整”出口

completion 有 `isIncomplete`，semantic token/inlay hint 有 refresh，code lens 也会 refresh。
references/workspace symbol 没有这类协议字段：index 未读完时可以返回当前看到的列表，只在
log 中记录 partial。

这个行为可理解，但产品语义应明确：

- UI 里 “没有更多 references” 不是 partial state；
- rename 必须继续保持完整性 gate；
- Find References / workspace symbol 不应被描述成完整 project closure；
- 可以考虑在 partial 时返回 `null`/error，或通过 client-specific notification 提示。

## 明确的能力边界

1. **单 workspace root。** 额外 folder 不是等价 Session。
2. **没有 semantic token range/delta provider。** 只实现 full token。
3. **只有 full document diagnostics。** workspace diagnostics 未实现，`resultId` 不使用。
4. **只在 client 支持 refresh 时修复冷启动不完整回答。** 其他 client 只能靠文档变化或手动
   再请求。
5. **reference/workspace symbol 可能是当前 index 的 partial result。**
6. **formatting 的最终效果受配置 formatter/external program 影响，LSP FormattingOptions**
   当前没有统一映射。
7. **`inactiveRegions` 是 clangd extension，不是标准 LSP。**
8. **LS 不增加 compiler-grade semantics。** parser/code-analysis 的 `Unknown`、scope、
   overload、type 限制仍会原样进入 LSP 响应。

## 值得保留的实现

1. **UpdateInbox + request barrier。** 这是这套 LS 最值得保留的核心，直接解决
   `didChange` 后 completion 吃旧版本的问题。
2. **`ContentModified` 取消。** 长请求不会用旧 version 回答新 buffer。
3. **`ServerContext::task` 统一 response/error/cancel/panic。**
4. **prepare / read / commit 分离。** parse/render 大量不在 writer 下完成，交互延迟有真实
   架构保障。
5. **analysis blocking semaphore 和 write budget instrumentation。** 性能报告可以定位
   `permit / gate / session / compute`。
6. **UTF-16 position 与 render/written map 单独模块化。**
7. **pull diagnostics + versioned push fallback。** protocol 不是两套互相覆盖的 diagnostics。
8. **stdio 端到端测试覆盖 handshake、cancel、watch、progress、idle 和真实 feature result。**

## 可维护性评价

当前优点是“每个新 feature 有对应 handler module”，缺点是：

- `initialized::index_in_background` 已经集中了 indexing、cook、unit read、progress、refresh、
  diagnostics、cache save，不再只是 initialized；
- `handshake.rs`、`hover/mod.rs`、`analysis_state.rs` 很大，历史测量写在实现注释里，维护时
  容易把当前 contract 和旧实验混在一起；
- request/capability table 仍是人工同步，已经出现 willSaveWaitUntil 漂移；
- watcher extension list 与 analysis extension list 重复，已经出现 CUDA 漏项；
- `ExitError`、`supports_workspace_diagnostic` 等 dead/partial API 会降低“看见代码就相信
  已被连接”的可信度。

建议把高成本历史结论留在简短注释和外部 benchmark/report，源码只保留当前 invariant。

## 建议推进顺序

### 第一阶段：收口 workspace/reload correctness

1. roots 先于 `workspace/configuration`；
2. explicit `--config` 进入 watcher/filter；
3. open configuration save 触发 reload；
4. didChangeConfiguration 纳入 sequence；
5. CUDA extensions 统一来源。

### 第二阶段：固定 push/progress snapshot

1. inactive-region 发布使用 version + caught-up guard；
2. progress token 增加 state，end 后禁止 report；
3. 修复 idle test 的 late refresh/cook 生命周期；
4. dynamic registration failure 有 fallback。

### 第三阶段：清理 capability / CLI wiring

1. willSaveWaitUntil advertise 或删除；
2. `--load-stdlib` / `resources-path` 实现或移除；
3. effective ClientId 只保留一个；
4. `exit` 语义和 deviation 写入文档；
5. single-root 限制由客户端能力与 UI 说明共同表达。

### 第四阶段：协议回归矩阵

建议最少新增：

- 初始化 request 的 `scopeUri`；
- `.cppls.toml` open -> edit -> save -> reload；
- explicit config file outside workspace changed；
- CUDA file dynamic watcher event；
- didChangeConfiguration + next request；
- didChange + inactive-region publication interleaving；
- dynamic registration error fallback；
- progress End 后不再 Report；
- exit before/after shutdown；
- `--editor` 对 code lens、semantic token、config 的一致性。

## 测试与验证记录

### Unit

```text
cargo test -p cpp_ls --lib
196 passed, 0 failed, 1 ignored
```

### Release integration

```text
cargo test --release -p cpp_ls --tests
104 passed, 1 failed, 10 ignored
```

唯一失败：

```text
idle::an_idle_session_sends_the_client_nothing
```

它单独重跑仍失败，错误是 idle 5 秒内收到 `$/progress` 和
`workspace/diagnostic/refresh`。

### Raw protocol probe

最小 stdio probe 验证：

1. server 在 `initialized` 后发起 `workspace/configuration`；
2. request item 只有 `"section": "cppls"`，缺少 `scopeUri`；
3. initialize capability 没有 `willSaveWaitUntil`；
4. request dispatch 中确实已经存在该 handler。

这说明 LS 的 feature 数量较多，但 capability/handler/state wiring 尚未形成一个统一的
contract test。

