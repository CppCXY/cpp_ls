# 内存与全局状态审核：5 GB 从哪来，以及启动索引要怎么重构

**结论先行。**

```text
1. **没有跨会话的泄漏。** 丢掉 Session 后 RSS 从 971 MB 回到 26–42 MB；紧接着开第二个 Session 是 30 MB。
   所以"一直不释放"不是问题所在。
2. **真正的病是四张没有字节上限的表 + 分配器不还页。** 冷启动后活着的表合计约 427 MB，
   而 RSS 是 971 MB——差的 544 MB 是临时工作集（解析树、渲染流、闭包文本）被 mimalloc 留在堆里，
   进程的水位只会涨不会落（实测每一步 `rss == peak`）。
3. **最危险的一张是 `walked_by_content`**：按闭包内容寻址的时间线去重表，**无上限、无淘汰**。
   实测 1 273 个文件的冷启动后它有 **384 条 / 222 MB**；2110 个源文件的项目按 0.58 MB/条推算就是
   **1.2 GB**，而编辑会不断产生新的内容 → 新条目。这是"一天下来 5 GB"的第一贡献者。
4. **第二张是单元表**：`MAX_UNITS = 16` 是按**条数**限的，但一条单元实测 **6.2 MB**（头文件重的闭包），
   16 条就是 99 MB；它没有按字节的预算，闭包变重时会悄悄翻倍。
5. **还有一批无界表**：`definitions`（34k 条 / 11.6 MB，键里每条都带一份 PathBuf）、`surfaces`、
   `effects`（都按内容哈希键，一次编辑一条）、`vfs`（全量文本 + 行索引，无淘汰）。
6. **全局状态**：1 个 `thread_local!`（`walk_census` 的投影缓存）+ 3 个进程级缓存
   （`walk_cache::CONTRIBUTIONS`、`include::fuzzy::CACHE`、`cache_db::HANDLES`）。它们不属于任何 Session，
   生命周期与"一个会话一个所有者"的设计冲突，也让内存报告无法把它们归账。
7. **方案分五段**，每段都有可独立验收的数字：M0 仪器（已完成）、M1 给四张表加字节预算、M2 去全局化、
   M3 摘要磁盘优先（内存里只留窗口）、M4 启动索引流式化（一次性重构）、M5 空闲还页 + 遥测。
   验收线：本语料冷启动峰值 **≤ 400 MB**（现在 971），稳态 **≤ 250 MB**，冷启动时间不劣于现在（≤ 10.5 s）。
```

---

## 一、测量方法与仪器

没有仪器就没有这次审核：`Session::cache_sizes()` 只回答"几条"，而一条 `#define` 和一条渲染差六个数量级。

新增（本次提交）：

```text
· `crate::memory::MemoryReport`：每张表的字节估算，一个 Display 打成一行
· `Session::memory_report()`：把各表的 accounting 汇总
· `Vfs::accounting()`、`ProjectIndex::accounting()`、`MacroDefinitions::accounting()`、
  `TranslationUnit::heap_bytes()`、`LineIndex::heap_bytes()`、`walk_cache::accounting()`
· `examples/memory_report.rs`：按阶段打印 **RSS / 峰值 RSS / MemoryReport**，并做两次"编辑同一批文件"
  的对照实验、drop 后复测、第二个 Session 复测
```

估算方法写在 `memory.rs` 的模块文档里：`size_of` + 结构自己拥有的堆字符串长度；**不算** HashMap 的空闲容量与
分配器开销，所以它是**下界**。下界与 RSS 的差就是"分配器留下的"部分，这个差本身就是数据。

## 二、实测（EmmyLuaCodeStyle，1 273 个文件，冷缓存）

```text
阶段                        RSS      峰值      活着的表（估算）
opened                      7.5 MB   7.6 MB    0
indexed 1273 files          339.7    349.7     138.8 MB
  vfs 1273 files            20.4 MB 文本 + 4.5 MB 行索引
  summaries 1273            59.1 MB（46 KB/文件）
  cooked 64                 2.9 MB
  definitions 31k           10.5 MB
  units 16                  14.7 MB
  walked_by_content 106     20.3 MB        ← 无界
  walk cache 512（全局）      8.0 MB
tail drained (63 slices)    971.3    974.5     427.4 MB
  units 16                  99.5 MB         ← 6.2 MB/条，按条数限
  walked_by_content 384     221.8 MB        ← 无界，+200 MB 就在这一段
  definitions 34k           11.6 MB
  summaries / vfs / cooked / walk cache 不变
之后编辑 24 个文件、每文件一轮    1 049.9  → 每条新闭包 +0.6 MB，**从不回落**（rss == peak）
drop 掉 Session             42.3 MB          ← 全部还回去了：不是泄漏
第二个 Session              30.5 MB          ← 进程级表也没有跨会话增长
```

