# 架构审核

审核对象：`cpp_parser` / `cpp_code_analysis` / `cpp_ls` 三个 crate（2026-10-21，HEAD `ac9532b`）。
本文只写**结构性的判断**，每条都带证据（文件、机制、实测数字）。数字要么是仓库里已有的测量，要么是这次审核重新量的。

---

## 1. 形状

```text
cpp_parser           87 文件  50 583 行     rowan CST、lexer、grammar（无 IO、无并发）
cpp_code_analysis   190 文件 120 241 行     VFS + 摘要索引 + 预处理/烹饪 + 语义 + 检查
cpp_ls               67 文件  18 586 行     LSP 协议层：AnalysisState、handler、能力表
                                              1 907 个 #[test]，0 个 todo!/unimplemented!，4 处 unsafe
```

请求的路径是清楚的：

```text
JSON-RPC ─▶ handler ─▶ AnalysisState<RwLock<Option<Session>>>
                        ├─ 读锁：查询（hover / completion / definition / tokens …）
                        └─ 写锁：通知 worker、索引泵（pump）、"准备"调用
                     Session { VFS, SummaryStore, ProjectIndex, queue, cooking, units, renderings }
                        └─ FileView { rendering + spans + 两套坐标 } ─▶ 查询
```

**两个边界**是这个项目最重要的设计，也是所有难点的来源：

1. **文件坐标 vs 渲染坐标**：`FileOffset`（用户写的文本）与 `ReadingOffset`（宏展开/去指令后的文本），由 `RenderedCooked.spans` 映射。
   诊断、语义着色、补全都必须在这两个坐标系之间正确往返。
2. **写侧 vs 读侧**：泵（`handlers/initialized`）持有写锁做索引切片；查询持读锁。整个 `analysis_state.rs` 的 100 行文档
   就是在讲这个边界怎么不破。

## 2. 并发模型：核心资产是"写预算"

```text
WRITE_BUDGET = 50 ms           任何 update 超过它就 log::warn 并带上 label
worst_write_hold()             最坏一次写持有的毫秒数，测试直接断言
切片的粒度                     INDEX_SLICE / COOK_A_SLICE / PREPARE_INDEX_SLICE / 自适应 wave
```

这是**少见的、可被测试强制执行**的性能约定：`worst_write_hold` 超过预算就是一个可断言的失败，而不是"感觉慢"。
这一轮实测它确实抓到了问题（`prepare a file a pull request named` held 1 815 ms），修完是 86 ms。

锁的数量说明复杂度：

```text
cpp_code_analysis  25 处 Mutex/RwLock      cpp_ls  14 处
```

## 3. 问题（按严重度）

### P1-1 通知与请求之间没有顺序屏障（协议级缺陷）

协议要求：客户端发完 `textDocument/didChange` 之后再发的请求，必须看到新文本。当前的实现做不到：

```text
on_did_change_text_document      只 enqueue                （handlers/text_document/text_document_handler.rs）
process_did_change_text_document worker 里 sync_open_file + apply_buffer
请求                             直接进 RwLock，**不经过该队列**
```

证据：`handshake::a_completion_sees_a_change_to_an_included_header` 断言"改完头文件立刻问一次补全，必须看到新增成员"。
**单独跑 3/3 通过，整套跑 3/3 失败**（这次审核量的）。机器有负载时窗口变大，请求跑在 worker 之前，读到旧 buffer。

这不是测试问题，是**编辑器里真实会发生的**：快速打字后立刻补全 → 成员是旧的。之前报告的"补全缺失/陈旧"很可能有一部分就是它。

### P1-2 "查询不写"只是一半的不变量

`analysis_state.rs` 的文档把"请求不该拿写锁"当成设计目标，但当前有四个请求路径会写：

```text
AnalysisState::prepare             有工作时拿写锁
AnalysisState::catch_up            有编辑时拿写锁（一次 parse）
handlers::read_the_modules         读模块（本轮之前是"读"，现在是"只排队"，但仍拿写锁加文件）
AnalysisState::prepare_for_a_diagnostic   advance(PREPARE_INDEX_SLICE) + cook
```

你上一轮拒绝"诊断里读模块"是对的，那是这条不变量被破坏的极端情况。**结构上的正确形态是：查询只读快照；所有读取新文件的工作
都交给泵**，查询对"还没有"的回答是 `Missing`/`isIncomplete`，不是自己去做。

### P1-3 检查层是唯一无界、无计时、且能在查询里做重活的阶段

诊断拉取在 `std_module_demo.cpp`（`import std;`）上稳定 **1.24–1.27 s**，用仓库自带的 `CPPLS_TRACE_DIAGNOSTICS=1` 切开：

```text
[diagnostics] the file's tree (view)        0 ms
[diagnostics] module notes                  0 ms
[diagnostics] diagnostics                1242 ms   ← 整个 Session::diagnostics
```

