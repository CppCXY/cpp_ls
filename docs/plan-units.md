# 以**编译单元**为工作单位(方案,待评审)

> **一句话**:我们不是算得慢,是把同一批字节算了一百多遍。把工作单位从"文件"换成"编译单元",下面每一层都跟着变简单,
> 而不是变复杂。

这份文档是**提案**,不是规范。它落地之后,结论并进 `architecture.md`(§1 的流水线、§6 的记录、§7 的读数),这份文件删除——
`architecture.md` §0 的规矩是"唯一规范",一份并进去的方案不该留在外面。

---

## 1. 证据(本轮实测,全部 release,本机)

用户的工作区 `C:\Users\zc\Desktop\cpp_project`:**一个 2 896 字节的 `main.cpp`**,`#include` 了
`<cstdio> <iostream> <optional> <string>`,include 闭包 **138 个文件**,项目里只有 1 个 .cpp。

| 谁 | 做什么 | 读数 |
|---|---|---|
| 我们 | 冷启动索引这 138 个文件 | **26.20 s**(`reused: 0, rebuilt: 194, unstored: 1`) |
| MSVC 前端 | `cl /Zs`(语法检查)+ `cl /E`(预处理)**各一遍** | **1.91 s** |
| MSVC 前端 | `cl /E` 渲染出来的整个编译单元 | **1 804 701 字节 / 70 441 行** |
| 闭包本身 | `cl /E` 的 `#line` 提到的文件 | **109 个文件 / 2 863 407 字节**(索引读了 138 个:多出来的多半是整个正文都落在没取的分支里、一个 token 都没贡献的文件——§7 那条"空渲染不等于读过"的规矩) |
| 我们的词法器 | 词法化那 1.8 MB 渲染 | **12.3 ms**(147 MB/s) |
| 我们的语法器 | 解析那 1.8 MB 渲染 | **264 ms** |

三条结论,每一条都能自己站住:

1. **原材料便宜得离谱。** 按同一个语法器的吞吐,2.86 MB 源码全部走一遍 = 约 **0.4 s**;整个程序的渲染解析一遍 = **264 ms**。
   26.2 s 里 **97% 不是"算",是"重复"**。
2. **重复来自工作单位,不来自算法。** 见 §2:索引是"每个文件一次"、熟读是"每个文件用自己的闭包环境再读自己一次"。
   `rebuilt: 194` 对 138 个文件——第二遍 pass 又读了一遍。
3. **冷路径才是用户走的路径。** `reused: 0` 不是意外:`READING_FINGERPRINT` 把 `cpp_parser/src` +
   `cpp_code_analysis/src` 的哈希写进缓存键(§7),所以**我们自己每改一次源码,下一次启动的缓存就全部不可达**——
   开发期每次重启都付 26 s。用户报的"启动 30 秒"与这个形状吻合(那个工作区的 `.cppls/summaries` 里已经有约 150 个
   条目,说明缓存确实写进去过,只是每次都被指纹废掉)。结论不变,而且更强:**冷路径不是"第一次",是"每次升级之后"。**

语法器那条超线性要单独记一笔(4× 输入 → 19× 时间:0.064 → 0.150 ms/KB)。1.8 MB 上它是 264 ms,不成问题;
它是**下一个数量级**的问题,不是这个。列在这里是为了不把它忘掉。

---

## 2. 诊断:工作单位错了

今天每一个阶段都是"以文件为单位"发起、以文件为单位收尾:

```text
索引(SummaryStore::get)   对 138 个文件各做一次:read → hash → parse → 作用域走查 → 事实扫描 → encode
                          (然后 re_read_where_a_body_decides 再对其中一批做第二遍)
熟读(Session::cook)       对每个要读的文件各做一次:walk 自己的闭包 → lex → 展开 → 渲染 → 再解析渲染
查询(ProjectIndex)        问某个文件的声明时,读的是**那个文件**的读数
```

而语言自己的工作单位是**编译单元**。编译器对同一个闭包做的事:

```text
一次 walk(include 栈 + 条件 + 宏表)  →  每个文件的字节读一次、词法一次
                                    →  整个程序展开成一条流
                                    →  这条流解析一次
                                    →  每个声明带 (文件, 偏移),所以"哪个文件"是位置,不是单位
```

**这不是我们没想到,是产品路径没接上。** 代码里已经写着这件事:

- `preprocess/cooked.rs:24` —— "**`#include` 是一条边界**:被包含文件的 token 不在这个流里。熟读一个*编译单元*
  意味着按 include 顺序把各文件缝起来,那需要 include 图——**那是下一步**,本模块有意停在文件。"
- `summary.rs:1532` 的 `TranslationUnit::cook_the_unit` —— 那一步**已经落地**(§6"整个 TU 拼成一条流"),
  255 个 SDK 文件缝成 517 021 个 token、解析 2.06 s + 2.34 s,三档语料的错误数与逐文件读数**完全吻合**。
- 但它的调用者只有 `examples/std_probe.rs` 和 `tests/translation_unit.rs`——**`Session` 一次都没用过**。

也就是说:地基已经浇好、已经量过、有测试,而房子还盖在旧地基上。这个方案就是把产品路径搬到新地基上,
外加"把索引也搬到单元上"这半边(今天连索引都是按文件走的)。

### 2.1 代价的形状,为什么它随项目变大

一台机器上,一个闭包 138 个文件、其中 137 个是头文件:

| | 今天 | 编译器 |
|---|---|---|
| 词法 | 每个文件至少 2 次(索引一次、熟读一次),且**每次熟读还要重读闭包** | 每个文件 1 次 |
| 语法 | 138 次(每个文件一次)+ 熟读时每个文件再一次 | 1 次(整个程序) |
| 宏环境 | 每个文件走一次自己的闭包(被 TU 缓存救回一部分) | 1 条时间线,位置即环境 |
| 事实扫描 | 138 次,每次的输入是那个文件的文本 | 1 次,输入是整个程序 |