四个结论：

```text
① 活着的表只有 427 MB，RSS 是 971 MB：**544 MB 是分配器水位**。它不回落，所以用户看到的是 5 GB 而不是 1 GB。
② `walked_by_content` 在尾巴这一段从 106 条涨到 384 条（+200 MB），是单次冷启动里最大的一笔增量。
③ 单元表按条数限：16 条 × 6.2 MB。闭包更重的项目里这一项会自己变成 400 MB 量级而"条数没变"。
④ 编辑不回落，但同一批文件第二轮不涨：增长来自**新的内容哈希**（每次编辑产生新闭包、新 surface、新 effect），
   不是每次编辑都泄漏一份同样的东西。
```

**5 GB 是怎么来的（按表外推）**：

```text
10 000 个文件的工程：
  summaries     46 KB/文件            ≈ 460 MB
  walked_by_content  2 110 个 root × 0.58 MB ≈ 1.2 GB   （2110 是仓库文档里实测过的 root 数）
  units         16 × 6.2 MB          ≈ 100 MB
  vfs           文本量线性            ≈ 200 MB
  definitions   ≈ 9 KB/文件           ≈ 90 MB
  临时工作集 × 分配器水位              ≈ 1–2 GB
  ─────────────────────────────────────────────
  合计                                  3–4 GB，编辑一小时就到 5 GB
```

## 三、逐项审核

### 3.1 无界表（按危害排序）

| # | 表 | 现在的界 | 键 | 实测 | 后果 |
|---|---|---|---|---|---|
| F1 | `Session::walked_by_content` | **无** | `(内容哈希, 闭包哈希)` | 384 条 / 222 MB | 冷启动与编辑都线性涨；5 GB 的第一贡献者 |
| F2 | `UnitTable`（`units`） | 16 **条** | 路径/内容 | 16 条 / 99 MB，单条 6.2 MB | 条数不变而内存翻倍，预算形同虚设 |
| F3 | `MacroDefinitions::parsed` | **无** | `(PathBuf, offset)` | 34k 条 / 11.6 MB | 键里每条一份 PathBuf；随项目规模线性 |
| F4 | `Vfs` | **无** | 路径 | 1 273 文件 / 25 MB | 读过的每个头文件都留着文本 + 行索引；大工程 = GB |
| F5 | `effect::Surfaces::held` | **无** | 内容哈希 | 未计入 | **一次编辑一条**；长会话按编辑次数涨 |
| F6 | `Session::effects` | **无** | effect 哈希 | 未计入 | 同上 |
| F7 | `ProjectIndex::summaries` | 按工程 | 路径 | 59 MB / 1 273 | 不是泄漏，但"全部常驻"是设计选择（见 §四） |
| F8 | `include::fuzzy::CACHE` | **无**（全局） | 根路径 | 未计入 | 进程级，跨会话不释放；每个根一份模糊索引 |
| F9 | `walk_cache::CONTRIBUTIONS` | 512 条（全局） | 文件+内容 | 8 MB | 进程级；`clear()` 只在 revision 变时调用 |

### 3.2 全局状态

```text
T1 crates/cpp_code_analysis/src/index/walk_census.rs:58  thread_local! {
       static PROJECTIONS: RefCell<HashMap<PathBuf, (u64, Arc<Vec<(Box<str>, u64)>>)>>
   }
   探针缓存，但它是 thread_local：每线程一份，内容按文件投影，**数量随线程数×文件数**，
   且 `forget_projections()` 只能清当前线程。设计上不允许：状态属于 Session，不属于线程。

S1 include/fuzzy.rs:78      static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<FuzzyIndex>>>>
S2 index/walk_cache.rs:179  static CONTRIBUTIONS: OnceLock<Mutex<Contributions>>
S3 cache_db.rs:723          static HANDLES: OnceLock<Mutex<...>>
   （另有三个探针用的 OnceLock，属于 probes，可保留但要标注）
```