`session/mod.rs:3241 diagnostics()` 里计时的只有 view 和 notes；剩下的 1.24 s 全在 `checks_about` 的
`crate::sema::check::run(...)`（22 条规则，默认全开，没有按规则计时、没有预算、没有取消）。

同一份代码在分析层用探针量却是 **1.0–1.6 ms** —— 说明它与"服务器当时的会话状态"有关（可见集 = 327 个文件、
`import std` 已解析），而不是纯粹的规则开销。**这正是需要按规则计时才能定位的形态**。

### P2-4 `renderings` 缓存没有上限

```rust
renderings: Mutex<HashMap<(PathBuf, u64), Arc<RenderedCooked>>>
```

键是 `(路径, 内容哈希)`，**每次编辑一条新条目**，从不淘汰。其它缓存都有上限（`MAX_UNITS = 16`、
`MAX_HEADERS = 12 000`、`MAX_PROJECT_FILES = 10 000`），只有它没有。一个大 TU 的 `RenderedCooked` 是几十到几百 KB，
连续编辑一小时就是在涨内存。

### P2-5 `Session` 是上帝对象

```text
session/mod.rs        6 261 行，111 个 pub fn
index/project.rs     12 999 行（其中约 3 700 行是测试），57 个 pub fn
```

分层靠约定而不是类型：VFS 读写、摘要存储、索引查询、队列、烹饪、模块解析、诊断、语义模型全在一个 `Session` 上。
这解释了为什么"把某件事从查询挪到泵"每次都要在 6 千行里找 5 个入口（这几轮我就在反复做这件事）。

### P2-6 模块：命名约定 + 编译器特例

`resolve_module` 靠 `INTERFACE_UNIT_EXTENSIONS` 的命名约定，加上两个**硬编码的编译器布局**：

```text
libstdc++  <c++ include>/bits/std.cc, bits/std.compat.cc
MSVC       <VC>/Tools/MSVC/<ver>/modules/std.ixx
```

能用，但这是"每个工具链一个 if"。另外 `import` 与宏的语义有缺口：标准规定**宏不跨模块边界**，
而当前 unit 的构造是否会把 `bits/stdc++.h` 的宏表装进导入者的 unit，还没有被验证（上一轮想量没量成）。

### P3-7 文档与代码漂移

`PREPARE_INDEX_SLICE` 的注释写着"Five hundred files…"，而上一轮把它改成 24 —— 注释与常量矛盾，已在本轮修正
（并把 500 → 24 的实测数字写进注释）。这类"注释里的数字比代码更可信"是这个仓库的常态，漂移是必然的，需要定期对账。

### P3-8 测试规模大，但有一类用例依赖墙钟与顺序

1 907 个测试是资产。但 `handshake` 用的是真实子进程 + 300 ms 级 deadline，且有 P1-1 那种顺序依赖，
在并行跑整套时会红。建议：`#[test]` 并行度与 deadline 分离成两个维度（`CPPLS_TEST_TIMEOUT`），
或者把"顺序"从"时间"里拆出来（P1-1 修好后，这类用例就不再依赖墙钟）。

## 4. 该保留的强项

```text
· 降级而不撒谎：Missing / Unknown / NotDeclaredHere 是有语义的答案，不是空列表
· 每个数字都有出处：docs/*.md 里几乎每条结论都带实测与"怎么量的"
· 边界被反复写下来：cook / renderings / units 的注释解释了为什么是这个形状，而不是只写"做了什么"
· 0 todo!，4 处 unsafe（都在测试里），错误处理统一 unwrap_or_else(|poisoned| …)
· 写预算 + 切片的组合，是这套设计里最值得保留的机制
```

## 5. 建议顺序

```text
P1-1  给通知一个递增序号，请求在取快照前等到"已应用 >= 收到时的序号"；或把 buffer 同步移到 on_* 同步路径
      （前者不动索引，代价是一次原子读 + 必要时等待；后者风险小但把 parse 留在通知线程）
P1-3  给 sema::check 加按规则计时（复用 DiagTrace），先拿到"哪条规则 1.2 s"，再决定是加预算还是改写那条规则
P1-2  把 prepare_for_a_diagnostic 的 advance+cook 去掉：答 false，让泵做；诊断只读快照
P2-4  renderings 改成"每个路径只留最新一个内容哈希"
P2-5  不动类型，按"查询 / 索引 / 预处理 / 语义"拆 session/mod.rs 的 impl 块，先把 111 个 pub fn 变成四张可读的清单
P2-6  给 unit 加一条断言/测试：导入者的宏表不包含被导入模块的宏（对 `import std` 用 test_module 量）
```

---

审核同时修正了一处：`PREPARE_INDEX_SLICE` 的注释（500 → 24，附实测对比）。