所以 26 s / 138 个文件 = 190 ms 一个文件,而**这 190 ms 里真正的解析只占很小一块**——按同一台机器的
parse_scale 读数,13 MB 源码走一遍语法器大约 2 s,占不到 8%。剩下的是**每个文件一次的固定开销**:
建 config、建作用域、建宏证据、建 summary、encode、写缓存、第二遍 pass。

单位一换,这一整类开销从"每文件一次"变成"每程序一次"。

### 2.2 顺手量到的第二条:`buffer_changed` 把**所有**单元作废

`session.rs:837`:

```rust
// The **translation units** too: every one of them was a walk over a closure that contains this file.
self.units.clear();
```

这是"最短的正确失效"(注释里就是这么写的),但它把一条**语言事实**当成了"保守":改一个 `.cpp` 的**函数体**,
不会改变任何头文件的宏状态,也不会改变这个单元里任何别的文件的事实。而 clangd 与 rust-analyzer 都把这条
明确写成不变式——clangd 里落在 preamble 区域**以下**的编辑根本不作废 PCH;rust-analyzer 的 `ItemTree` 不变式是
"**在函数体里打字,永远不会让全局派生数据失效**"。

今天每一次按键都丢掉整张单元表,下一次熟读就要重新 `walk`/解码一遍闭包。这条**不需要**等阶段 1/2,它是一个独立的、
一句话的规则(见阶段 3),而且它决定"打字"这一侧的手感。

---

## 3. 从 clang / clangd / ccls / rust-analyzer 学到的

**先摆外部读数**(不是我们的机器,是别人的实测,用来校准"这算不算慢"):

| 出处 | 配置 | 读数 |
|---|---|---|
| ISOCPP std-discussion,51 次取中位 | 一个**只用 include、什么都不用**的 TU,35 个常见标准头 | GCC 13.3 C++23:**112 865 行 / 868 ms**;Clang 18.1:**86 602 行 / 995 ms**;带 PCH:**238 ms / 102 ms** |
| clangd 设计文档 | preamble 与 AST | "首次构建 preamble 可能**极慢——几十秒**;有了最新 preamble 的增量 AST 构建很快(**亚秒**)" |
| GCC `-ftime-report` | -O0 大工程 | parser 52%、name lookup 15%、**preprocessing 只有 7% CPU(但 23% wall,是 I/O 那一段)** |

三条推论,和我们的读数拼在一起:**一个真实编译器读一个"空" TU 的 include 闭包要 0.9–1.0 s**;
我们同一个形状的闭包(4 个头的 138 文件)要 **26.2 s**。差的不是解析速度——我们的词法是 147 MB/s、比 clang 快——
差的是"读了几遍"。

### 3.1 单元是工作单位,文件只是位置

Clang 的一个 `CompilerInstance` 拥有一个 TU:`SourceManager`/`FileManager` 是**一份字节表**,
`Preprocessor` 是**一次词法 + 一个 include 栈 + 一张宏表**,AST 节点带 `SourceLocation = (FileID, offset)`。
"这个声明在哪个文件里"是**位置**,不是工作单位。→ 我们的 `TranslationUnit` + `RenderedUnit` 就是这两件东西,
只是产品没接上(§2)。

另一台 C++ 服务器 **ccls 也是这么做的**,而且可以直接对照:`idx::index()` 返回的 `IndexResult` 里是
**这个 TU 里每个文件一个 `IndexFile`——头文件也在内**,每个带 path、args、mtime、`dependencies`、`includes`、
`usr2func/usr2type/usr2var`(以 Clang USR 为键);文件正文不序列化。
(来源:`src/indexer.hh`、`src/pipeline.cc`、`src/query.hh`,见文末链接。)
**这就是本方案阶段 1a 的"一次 parse、按文件分组发事实"**,别人已经这么做了。

### 3.2 preamble / suffix:前缀缓存,后缀重读

- clangd 的 `PrecompiledPreamble::CanReuse` 判定复用,**三件事全部成立**才算有效:①前缀区域逐字节相同、边界相同;
  ②`FilesInPreamble` 里每个文件的 **size 与 mtime 都没变**(内存缓冲区用 MD5);③上次"找不到"的文件仍然找不到。
- **边界**是语言自己的:`ComputePreambleBounds` = 第一个声明之前最后一个顶层 `#include` 结束的位置。
- 失效是**懒的、逐文件的**:没有"头文件改了 → 扇出通知所有依赖者"这回事;一个文件下次 `update()` 时才重建。
- 编辑器里的改动走 `DraftStore` 覆盖层;落在 preamble 区域**以下**的编辑**根本不作废 PCH**(`PreamblePatch` 复用基线)。
- ccls 有同一层:`SemaManager` 每个打开的文件一个 `PrecompiledPreamble`,记录 include 的 mtime,新就失效;
  光标落在 preamble 里就用 `SingleFileParseMode`,否则重新排队建 preamble。

**我们对应的是**:环境 = 时间线上的位置(`MacroView`),事实 = 索引里的条目。于是编辑的代价应该只取决于
"改动落在边界哪一侧":**`#include`/`#define`/第一个声明之前 = 作废这个单元;之后 = 只重算这个文件自己的那一段。**
这条规则今天没有,是我们阶段 3 要补的,而且是**一句话的规则**,不是一套机制。

三条核对源码之后才能说的话:

- **clangd 每次按键都建一个全新的 `CompilerInstance`**(`ParsedAST::build`),`SourceManager`/`FileManager`/token/诊断
  **全部丢掉**——"**没有增量 AST**"。活下来的只有 PCH(前缀)和 `PreambleData` 里那几张只能在解析中观测到的表
  (`IncludeStructure`、`MainFileMacros`、`StatCache`…)。所以我们的后缀重读**也该是一次全新的渲染 + 解析**,
  不是去改旧树——这与 §6"不做增量重解析"是同一条。
- **前缀不能跨 TU 共享**:preamble 里含着主文件开头那一段自己的字节,所以它是**每文件**的;
  跨 TU 共享的那件东西是**索引**,不是解析(clangd 的分片就是按声明文件存、启动时按 include 图传递加载)。
  这条回答了"我们是不是该做一个可共享的 PCH"——**不该,该共享的是事实**。