三条都违反同一条规则：**一个会话一个所有者**。后果是可测的：内存报告无法给它们归账；第二个会话复用第一个的
表（看起来省内存，实际是"上一个项目的数据还在"）；测试之间相互污染。

### 3.3 启动构思里错的三条前提

```text
P1 "先把整个工程读进内存，磁盘缓存只是为了下次启动更快"
   → 对超大工程，内存副本本身就是问题。redb 已经在了，但摘要**同时又全在内存**（ProjectIndex::summaries），
     等于把磁盘当备份而不是当主存。

P2 "第二遍必须存在，因为环境要靠 summary 算出来"
   → 这条依赖是**循环**：环境需要 summary，summary 又需要环境（`docs/clangd-model.md` §九 的根因 4，
     `docs/exact-index-design.md` §2.2 有实测）。但它可以被**指令级读数**打破：一个文件的环境只需要
     它的 `#include` 与 `#define/#undef/#if` 事件，而这些不需要解析（`directive_summary` 已落地，
     1 963 个文件上除 2 个 include 语义分歧外与解析结果逐字段一致）。
     → 启动可以变成**一遍流式**：读指令读数 → 求环境 → 解析 → 写 redb → 丢掉。

P3 "单元表按条数限就够了"
   → 一条单元 0.1–6.2 MB，差 60 倍。所有缓存都必须**按字节**限。
```

## 四、方案

原则三条：

```text
① 一个所有者：Session 拥有所有缓存与预算；进程级 static 与 thread_local 清零。
② 一切按字节：每个缓存声明 `weight()`，由一个 `Budget` 统一扣减与淘汰；条数上限只作为次要保护。
③ 磁盘是主存：摘要/时间线/渲染以 redb 为准，内存里是**有界窗口**（打开文件的闭包 + 最近用过的）。
```

### M0 仪器（本次已落地）

`MemoryReport` + `memory_report` example + 各表 accounting。**验收**：一条命令打印出上面那张表。

### M1 给四张表加字节预算（不改变答案）

```text
F1 walked_by_content → 改成 `cache_lru::Cache`（已有实现），上限 64 MB；
                        淘汰顺序按"条目最小优先"（大闭包更值钱，与 units 的策略一致）
F2 units             → 预算从"16 条"改成 `128 MB`，同时保留条数上限作兜底
F3 definitions       → 预算 32 MB；键改 `(Arc<Path>, u32)`（路径共享，不再每条一份 PathBuf）
F4 vfs               → 预算 64 MB 文本 + 索引；**打开的缓冲区钉住不淘汰**；淘汰按 LRU
F5/F6 surfaces/effects → 预算 16 MB / 8 MB
```

实现方式：`cache_lru::Cache` 已有 `(entries, weight)` 双上限和按 weight 淘汰，`renderings`/`macro_environments`
已经在用；把 `HashMap` 换成它即可，接口不变。

**验收**：本语料冷启动峰值 ≤ **400 MB**（现 971），稳态 ≤ **250 MB**；冷启动时间 ≤ 10.5 s（不劣化）；
findings 18 → 18；`cargo test` 失败集合不变。

### M2 去全局化

```text
· S2 walk_cache   → `Session::walk_cache: WalkCache`（结构体，字段私有）；进程级 `clear()` 删除
· S1 fuzzy        → `Session::fuzzy: Mutex<HashMap<root, Arc<FuzzyIndex>>>`，按 root 一个会话一份
· S3 cache_db     → `SummaryStore::database: OnceLock<...>` 已有（`summary_database`），HANDLES 并入
· T1 walk_census  → 探针状态挂到 `Session`（`Option<WalkCensus>`，默认 None），`thread_local!` 删除
· CI 门：`grep -rn "thread_local!" crates/` 必须为空；`static .*OnceLock<Mutex` 只允许出现在 `probes/`
```

**验收**：两个 Session 前后创建，第二个的 `MemoryReport` 全零（除 VFS 外）；跨会话 RSS 不增长。

### M3 摘要磁盘优先

`ProjectIndex` 现在是 `HashMap<path, Arc<FileSummary>>`。改成：

```text
QueryWindow { resident: Cache<path, Arc<FileSummary>>, database: SummaryCache }
  · get(path)：resident → redb → 读文件重建（并写回）
  · insert()：写 redb + resident（受预算约束）
  · summaries() 的迭代器改为「resident ∪ redb 键」的只读视图（少数调用方需要；
    逐个审查：谁真的需要"全部摘要"，谁只需要"某个文件的闭包"）
```

**验收**：索引 10 000 个文件后 RSS ≤ 300 MB（摘要不再是常驻项）；打开文件的查询不因为窗口满而变慢
（p95 与现在相同）；`summary_database_sizes()` 的字节数随工程线性增长（在磁盘）。

### M4 启动索引流式化（真正的重构）

目标：**不再有"第一遍把一切装进内存"和"第二遍回填环境"**，两者合成一遍流。

```text
for each translation unit（源文件清单，来自 compile_commands.json 或目录扫描）:
    read its directives only            → Surface（词法级，1.7 ms/文件实测）
   求它的环境（拓扑：include 图上的前缀）
   parse each file ONCE with that environment          → 写 summary 到 redb，内存只留它的 facts 指针
   写 unit 到 unit cache（磁盘）；内存里只留 N 条最近使用的
   drop everything else
```

这消掉三样东西：`walked_by_content`（不再需要"按内容去重"，因为一遍就够）、第二遍（`parsed_since_the_last_pass`、
`RE_READ_SLICE`、`units_for_the_pass` 的 320 次走查）、以及"全部摘要常驻"。

**验收**（10 000 文件工程）：冷启动峰值 ≤ **500 MB**、墙钟 ≤ 今天的 10.5 s（目标是 5 s 量级，
见 `docs/exact-index-design.md` §2.2 的预测）；热启动 ≤ 1.5 s；findings 与今天逐字节相同；
`Session::memory_report()` 的任何字段都不随工程规模线性增长（除了磁盘字节数）。

### M5 空闲还页与遥测

```text
· 队列空闲时调用 mimalloc 的 `mi_collect(true)`（一次 FFI）把空闲页还给 OS；
  实测 RSS 与峰值相等，说明现在**从不还页**，这是用户看到 5 GB 的最后一环
· 服务器每 N 秒/每个 workspace 事件把 `MemoryReport` 写进日志与 status bar
· CI 加一条"内存预算测试"：索引语料后断言 `memory_report().total() <= 250 MB`
```

**验收**：编辑停止 60 s 后 RSS 回落到"活着的表 + 64 MB"以内。

## 五、风险与取舍

```text
R1 淘汰带来的重算。`walked_by_content` 从无界变成 64 MB，意味着冷启动后段可能重走闭包。
   代价可测：现在一条走查 9–20 ms；预算 64 MB 相当于 ~100 条，比 384 条少 3/4 的条目，
   但如果命中率掉得厉害，冷启动会变慢。**先用 M1 的预算测一轮冷启动墙钟，掉了就调大预算或改按"最小优先"。**
R2 磁盘优先的查询延迟。redb 读一条摘要 ~10–40 µs（`cache_db` 已有测量），比内存慢一个数量级，
   但只在"窗口未命中"时发生；窗口按"打开文件的闭包"预热，命中率应当很高。
R3 一遍流的正确性。环境由指令读数推导，与解析结果在 1 963 个文件上有 2 处分歧（两个 asio 的 include），
   必须先定"用哪一方"，再切；`examples/directive_oracle.rs` 是这条的验收工具（现已 2/1963）。
R4 mimalloc 的 `mi_collect` 是否真还页，要实测；如果它只整理不还，退路是 `MIMALLOC_PURGE_DELAY=0`
   或换回系统分配器再测。
R5 预算数值本身需要按机器/工程调；把预算放进 `MemoryBudget` 常量并在报告里打印"当前值与是否命中"，
   避免又一次"条数上限看起来生效"的错觉。
```

## 六、与已有文档的关系

```text
· `docs/exact-index-design.md` §2.2：第二遍的根因与"①②③④"计划。本文的 M4 是它 ③④ 的落地路径，
  并且给出了"一遍流"的内存收益（这也是它没算过的那一半）。
· `docs/refactor-cache-and-startup.md`：redb 缓存与 M1 指标（磁盘字节、命中率）。本文的 M3 接着它做。
· `docs/clangd-model.md` §九：根因 4（summary↔environment 的循环）。
· 本文的仪器：`Session::memory_report()`、`examples/memory_report.rs`。
```