- 我们的键**比 clang 强**:clangd 用 size+mtime(内存缓冲区才用 MD5),文档里自己写"我们**希望** mtime 就够了";
  我们用的是**内容哈希**。这一条不要为了省几十毫秒退回去。

### 3.3 索引与解析分离:能查索引的就不建树

clangd 的两条路要分开说(这一条第一版写得太粗,核对源码后改):

- **纯索引**:补全与项目符号 —— `FindSymbols.cpp`:`Index->fuzzyFind(...)` + `FuzzyMatcher` +
  `SymbolQualitySignals`/`SymbolRelevanceSignals` + `TopN`,完全不碰 AST;静态索引的 Dex 倒排在排序前会**过取**
  (`Corpus.limit(Root, *Req.Limit * 100)`)。
- **AST 优先、索引合并**:跳转 —— `locateSymbolAt` 先在 `ParsedAST` 上跑 `SelectionTree` 定出指称,
  再发**一次批量** `LookupRequest` 把索引里的定义合进来(`enhanceLocatedSymbolsFromIndex`);
  纯文本兜底那条名叫 `locateSymbolNamedTextuallyAt`(≤10 个候选,超过 5 个就放弃)。
  → 我们的形状**正好是这样**(先在树上 `sema::resolve` 定出指称、再问索引),这条路不用改。

索引来自**后台索引池**,它吃的正是 `compile_commands.json` 的每一个编译任务——**也就是每个 .cpp 一次**,
不是每个文件一次;`.idx` 分片按**声明所在的文件**存(`filename + "." + hex(digest(path)) + ".idx"`,
RIFF `Version = 21`),重索引时摘要没变的文件被 `FileFilter` 挡掉、只有变了的分片被重写。
分片在启动时**按主文件的 include 图传递地全部读进内存**(`loadProject` 是队列最高优先级),
查询**从不碰盘**。发布节奏:`BackgroundIndexRebuilder{TUsBeforeRebuild = 100}`——第一批 N 个 TU 之后发布一次,
以后每 100 个、空闲时、每次加载会话之后各一次。

→ 我们今天的 `ProjectIndex` 已经是"事实的库",这一层不用重新发明;要学的是三件事:①**产出事实的单位是单元**;
②**存放的单位是文件 + 按名字倒排**;③**发布是批量、有节奏的,不是每读一个文件就换一份**(我们的写锁粒度可以照抄这条)。

### 3.4 线程:一个单元一个 worker,单元之间并行;后台工作低人一等

| clangd 的做法 | 出处 |
|---|---|
| `TUScheduler`:每个打开的文件一个 `ASTWorker`(**每文件 ≥2 条线程**:AST + preamble),FIFO,"任何请求按 FIFO 排队" | `clangd/TUScheduler.cpp` |
| 线程数 = `heavyweight_hardware_concurrency().compute_thread_count()`(**物理核**),`-j` 覆盖;它限制的是**并发**,不是创建 | `clangd/TUScheduler.cpp` |
| `BackgroundIndex`:线程池 `ThreadPoolSize = max(AsyncThreadsCount, 1)`;优先队列 + `Key = hash(path)` 去重("**实际上我们从不对一个文件重新索引**");先加载已有分片,再按 TU 洗牌索引 | `index/Background.{h,cpp}`、`index/BackgroundQueue.cpp` |
| 后台线程**降优先级**(`--background-index-priority` 默认 `low`,Linux 走 `SCHED_IDLE`),进程内还有 `malloc_trim` | 同上 + `tool/ClangdMain.cpp` |
| 陈旧的工作**在出队时取消**,不是抢占:版本化的请求、能省掉的更新直接省掉、该降级的降级 | `clangd/TUScheduler.cpp`、`support/Cancellation.h` |
| 保留策略有**硬上限**:`ASTRetentionPolicy::MaxRetainedASTs = 3`;文件一关,worker 与 AST 立刻销毁 | `clangd/TUScheduler.h` |
| 主循环单线程、**不许阻塞**:"它的方法不该阻塞——那会挡住无关的(甚至相关的,比如取消)消息" | `clangd/ClangdServer.h` 的设计说明 |

→ §5 阶段 2 的形状就是这张表:**单元之间并行、单元内部顺序、一个写者、后台低优先级、有上限地保留中间产物。**

### 3.5 从 rust-analyzer / Salsa 学到的一条不变式(比并发更值钱)

`ItemTree` 的设计不变式是:**"在函数体里打字,永远不会让全局派生数据失效"**。
配套的两条:①`salsa` 的 **backdating**——重算出来**相同**就停止传播失效;②parse 记忆是**有上限的**
(`DEFAULT_PARSE_LRU_CAP = 128`),树是"半临时"的(要保持常驻就会"内存翻一倍以上")。
另外 rust-analyzer 自己说得很清楚:`rowan` 的增量重解析**不是承重墙**——"实践中增量重解析对 IDE 用处不大,
从头解析看起来已经够快"。

→ 我们要抄的不是增量重解析(我们连"从头解析"都还没做对),而是那**两条不变式**:
**改函数体不作废单元的事实;事实重算后相同就不要往传播链上走。**
第二条尤其便宜:我们的单元缓存键是闭包内容哈希,所以"改了一个头,内容又改回去"必须命中,而不是重算。

### 3.6 明确**不**学的四条

1. **不用 mtime 做唯一判据**(clangd 用 size+mtime,是因为它有 PCH 这样的字节级产物;我们的键是内容哈希,更强);
2. **不做无界的记忆化**(parse/AST/流都要有 LRU 上限,并把上限写进文档);
3. **不假设"预处理是瓶颈"**(GCC 的账里 preprocessing 只占 7% CPU;我们的账要自己量,见阶段 0);
4. **不落盘 PCH/AST**(clang 的 preamble 是"几十秒、留磁盘"的重物;我们落盘的是**事实**,小一两个数量级)。

### 3.7 对照表:别人的零件 ↔ 我们的零件

| clangd / ccls / RA | cpp_ls 现在 | 本方案 |
|---|---|---|
| `FileManager` + `SourceManager`(一份字节表) | `Vfs` + `CachedFiles`(`Arc<str>` 内容缓存) | 不变 |
| `Preprocessor`(一次词法、include 栈、宏表) | `TranslationUnit`(时间线 + `MacroView`) | 从"给一个文件"升为主线 |
| `PrecompiledPreamble` + `CanReuse` | `TranslationUnitCache`(闭包内容哈希) | 扩:同一键下再存单元事实 |
| `-E` 的输出 | `RenderedUnit`(`cook_the_unit`) | 从探针专用变成产品路径 |
| AST + `SourceLocation` | 渲染流上的真实树 + `UnitSpan{written, reported}` | 不变,多一个"按文件分组"的出口 |
| `BackgroundIndex`(吃 CDB 的每个 .cpp) | `Session::advance` 逐文件 | 阶段 1/2:按单元派发 + worker 池 |
| `FileIndex` / `MemIndex` / 分片 | `ProjectIndex`(文件 → 事实) | 事实带"哪个单元读的";批次按单元进 |
| `TUScheduler` 每文件一个 worker | 一把 `RwLock<Session>` + 一个泵 | 单元 worker + 单写协调者 |
| `MaxRetainedASTs = 3` | 无上限 | 单元中间产物按 N 与"用完即弃"框住 |
| LSP 主循环不阻塞 | 已经如此(`query_blocking` + 快照) | 不变,读快照的粒度改成"按单元提交" |

---

## 4. 目标结构

```text
                     ┌────────────────────────────────────────────────┐
   一个 .cpp  ──────► │ UnitReader:一个编译单元,一次读完               │
                     │  1 walk        闭包 + 条件 + 宏时间线(TU 已有) │
                     │  2 lex         每个文件一次(内容键,跨层复用)   │
                     │  3 expand      按 include 顺序缝成一条流(已有) │
                     │  4 parse       这条流解析一次                   │
                     │  5 emit        (文件, 偏移) 分组的事实 + 诊断   │
                     └───────────────────┬────────────────────────────┘
                                         │ UnitFacts
                     ┌───────────────────▼────────────────────────────┐
                     │ 索引:按**文件**存事实,按**名字**建倒排          │
                     │  每份事实记得"哪个单元读出来的"(见 §4.2)        │
                     └───────────────────┬────────────────────────────┘
                                         │
   编辑器里打开的文件 ──────────────────► │ 熟读 = 这个文件自己的 token 在**单元环境**下重渲染 + 重解析
   (缓冲区,未保存)                      │ (前缀不重读:它的环境是时间线上的一个位置,它的事实已在索引里)
```

四个对象,职责互不重叠:

| 对象 | 是什么 | 生命周期 |
|---|---|---|
| `TranslationUnit` | 一个闭包的**时间线**:文件、条件、宏事件、frame 路径 | 已有;落盘已有(`TranslationUnitCache`,闭包内容哈希做键) |
| `UnitReader` | 把时间线变成**一条流 + 一棵树 + 一组事实**的那段代码 | 新增:是 `cook_the_unit` + 一次 parse + 分组发事实 |
| `ProjectIndex` | 事实(按文件)+ 倒排(按名字)+ 宏表 + include 图 | 已有;`insert_cooked` 从"一个文件"改成"一个单元的一批文件" |
| 会话(`Session`) | 队列 + 协调者:谁先读、失效谁、把事实并进去 | 已有;工作是"派发单元"和"合并",不再是"逐文件推进" |

### 4.1 两种读数各自留在哪一层

- **裸读**(文件自己的文本)本来就是**每文件**的:死分支、宏体、没取的分支都在里面,大纲刻意读它。**不变。**
- **熟读**(编译器看到的那份)本来就是**每单元**的:今天我们把"单元读数"拆成 138 个"单文件读数"来算,
  所以既贵、又对作用域说谎(一个头文件单独编译时 `_STD_BEGIN` 不在环境里)。**改成单元读数之后它反而更准。**

### 4.2 一个头文件被两个单元读到,答案是谁的

这是换单位以后**唯一**新的正确性问题,必须现在写下来:同一个头,两个单元用不同的 `-D` 读它,事实可以不同。

- 今天的形状**不能**回答这个问题,因为它根本不记环境:头文件的 summary 与宏环境无关,所以它对作用域一律说错。
- 目标形状:事实按 `(文件, 单元根)` 存,**退化规则按 clangd 的来**——问一个文件时用"最接近它的那个单元":
  ① 它自己就是单元根 → 用它自己;② 否则用它**最后被读到的**那个单元;③ 答案里说清是哪个单元(`NameProvenance` 已经有
  这个形状:四档来源,查询会说出它用的是哪一档)。
- **clangd 把这条规则写死在一个地方,我们照抄那个形状**:`Merge.cpp` 的 `isIndexAuthoritative` ——
  **动态索引对它覆盖的文件是权威的**,静态索引里那些文件的条目被丢掉并**计数**(`StaticDropped`),而不是静默消失。
  我们的对应物是"缓冲区/自己单元的读数 > 别的单元的读数";被盖掉的那份要**计数**,让"两个单元不一致"
  变成一个能印出来的数,而不是一个看不见的覆盖。
- **"谁读过这个头"这个问题,clangd 有意不用 include 图回答**:`TUScheduler::HeaderIncluderCache` 只维护
  "头 → 一个打开它的主文件",用途**只是借一条编译命令**,文档原话是"工程级的 include 图很大!"。
  我们的失效走的是反向 include 边(`dependents_of`),那是另一件事(且我们必须有);但要**分开**:
  "给这个头找一条编译命令"和"这个头改了要重读哪些单元"是两个问题,别用一个数据结构回答两个问题(§8 第 19 条的教训)。
- 这条规则要有一条测试钉住,并且要能被探针打印出来(哪个文件、哪两个单元、差在哪)。

---

## 5. 阶段与门禁

**纪律**(沿用 `architecture.md` §7,并加一条):每一阶段只认**一条读数**;读数不动就**停下重新量**,不许往下推。
按文件 diff,不看总数。每次只跑能吃掉的测试,不跑全套。

**阶段 0 已经跑完了,而它否掉了阶段 1 的排序**——见 §11。下面保留原来的计划,顺序按 §11 修正。

### 阶段 0 —— 把账算平(半天)

在 `workspace_probe`(或一个新探针)里给现在的路径加**分级计时**,让每一段加起来等于总数:
`read/hash | lex | parse | 作用域+事实 | 宏证据 | encode+写缓存 | 第二遍 pass | walk | stitch | 渲染 parse | index_rendering`。

- **门禁**:各段之和与总时间的差 ≤ 5%,否则先修仪器(§8 第 15 条:加计时器之前的所有猜测都是错的)。
- **形状按 `-ftime-trace` 来**:clang 的 `-ftime-trace` 是**按被包含的文件**给区间的,它就是这么找出"一个头
  花了 8 秒"的。我们照抄这个粒度:**每个被走查到的文件一行**,带上 include 栈深度,而不是只给总计。
- **产出**:一张表,决定 1 里先做哪一半。**这一阶段不改任何产品代码。**

> **做完了,见 §11。** 产物是 `crates/cpp_code_analysis/src/stages.rs`(互不重叠的阶段计时,细节阶段单独一块、
> 不计入总数)与 `workspace_probe` 里那张表。它**否掉了阶段 1 作为第一步**。

### 阶段 1 —— 单元读数(核心)

`Session` 长出一条新路径:**读一个单元**。

```text
read_the_unit(root):
  unit   = translation_unit_of(root)          // 已有:内存 → 磁盘 → walk
  stream = unit.cook_the_unit(sources, defs, seed, flag)   // 已有,产品第一次用它
  tree   = parse(stream.text)                 // 一次
  facts  = index_unit_rendering(root, &stream, &tree)      // 新增:一次走树,按 written/reported 分组到文件
  index.insert_unit(root, facts)              // 新增:一批文件一次写锁
```

- **1a** `index_unit_rendering`:一次 parse 变成**每个文件各自的事实 + 各自的诊断**。分组用已有的
  `RenderedSpan::{written, reported}`:`written` 说"拼写站在哪个文件里",`reported` 说"该往哪个文件报"
  (宏产生的 token 报在最外层调用点)。`map_into_the_file` 已经有这条规则,现在要按**文件**分组而不是收敛到一个文件。
- **1b** walk 不再需要"闭包内每个文件的 summary":它需要的是每个文件的**指令读数**(includes/条件/宏/guard),
  而那正是它自己必须扫一遍的东西(词法 147 MB/s,扫指令几乎免费)。`SummaryStore` 于是退回它真正的角色:
  **没人读过的文件**的读数 + 落盘缓存,而不是每个单元里 138 个文件的必经之路。
- **1c** 单元的**事实随单元落盘**:`TranslationUnitCache` 今天存时间线,再存一份事实,键一样(闭包内容哈希)。
  于是热启动 = 解码,而不是"138 次 summary 命中"。
- **门禁**:用户那个工作区(1 个 .cpp / 138 文件闭包)**冷启动 < 5 s**(现在 26.2 s);探针里那几张表
  (`definition` / `members_of` / 补全 / `Ambiguous` / unresolved 的逐条清单)**不退化**;`cargo test -p cpp_code_analysis` 绿。
- **不做**:这一步不碰并发,不碰 LSP。先把"读一个单元"这件事做对、量出来。

### 阶段 2 —— 单元之间并行(用户说的"多线程索引")

到这一步,工作项已经是"一个 .cpp",天然互不相干——**这就是编译器自己的并行维度**(`-j`;clangd 是每个打开的文件
一个 worker + 一个后台索引池)。一台机器一个单元内部是顺序的(include 栈和宏时间线只能顺序走),
单元之间没有共享状态,所以并行不需要把 `Session` 变成 `Sync`:

```text
   LSP 主循环(tokio, 只发请求/收答案, 永不阻塞)
        │ 读快照(不可变:索引 + VFS + 配置)
        ▼
   协调者(1 个任务)  拥有队列与索引的写权;派发单元根;合并事实;推进序号
        ├── worker 1 ─┐
        ├── worker 2 ─┤  各持一个单元,跑 read_the_unit 的 1–5 步,只读共享数据,只产出事实
        ├── worker N ─┘  N = min(核数-1, 8);留一个核给主循环
        └── 事实经有界通道回协调者,按**单元**成批合并(写锁次数比现在少 138 倍)
```

- **worker 是纯的**:只读(文本、配置、时间线),只产出事实。索引永远单线程写——失效只有一处,
  今天那条"一个请求必须看见它之前的每一条通知"的序号规矩原样保住。
- **照抄 clangd 的六条策略**,不自创:
  ① 线程数按**物理核**算(`TUScheduler` 用 `heavyweight_hardware_concurrency()`),留一个核给交互;
  ② 队列按路径**去重**("实际上我们从不对一个文件重新索引")+ 打开文件所在单元**提优先级**(`boostRelated`);
  ③ 后台线程**降优先级**(clangd 默认 `low`,Linux 走 `SCHED_IDLE`);
  ④ **陈旧的工作在出队时丢掉**,不是抢占(改过的单元重新排队,而不是跑两遍);
  ⑤ 中间产物有**硬上限**(`MaxRetainedASTs = 3`):单元流/树只留最近 N 个,关掉的文件立刻丢;
  ⑥ 长循环里显式查取消(`Cancellation.h` 明说"库代码不查就停不下来")——我们的词法/展开循环正是 Salsa 那种盲区。
- **先不要分片索引**:写锁按单元批量进,协调者成为瓶颈**之后再**谈分片,不提前。
- **门禁**:造一个 M 个 .cpp 的合成工程,`M=8` 时索引总时间 ≈ 串行的 1/N(同一台机器 A/B)。
- **要说清楚**:用户手上这个工作区只有 **1 个** .cpp,所以这一阶段**对它一点用都没有**。
  它值钱的地方是"一个真实工程有几百个翻译单元"。这一阶段的收益要用那个合成工程量,不能用这个工作区量。

### 阶段 3 —— 编辑器那半:preamble / suffix

打开的文件的读数:

```text
前缀 = 它的 include 闭包(环境 = 时间线上的位置, 事实已在索引里)   ← 一次读完之后不再重读
后缀 = 这个文件自己的 token                                        ← 每次按键只重算这一段
```

于是"编辑 → 重读"的成本与**闭包大小无关**,只与这个文件自己有关。这是 clangd 的 preamble 模型,
只不过我们的"preamble"不是序列化的一棵 AST,而是"宏时间线的位置 + 已经在索引里的事实"。

三条具体的规则,两条来自 clangd,一条来自 rust-analyzer:

1. **边界是语言自己的**:`ComputePreambleBounds` = 第一个声明之前最后一个顶层 `#include` 结束的位置。
   我们用它把"这次编辑要不要作废单元"变成一个**位置比较**:改在边界**以上**(`#include`、`#define`、声明之前的文本)
   → 作废这个单元;改在**以下**(函数体、类体里的实现)→ **不作废**,只重算这个文件自己的那一段(§2.2 的那条)。
2. **重算出来一样就不要往下传**(Salsa 的 backdating):单元的键是闭包内容哈希,所以"改坏了再改回去"必须**命中**;
   而"头文件被 touch 了一下"在我们的键下**本来就不算变化**——这正是我们比 clangd 的 size+mtime 强的地方,
   不要退回去。
3. **一个单元失败了,退回逐文件读数**(见 §7 第 3 条),并把"这个单元没读成"记进状态。

- **门禁**:同一个编辑,在一个 138 文件闭包的文件上和一个空文件上,**重读耗时同阶**(现在前者是后者的几十倍);
  端到端:按键 → 诊断/补全出答案 < 200 ms(release);
  以及一条钉住不变式的测试:**在函数体里打字,单元表不被清空**(现在 `units.clear()` 每次按键都跑)。
- 顺带把"头文件改了要重读哪些单元"这条失效做成显式的:改一个头 → 只重读**读过它的那些单元**,
  而不是"所有打开文件的闭包"。

### 阶段 4 —— 收尾:删掉只为"按文件读"存在的东西

- `Session::cook_the_open_files` / `want_the_closure_cooked` 这一族"每个文件都熟读一遍"的调度;
- 只在按文件路径里才需要的那层映射(`map_into_the_file` 保留,但消费者变成分组器);
- 探针里"逐文件熟读"的那套(`std_probe --cooked` 的对照面)**保留**,它是单元读数的外部判据,不是脚手架。

**门禁**:`rg` 出来的调用点只剩新的那条路径;全套测试绿;四档语料的读数与阶段 1 结束时一致。

---

## 6. 不做的事(免得方案看起来像"什么都要做")

- **不落盘 AST、不做 PCH。** 我们落盘的是**事实**(声明、宏、include 边),比 clang 的 PCH 小一两个数量级;
  序列化一棵 C++ 树再惰性反序列化,是我们没有的资源换不来的东西。clangd 自己说 preamble"可能极慢——几十秒",
  它的代价我们不必继承。**clang 那边"缓存解析结果"的历史也印证了这条**:`ASTUnit::Reparse` 拿住
  `CompilerInvocation` 复用,`ResetForParse()` 把 `SourceManager`/`Sema`/`ASTContext`/`Preprocessor` 全丢掉——
  所谓 reparse 就是"只有 PCH 活下来的一次全量重解析",clangd 6 直接换成了 `ParsedAST::build`。
- **不做模板实例化 / 不做类型系统**。`auto` 的 54/60 拒答在模板体里,那是另一条线,与本方案正交。
- **不做 M4(删形状规则)**。它已经量过、判断过(§6),与本方案无关。
- **不做增量重解析**(复用旧树里没变的子树)。rust-analyzer 装了它,并且在自己的文档里明说
  "实践中增量重解析对 IDE 用处不大,从头解析看起来已经够快";我们连"从头只解析一遍"都还没做到。
- **不用 mtime 当判据**。我们的缓存键是内容哈希,比 clangd 的 size+mtime 强,不要退回去。
- **不做无界记忆化**。中间产物要有 LRU 上限并把上限写进文档(rust-analyzer 的 parse 记忆是 128;
  clangd 常驻 AST 上限是 3)。
- **不在单元内部并行**。include 栈 + 宏时间线是顺序语义,用户已经明确接受这一点,clang 也一样。
- **不追求"零秒启动"**。目标是把 26 s 变成 < 5 s、把按键变成与闭包无关,**不是**把编译器做的事省掉。

---

## 7. 代价与风险(现在说,不是出事再说)

1. **这是一次重构,不是一次优化。** `SummaryStore`(95 KB)与 `ProjectIndex`(400 KB)的入口语义会变:
   "读一个文件"降级成"没人读过它时的兜底","读一个单元"升格为主线。阶段 1 必须**新路径与旧路径并存**跑一轮,
   用探针的两张表对比,确认没有退化,再删旧的。
2. **事实的归属变了。** 今天"这个文件声明了什么"是一个文件自己的事;以后它是"某个单元读它时看到了什么"。
   §4.2 那条规则(最接近的单元)必须写进文档、钉进测试,否则会出现"同一个头在两个文件里给出两套答案"
   而没有任何东西报错——这正是 §8 第 19 条那类事故的形状。
3. **一个单元失败了,影响面是一个单元。** 今天一个文件解析失败只影响那个文件;以后渲染/解析失败会让**整个单元**
   的文件都没有熟读。缓解:失败时**退回逐文件读数**(旧路径还在),并把"这个单元没读成"记进状态而不是静默。
4. **内存峰值变高。** 一个 worker 手里是"一条 1.8 MB 的流 + 45 万 token + 一棵树"。用 N 上限和"提完事实立刻丢中间产物"两条规矩框住,
   并把峰值印进探针。
5. **数字可能不如预期。** 阶段 0 的表如果说明 26 s 主要在事实扫描而不是重复,那么阶段 1 的收益会明显小于 5×,
   那时该做的是**先停下重新量**,而不是硬推后续阶段。

---

## 8. 与现有代码的对应

| 现有 | 处置 |
|---|---|
| `lex` / `CppSyntaxTree` / 宽容语法 | **不动** |
| `scan_directives` / `condition` / `guard` / `MacroTable` | **不动** |
| `TranslationUnit` / `UnitDefinitions` / `MacroView` / `MacroFile` | **不动**;从"给熟读一个文件用"升格为主线 |
| `TranslationUnit::cook_the_unit` / `RenderedUnit` / `UnitSpan` | **不动**;从探针专用变成产品路径 |
| `TranslationUnitCache` | 扩:同一个键下再存单元的事实 |
| `SummaryStore` | 收窄:兜底读数 + 落盘缓存(1b 之后它不再是闭包的必经之路) |
| `FileIndexer::index_rendering` | 加兄弟函数:按文件分组(1a) |
| `ProjectIndex` | 插入按单元成批(1a);事实带上"哪个单元读的"(§4.2) |
| `Session::cook` / `want_*_cooked` | 阶段 3 之后只剩"打开的文件"这一种用途 |
| LSP 壳(`cpp_ls`) | **表面不变**;`AnalysisState` 从"一把写锁 + 一个泵"变成"协调者 + worker 池" |

---

## 9. 下一步(**已按 §11 的读数修正**)

阶段 0 的读数(§11)把顺序改成了:**先修 `declared_type_of`/`declared_returns_of` 的每绑定下潜(冷启动 23.8 s 里
的 14.4 s),再把 `bodied-scan` 缩到这一轮真正相关的范围(暖启动 3.9 s 里的 3.8 s),然后才是单元读数。**
两项都不动架构,都在已有的文件里;门禁是 §11 那张表里的数字。

---

## 10. 出处(每条都能自己核对)

**clangd / clang**(源码在 `llvm-project` 的 `clang-tools-extra/clangd/` 与 `clang/lib/`):

- `TUScheduler.{h,cpp}` —— 每个打开的文件一个 `ASTWorker`(每文件 AST + preamble 两条线程)、FIFO 队列、
  按物理核算线程数(`heavyweight_hardware_concurrency()`)、出队时取消/降级、`MaxRetainedASTs = 3`;
  <https://clangd.llvm.org/design/threads>
- `index/Background.{h,cpp}`、`index/BackgroundQueue.cpp` —— 线程池 `max(AsyncThreadsCount,1)`、
  按路径去重的优先队列、低优先级线程、进度统计;`index/{Index,MemIndex,FileIndex,Merge,Serialization}.h` ——
  查询路径上不建 AST,分片按声明文件存,RIFF 带版本;`index/BackgroundIndexLoader.cpp` —— 按 TU 的 shard 依赖闭包加载;
  <https://clangd.llvm.org/design/indexing>
- `Preamble.{h,cpp}` + `clang/lib/Frontend/PrecompiledPreamble.cpp` —— `CanReuse` 的三条判据
  (前缀逐字节 + `FilesInPreamble` 的 size/mtime + 仍然缺失)、`PreamblePatch`、preamble 边界 = 第一个声明前最后一个顶层 `#include`;
- `ClangdServer.{h,cpp}`、`support/{Threading,Context,Cancellation}.{h,cpp}`、`tool/ClangdMain.cpp` ——
  "方法不该阻塞"、`Semaphore` 限并发、`Context` 跨线程传递、取消的边界,
  <https://clangd.llvm.org/design/code>

**ccls**(cquery 一脉):`src/indexer.hh`(`IndexResult` = 每个文件一个 `IndexFile`,头文件在内)、
`src/pipeline.cc`(reader/writer/N 索引线程 + 四队列)、`src/sema_manager.cc`(每打开文件一个 `PrecompiledPreamble`)、
`src/query.hh`(`IndexUpdate::createDelta`)、`src/config.hh`(`index.threads` 默认"80% 核")。

**rust-analyzer / Salsa**:`docs/book/src/contributing/{architecture,syntax}.md`(`ItemTree` 不变式、
"hir 只存 `(FileId, TextRange)` 再重解析"、增量重解析"对 IDE 用处不大")、
`crates/base-db/src/lib.rs`(`DEFAULT_PARSE_LRU_CAP = 128`)、`crates/syntax/src/parsing/reparsing.rs`、
<https://salsa-rs.github.io/salsa/reference/algorithm.html>(backdating)、
<https://rust-analyzer.github.io/book/contributing/architecture.html>

**外部读数**:ISOCPP std-discussion(35 个标准头、51 次取中位,GCC 868 ms / Clang 995 ms,带 PCH 238/102 ms)
<https://lists.isocpp.org/std-discussion/2026/05/3369.php>;
GCC `-ftime-report`(parser 52% / name lookup 15% / preprocessing 7% CPU)
<https://gcc.gnu.org/pipermail/gcc/2010-May/192026.html>;
`-ftime-trace` 的粒度 <https://aras-p.info/blog/2019/01/16/time-trace-timeline-flame-chart-profiler-for-Clang/>。

**本机读数**(本轮实测,release):`cl /Zs` + `cl /E` 1.91 s、渲染 1 804 701 字节、闭包 109 文件 / 2 863 407 字节、
我们的 `workspace_probe`:`indexed 138 files in 26.198816s`;`parse_scale`:1.8 MB 词法 12.3 ms / 语法 264 ms。

---

## 11. 阶段 0 的读数(本轮实测):它否掉了阶段 1 的排序

`crates/cpp_code_analysis/src/stages.rs`(新)给每一段加了**互不重叠**的计时,`workspace_probe` 把它印成一张表。
两次运行,同一个工作区(release,本机):

```text
冷(缓存删掉后重建:reused 0 / rebuilt 194)       暖(全部命中:reused 137 / rebuilt 1)
indexed 138 files in 23.77 s                     indexed 138 files in 3.90 s
  [indexing]  20111 ms                             [indexing]  3862 ms   (占 99% 的墙钟)
     read         19                                  read        17
     hash          7                                  hash         6
     lookup        4                                  lookup      49
     parse       970  (4.8%)                          parse        3
     sweep     14842  (73.8%)                         sweep       10
     encode      186                                  bodied-scan 3776  (97.8%)
     bodied-scan 4083 (20.3%)
  明细(在 sweep 里面,不计入总数)
     scan         75 / scopes 169 / includes 49 / guards 3 / settling 9
     facts     14510   ← sweep 的 97.8%
       type-of  7487   ← declared_type_of
       returns  6925   ← declared_returns_of
       alias      22
       bases      11
```

**结论,一条:**

> **冷启动 23.8 s 里,14.4 s(61%)是 `build_facts` 里对**每一个绑定**调用
> `declared_type_of`(7.5 s)与 `declared_returns_of`(6.9 s);另有 4.1 s(17%)是 `re_read_where_a_body_decides`
> 的 `bodied-scan`。这两项加起来 78%,而它们**都不是"重复"**——单元读数省不掉它们。**

阶段 1(把 138 次 parse 变成 1 次)针对的是 **970 ms = 4.8%**。它仍然是对的(long-term 的正确形状、
并行、preamble),但它**不是 30 秒的答案**。§7 的第 5 条风险就是为这一刻写的:"读数不动就停下重新量,
而不是硬推后续阶段"——现在读数说话了,所以停下。

**两条可以立刻验证的假设**(下一轮先量、再改):

1. **`declared_type_of` / `declared_returns_of` 每个绑定一次从根下潜**:两者都是"从 root 沿脊柱走到
   binding 所在的那个节点",每一层都扫一遍兄弟(`children()` 找 `DeclSpecifierSeq`、再找 `Declarator`、
   再找"包含 anchor 的那个孩子")。这是**每绑定 × 每层兄弟数**,在声明成千上万的头文件里就是那个数量级。
   修法与 `Declarations::is_clean` 已经用过的那一招同构:**一次遍历建表,然后按 offset 查**。
   `xstring` 有 1637 条声明,这个形状值得先量一次"每个文件的绑定数 × 下潜深度"。
2. **`bodied-scan` 与"这一轮解析了哪些文件"无关**:它对**全项目每个宏**问一次形状问题,而暖启动里
   这一轮只 rebuild 了 1 个文件——4 s 花在决定"那 1 个文件要不要重读"上。它至少要能按"被解析文件提到的名字"
   驱动(把 O(全项目宏) 换成 O(被解析文件里的名字)),或者把形状判决**按文件缓存**(它是纯函数)。

**修正后的顺序**(仍然一次只做一项,各带门禁):

| # | 做什么 | 门禁 | 状态 |
|---|---|---|---|
| 1 | `declared_type_of`/`declared_returns_of` 的每绑定下潜 → 一次建表 + 查表 | 冷启动 **23.8 s → < 8 s**;declarations 的 20 条测试绿;四档语料读数不变 | **已做,见 §12** |
| 2 | `bodied-scan` 缩到"这一轮真正可能相关"的范围(或缓存判决) | 暖启动 **3.9 s → < 1 s**;冷启动再降 ~4 s | 下一个 |
| 3 | 阶段 1(单元读数):此时重复才成为主要项 | 剩下的时间里再砍一半 | |
| 4 | 阶段 2/3(单元并行 / preamble–suffix) | 同前 | |

第 1 项**不碰架构**,就在 `sema/declarations.rs` 里;这一条也说明为什么 §5 的顺序必须以读数为准:
如果先做阶段 1,我们会花掉一整轮重构、换来 4.8%。

---

## 12. 第 1 项做完了:`DeclarationShapes`(冷启动 23.8 s → 9.4 s)

四个函数(`declared_type_of` / `declared_returns_of` / `declared_alias_target` / `declared_bases_of`)原来各走一遍
"从根下潜到绑定、路上记下最后一个",而**下潜在每一层都要扫兄弟**,于是每个绑定一次 O(文件里的声明数)。
改成 `DeclarationShapes::of(root)`:一次遍历建表(每个有话说节点一条,连同它那三个孩子),查询变成
"二分找到包含这个 offset 的节点,然后沿祖先链往上走"——O(声明嵌套深度)。

**实测(同一个工作区,release,冷启动=缓存删掉重建):**

| 读数 | 改前 | 改后 |
|---|---|---|
| `indexed 138 files in` | **23.77 s** | **9.36 s**(两次冷跑:9.38 / 9.36) |
| `type-of` | 7 487 ms | **43 ms**(174×) |
| `returns` | 6 925 ms | **36 ms**(191×) |
| `facts`(sweep 全体的 97.8%) | 14 510 ms | **268 ms**(54×) |
| `sweep` | 14 842 ms | **577 ms** |
| 明细 `drop`(销毁语法树) | 没量 | **25 ms** — 猜它是那 3.5 s 的答案,**猜错了** |
| 暖启动 `indexed 138 files in` | 3.90 s | 4.08 s(不变,它由 `bodied-scan` 主导) |

**答案没有退化**(探针两张表逐条相同):`declarations_in("std")` 1959 → 2008、
`definition` 55 本文件 / 50 头文件 / 8 无此名 / 4 不可解析、`Ambiguous` 11 类同名同数、
`std::basic_string` 成员 202、`xstring` 1637 条声明 526 条带作用域。`cargo test -p cpp_code_analysis --lib`
**510 passed**。

**两个连带发现,下一个要量的东西:**

1. **`bodied-scan` 现在是最大的一块:4 183 ms = 全部阶段的 71.5%**,而且它在冷热两条路径上一样贵
   (暖启动 4.08 s 里它占 3 78 s)。它就是 §11 里说的那个形状:对**全项目每个宏**问一次形状问题。
2. **9.38 s 的墙钟里只有 5.85 s 被阶段框住**,差的 3.5 s **还没命名**。排除法:不是解析(897 ms)、
   不是销毁树(25 ms)、不是重读文件(17 ms)。剩下的候选是每次 `advance` 里没被计时的两处——
   `index_one` 的 `vfs.load`(正文 + 行索引)、`SummaryStore::get` 末尾的 `ProjectIndex::insert`
   (以及 `index_tree` 尾部那几个未计时的排序与 `FileScope`/`FilePreprocessing` 的销毁)。
   **下一轮的第一件事是给这两处各加一个阶段**,不是接着猜。
