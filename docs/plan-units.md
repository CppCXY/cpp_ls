# 以**编译单元**为工作单位(方案,待评审)

> **一句话**:我们不是算得慢,是把同一批字节算了一百多遍。把工作单位从"文件"换成"编译单元",下面每一层都跟着变简单,
> 而不是变复杂。

这份文档是**提案**,不是规范。`architecture.md` 已从仓库移除,设计契约以各模块顶部的文档注释为准;
本方案落地之后,结论写进相应模块的文档注释,这份文件删除。

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
| 2 | `bodied-scan` 缩到"这一轮真正可能相关"的范围(或缓存判决) | 暖启动 **3.9 s → < 1 s**;冷启动再降 ~4 s | **已做,见 §13**(暖启动 **270 ms**;冷启动不变,原因在 §13) |
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

---

## 13. 第 2 项做完了:第二遍 pass 的过滤器先问"这一轮谁会用到"(暖启动 4.08 s → 270 ms)

### 先量,再改

`bodied-scan` 拆成两段之后,答案比预想的干净(同一个工作区,冷启动):

```text
bodied-scan   4 185.6 ms
  bodied-plain    9.3 ms   ← 三万个宏体的"形状"判断本身几乎不要钱
  bodied-env  4 127.1 ms   ← 98.6% 是"形状说不清 → 建这个定义所在文件的闭包环境"
```

也就是说:**这个 pass 的钱全花在"给每个文件建一次闭包环境"上**(138 个文件 × ~30 ms),
而它的产物只是一个**过滤器**——`bodied`(哪些宏名值得重读),再拿去问"这一轮解析过的文件里,谁提到过它"。

### 改法:给过滤器加一层过滤器

`mentions_one_of` 是**整词**比较(`store.rs` 的 `words_of`:按非 `[A-Za-z0-9_]` 切分)。所以:

> 一个宏名,如果**这一轮要重读的那些文件的文本里根本没出现过**,它就不可能改变任何决定——
> 定义它的文件连问都不必问。

于是扫描之前先算 `mentioned` = 那些文件(只是那些文件!)的全部整词,循环里每个文件先做一次
`any(fact.kind.is_definition() && mentioned.contains(fact.name))`,不为真就 `continue`——**环境根本不建**。

**这不是近似**:`bodied` 只被 `mentions_one_of(text, &bodied)` 用过一次,而被这一层丢掉的名字,
按定义就是"要重读的文件里没有一个提到过它"。

### 读数

| | 改前 | 改后 |
|---|---|---|
| **暖启动** `indexed 138 files in` | 4.08 s | **270 ms**(15×) |
| 其中 `bodied-scan` | 3 776 ms | **168 ms** |
| 冷启动 `indexed 138 files in` | 9.30 s | 9.45 s(**没变**) |

冷启动没变,而且**这是应该的**:冷跑时每个文件都被解析过,`mentioned` 就覆盖了全部单词,
这一层过滤器什么也筛不掉。它是"**日常**"(开编辑器、改一个文件、重启一次)那一侧的钱:
一个文件被重读、它提到几百个词、于是只有定义这几个词的那几个文件被问。

暖启动剩下的 168 ms **全部**是 `bodied-env`——给那几个定义文件建环境,这是这个 pass 真正的工作,不是浪费。

**答案没有退化**:探针两张表与改前逐条相同(`declarations_in("std")` 1959 → 2008、`definition`
55/50/8/4、`Ambiguous` 11 类同名同数、`std::basic_string` 成员 202、`xstring` 1637/526、
`cook(<string>)` 1000 条声明 1 条只在展开后 / 0 unplaced / 2000 映射)。
`cargo test -p cpp_code_analysis`(全部 target)**全绿**。

### 冷启动那 3.5 s:**量出来了,是第二次 pass 的第二个循环**

先给 `index_one` 的 `vfs.load` 与 `ProjectIndex::insert` 各加了一个阶段——**两个都不是**(25 ms / 2 ms)。
顺手发现并修掉一个真的二次扫描:`mentions_one_of` 把每个词和**一个名单**线性比(`names.iter().any(...)`),
而它每个文件的每个词都要跑一次;改成 `HashSet` 之后这一段 **9 ms**(原来是它把 3.5 s 里的多少,见下)。
最后把第二次 pass 的两个循环分别计时,账就平了:

```text
墙钟 9 314 ms
阶段 5 798 ms(62%)   +   re-env 3 363 ms   =  9 161 ms  → 98.4% 有名字

  第二次 pass · 循环 1 的闭包环境(bodied-env)   4 040 ms   43%
  第二次 pass · 循环 2 的闭包环境(re-env)       3 363 ms   36%
  parse(194 个文件)                              899 ms   10%
  sweep                                          571 ms    6%
  encode + read/hash/lookup/load/insert          390 ms    4%
```

**结论:冷启动的 79% 是第二次 pass 在"每个文件建一次闭包环境",而且建了两遍**——
循环 1 为了问"这个宏体算不算一个读数会用的形状",循环 2 为了拿它重建那个文件。

我在这 3.5 s 上**猜错了三次**(销毁语法树 / `vfs.load` / `ProjectIndex::insert`),第四次是加计时器量出来的。
`§8 第 15 条`那句话这轮又验证了三遍。

**下一轮就是它,而且这次不用猜**:两处的环境都该来自**一条单元时间线**(`TranslationUnit::environment_of`),
而不是每文件一次闭包走查——这正是 §4 的目标结构。要做的那一层桥已经找到了:
`cpp_parser::shape_of_a_body_at` 收的**就是** `MacroBodies`(`&(impl MacroBodies + ?Sized)`),
所以单元时间线可以直接喂给它;缺的只有一步——`MacroBody` 是 token 序列、没有 `&str`,
所以要给 `MacroBodies::body_at` 的返回类型换成 `Cow<'_, str>`
(借的给 `MacroEnvironment`,拥有的给"从 token 拼出文本"这一侧),
`FileMacros` 再实现 `MacroBodies`。**这一步做完,7.4 s 就没了。**

预计:冷启动 **9.3 s → ~2 s**。

---

## 15. 第一座桥通了:循环 1 改吃**一条单元时间线**(冷启动 9.31 s → 5.46 s)

按 §14 的路线做的三步,都落地了:

1. **`cpp_parser::MacroBodies::body_at` 的返回类型换成 `Option<Cow<'_, str>>`**(`symbols.rs`)。
   理由是两种读者持有文本的方式不同:`MacroEnvironment` **有**文本、借出去;
   `UnitDefinitions` 只有 **token**(那是它在文件里有位置的东西)、得拼一个。`Cow` 是唯一不逼任何一方
   在每次调用上分配或复制的返回类型。改了三处 impl / 一处调用点(`sema/scopes.rs` 的 `&body`),零阻力。
2. **`impl cpp_parser::MacroBodies for FileMacros<'_>`**(`preprocess/cooked.rs`):把一条时间线的
   "这个位置这个名字是什么"接成形状读者的那一个问题,体文本由 token 按序空格拼出
   (形状读的是 **kind**——`namespace`/`::`/`{`/`}`——不是拼写,而体里唯一吃空白的那处
   `#define F(x) +x` vs `+ x` 不是任何读数会用的形状)。
3. **第二次 pass 的循环 1 改从单元取环境**(`store.rs` + `session.rs` 的 `units_for_the_pass`):
   `body_at` 一次问的是"定义文件在那个单元里的位置",于是**每文件一次闭包走查**没有了。
   工程文件优先当根(一个工程文件的闭包就是整个程序),已经被某个单元读到的候选直接跳过——
   这个工作区因此是 **1 个单元、1 次走查(97 ms)**。

| 读数 | 改前 | 改后 |
|---|---|---|
| 冷启动 `indexed 138 files in` | 9.31 s | **5.46 s** |
| `bodied-scan` | 4 162 ms | **21.6 ms** |
| 暖启动 | 270 ms | **155 ms** |
| 单元(`walk` + 取定义) | — | 114 ms(一次) |

**答案没有退化**:两张表与改前逐条相同(`declarations_in("std")` 1959 → 2008、`definition` 55/50/8/4、
`Ambiguous` 同名同数、`basic_string` 成员 202、`xstring` 1637/526、`cook(<string>)` 1000/1/0/2000 映射)。
`cargo test -p cpp_code_analysis --lib` **510 passed**,`clippy` 零警告。

**没有时间线的调用方**(探针从入口走一个闭包、测试)**行为一字不改**:`units` 为空时循环 1 退回原来那条
逐文件闭包走查(`index_includes_from` 传 `&[]`)。这不是脚手架,是"调用方手里没有时间线"这一档的读数。

### 还差第二座桥(剩下的 61%)

冷启动剩下的 5.46 s 里,阶段只占 1.90 s,`re-env`(**循环 2** 的逐文件闭包环境)仍是 **~3.36 s**。
挡住它的是同一个形状的第二个接口:`FileIndexer` 把环境交给 `ParserConfig::with_macros_from_includes`,
那里要的是 **`MacroFacts`(十个方法)** 而不是 `MacroBodies`(一个方法);`FileMacros` 现在实现的是本 crate 的
`MacroBindings`。所以第二座桥是 `impl MacroFacts for FileMacros`(或一个 `MacroBindings → MacroFacts` 的适配器),
之后循环 2 就能和循环 1 用同一个 `bodies_of`,**3.36 s 一起消失**。

预计:冷启动 **5.46 s → ~2 s**,暖启动 ~100 ms。

---

## 16. 第二座桥比预想的小:**`MacroView` 本来就是 `MacroFacts`**(冷启动 5.46 s → 1.93 s)

写 §15 时我以为第二座桥是"给 `FileMacros` 再实现一个十一方法的 `MacroFacts`",还担心
`body_text_of` 要 `&str`、而 `MacroDef` 只有 token。**翻了一遍才发现那个 impl 早就存在**:
`summary.rs` 里 `impl cpp_parser::MacroFacts for MacroView<'_>`,`body_text` 就存在时间线的事件里
(`event.body_text`)。也就是说"一条时间线"本来就是解析器要的那种证据,不需要新建任何东西。

真正要改的只有一处**类型体操**:`FileIndexer` 同时喂给两个读者——解析器要 `&dyn MacroFacts`(十一问),
作用域走查要 `&dyn MacroBodies`(一问)——而这两个 trait 对象之间**编译器不能互转**
(`MacroFacts ⇒ MacroBodies` 是 blanket impl,不是 supertrait)。改法是把构造器写成泛型:

```rust
pub fn with_macro_bodies<T: cpp_parser::MacroFacts>(mut self, bodies: &'a T) -> Self {
    self.bodies = Some(bodies);        // &dyn MacroBodies —— 作用域走查
    self.macro_facts = Some(bodies);   // &dyn MacroFacts  —— 解析器
    self
}
```
两次强制转换都发生在调用点(那里 `T` 是具体的:物化的 `MacroEnvironment`,或一条时间线上的 `MacroView`),
于是**解析和作用域走查不可能拿到两个不同的环境**——这本来就是 `FileIndexer` 收环境的原因。

**读数:**

| | 改前 | 改后 |
|---|---|---|
| 冷启动 `indexed 138 files in` | 5.46 s | **1.93 s** |
| 暖启动 | 155 ms | **155 ms** |
| 账目覆盖率 | 62% | **94.5%**(1 816 ms / 1 929 ms) |

冷启动现在花在哪:`parse` 895 ms(194 个文件自己的文本)、`sweep` 574 ms、`encode` 164 ms、
单元(`walk` + 取定义)110 ms,其余 ~80 ms。**没有一项是"重复"了。**

### 一条被测试抓住的回归(记下来,因为它是这类改动的标准形状)

我第一版让循环 2 **只认**单元:`units` 为空时既不做形状判断、也不给重建的文件环境。
`tests/scopes.rs::a_scope_a_macro_body_opens_holds_the_declarations_behind_it` 立刻红了——
它走的是 `SummaryStore::index_includes_from`(探针从入口走一个闭包,**背后没有会话**),
于是 `_STD_BEGIN` 开出来的 `std` 没了,`vector` 掉回文件作用域。修法是给循环 2 也补上
**"没有时间线"那一档**(`closure_environment`,就是改之前那段代码),两档并存、各说各的读数。
教训和 §5 阶段 1 写的一样:**换读者的时候,没有新机器的调用方必须原样保留旧机器**,否则它不会报错,
它只会答得更少。

---

## 17. "无损吗":一次 A/B(以及我先报错了一次)

性能改动是否改变了答案,不能靠保证,要靠 A/B。做法与读数:

1. 生成一份**新语料**(旧的清单不进仓库,已经没有了):MSVC STL 的 `include` 目录下 250 个头文件,
   **250 个文件 / 12 521 KB / 300 807 行**,各自当作独立文件读(无工具链,所以条件一律 `Unknown`)。
2. `git worktree add` 出 HEAD(d75a67d),用**同一个 target 目录**编出那一版的 `std_probe`,两版跑同一份清单。
3. 逐行 diff 输出。

**结果:除了计时行与新增的阶段表,两份输出一字不差。**

```text
HEAD:  files 250 | clean 149 | failing 101 | 12521 KB | 300807 lines
       index (parse + scopes + facts) 2.7196s | parse alone 2.3122s
现在:  files 250 | clean 149 | failing 101 | 12521 KB | 300807 lines
       index (parse + scopes + facts) 2.7196s | parse alone 2.3212s
差异行(去掉计时/阶段表):0
```

这条语料上新加的**声明形状表**值 `shapes 187.8 ms`(2 641 ms 里的 7%)——"一次遍历、之后每个问题都是查表"
在这个规模上的票价;它换来的东西在 §12(那个工作区上 14.4 s → 76 ms)。

### 我先报错了一次,记在这里

**A/B 的第一步我量错了对象。** 我先跑的是 `target\release\examples\std_probe.exe`,而它是**上一轮留下的陈旧二进制**
(这个 session 里从没编过它),拿到 19.14 s 就报了"7× 回归"。真正的两版都编出来之后,两边都是 2.72 s。
这正是 §8 第 15 条那一类错:**仪器的身份也是读数的一部分**——"我量的是哪个二进制"必须先说清楚,
否则一个数字就能把一整轮方向带偏。规则:探针的 A/B 必须**先编、再跑、并把版本写进同一行输出**。

### 还没有验的两条(诚实登记)

- **旧的四个语料**(455 / 255 / 109)的数字无法复现,因为清单没进仓库、临时目录也清了。
  这次是**新建**的一份 250 文件语料,它的数字记在 §17,以后用它对基线。
- **`--cooked` 那一档普查没跑**。这次跑的是裸读普查(§17 的读数)与单元流;熟读普查要另一条命令行。

---

## 18. 单元读数:能力建好了,**没有接线**(以及一次自己造出来的死循环)

### 建了什么

- `FileIndexer::index_unit_rendering(root, &RenderedUnit, key) -> IndexedUnit`:
  一个单元的渲染**解析一次**,把每条声明按 [`RenderedUnit::written_span`] 交回**它被写在的那个文件**;
  名字与范围落在两个文件里的事实**丢掉并计数**(半个文件坐标的事实不是"少一点",是错的)。
- `Session::read_the_unit(root) -> UnitReading` + `read_a_looked_at_unit`(一个 drain 读一个被打开文件的单元)
  + `read_a_unit_of_the_project`(显式要整个项目的那一档)。
- 测试:`a_unit_read_reads_the_whole_program_once` —— 它**明确调用** `read_the_unit`,
  所以机器有覆盖,而泵不走这条路。

### 为什么**不接线**:它让三个读数变差,而原因还没查清

```text
                                    不接单元读数   接上
drain 之后 declarations_in("std")        2008      1959
definition:在头文件里解析到                 50        46
definition:索引里没有这个名字                8        12   (std::size_t 是其中一个)
```

二分是确定的:把 drain 里那一行调用注释掉,三个读数全部回来,而本轮其它改动(第二遍 pass 吃单元时间线、
`want_cooked_reading` 的修复)都还在。所以成因就是这一行,不是懒读策略、不是时间线、不是那个修复。

**读代码之后,最显然的解释不成立**:`visible_declarations_upto` 先把**裸读**的事实收进去,
再跳过 `(name, kind)` 已被裸读说过的熟读事实——加熟读事实只可能**增加**候选。
下一步要量的东西写在 `Session::read_the_unit` 的文档里:`std_probe --cooked-index` 在这个工作区的闭包上跑一遍,
它会**逐文件**印出两种读数的声明数与差集,那是把"熟读事实不对"和"裸读事实动了"分开的那一次测量。

**没有解释清楚之前不接线** —— 用一个没解释的答案变差去换一个能力,是这份方案不做的那种交易。

### 顺手造出来一次死循环(教训)

我把 `unread_units()` 加进了 `pending_work()`,而**唯一会把它清零的那一步**(`read_a_looked_at_unit`)
恰好被我注释掉了。于是 `index_everything` 的 `while pending_work() > 0` **满速空转、永不退出**,
测试进程把一个核吃满、要人工杀掉。

规矩,写进 `Session::unread_units` 的文档里:

> **工作计数器只能数泵真的会做的工作。** 一个"关于泵不做的事情"的计数,会让每一个
> `while pending_work() > 0` 变成死循环 —— 而且不报错、不阻塞,只是把一个核烧掉。

这和 §8 第 11 条是同一个形状("一个只为某条路径存在的队列,必须对另一条路径不可见"):
**计数是承诺**,承诺了就得有人兑现。

---

## 19. "加熟读反而少 49 个名字"查清了:一个我们读不干净的文件,**在程序读数里不再只影响它自己**

§18 登记的谜团,这一轮量到底了。证据链四层,每一层都是一次测量:

```text
① 单元读数里 cstdio 的事实:   scope = Some("vc_attributes::std")        ← 名字错
② 编译器自己的渲染(cl /E):    "vc_attributes" 0 次, namespace std 60 次
③ 那个名字只在一个文件里:      CodeAnalysis\sourceannotations.h          ← §7 登记为"非缺口"的那一个
④ 流本身是平的:                braces: 0,而且那个名字空间明明开在该文件偏移 870 的文件作用域
```

**④ 是这一轮的关键**:我原本以为是"括号没关上",为此写了探测器(每个文件自己的 `{`/`}` 收支),
它**没有响** —— 因为那个文件的 token **是平衡的**。它平衡,而 walk 依然把后面的文件套在里面,
只能是**解析把它配对配错了**:等量的 `{` 与 `}` 并不保证解析器按文件的本意配对,**只有解析能保证**。

于是门换成了真的那一个:**渲染的解析有错误,就不落盘**。

```text
unit read: files: 0, tokens: 332755, unbalanced: [], braces: 0, errors: 26
declarations_in("std")  2008 → 2008        ← 什么也没变差(不设门时是 2008 → 1959)
```

### 这一轮真正的结论:那条"非缺口"的判断被推翻了

§7 一直记着:`sourceannotations.h` 是唯一读不干净的文件,"`/analyze` 专属语法,**已判定非缺口**"。

**在逐文件读数里那个判断是对的** —— 一个文件读不干净,代价止于它自己。
**在程序读数里它不成立**:它开的 `namespace vc_attributes` 在语法上没有关上,于是接在它后面的**每一个文件**
(整个标准库)都落进那个名字空间。`std::size_t` 因此从"在头文件里解析到"变成"索引里没有这个名字"。

所以:**一个文件的解析失败,现在是整个程序读数的问题**,不再是"那一个文件的事"。这条要写回 §7 的语料表
(那一族不再是"1 个文件 22 条消息",而是"程序读数的一道门")。

### 现在的状态(安全,但能力被门挡着)

- 单元读数:**建好、有测试、不接线**;即使接线,`errors != 0` 时它**什么也不写**,所以不会让任何答案变差。
- 顺带做对的一件事:`UnitReading` 现在报告 `files`(落盘了几个)、`braces`、`errors`、`unbalanced`
  —— 门为什么关着,读数是自己说出来的,不用猜。
- 探测器(每文件 `{`/`}` 收支)留着:它挡的是另一件事(真正词法不平衡的文件),有测试
  (`a_file_that_does_not_balance_its_braces_is_left_out_of_the_program`),而这次它不是答案。

### 下一轮的第一件事:**把那 26 条错误变成 0**,或者至少变成"不跨文件"

顺序(按代价):
1. **看一眼那 26 条错误分别是什么** —— 它们一共几族、有没有一条不是 `sourceannotations.h`;
2. `sourceannotations.h` 的 `/analyze` 语法(SAL 注解在声明里的位置)是**具体缺口**,不再是"非缺口";
3. 如果还要更稳:**渲染的解析一旦出错,就把出错点之后的事实标成"作用域不可断言"**,而不是整条读数作废
   —— 那是把门从"关着"改成"只关一半",代价是熟读事实要重新有 guard。

---

## 20. 我拉下去的那 27 秒:一个"从根遍历"落在**每绑定**的路径上

用户报的是"中午 Claude 重做性能、启动快了很多,下午晚上你继续做分析/类型检查之后又慢回去了"。
量出来的形状很干净:

```text
基线(修前)   indexed 138 files in 27.003 s
              sweep 68 081 ms → facts 67 613 ms → type-of 66 480 ms
              shapes 210 ms      ← 形状表本身只有 0.2 秒:钱花在表建好**之后**
```

元凶是 `declared_type_of_with` 里的一行:

```rust
if let Some(target) = using_alias_target(root, binding.name_range) { return Some(target); }
```

而 `using_alias_target` 的实现是 `root.descendants()` 全树遍历 + 过滤 `UsingDecl` + 取最小的那个。
**它每个绑定调用一次**,于是"一遍过"变成了"绑定数 × 节点数"。

**这是同一个形状第三次咬人**,三次都记在同一个文件里:

| 第几次 | 谁 | 读数 |
|---|---|---|
| 一 | `declarator_declaring` 从根走(Claude 量到并修掉) | 356 文件 2.9 s → 24.9 s |
| 二 | `declared_type_of`/`declared_returns_of` 从根下潜(§12) | 冷启动 14.4 s → 76 ms |
| 三 | `using_alias_target` 从根遍历(本轮) | 冷启动 **27.0 s → 1.17 s** |

修法沿用第二条的答案:形状表**已经**为这个问题记好了东西(`Shape.type_id` 就是 `using` 的 `=` 右边),
所以只把那一行换成"沿祖先链走一遍"。**`root` 参数随之变成未使用,被我删掉了** —— 那是个好信号:
它说明那处全树遍历是该函数里**唯一**的一处。

**读数(同一工作区、同一个探针、冷启动):**

| | 修前 | 修后 |
|---|---|---|
| `indexed 138 files in` | **27.0 s** | **1.17 s** |
| `type-of` | 66 480 ms | **139 ms** |
| `facts` | 67 613 ms | 1 308 ms |
| `sweep` | 68 081 ms | 1 838 ms |

答案不变:`declarations_in("std")` 1959 → 2008、`basic_istream/basic_ostream/basic_string` 成员 42/24/202。
`cargo test -p cpp_code_analysis`(全部 target)**全绿**(548 lib + 各集成测试)。

### 两条要记住的

1. **阶段表现在会超过墙钟**(`stages account for 4817 ms of the 1173.9 ms`)—— 因为 Claude 第三轮把
   `parse + sweep` 并行了,阶段时间是**各线程之和**。`stages total` 从这一轮起**不能再当墙钟读**:
   它是"哪一段",不是"多久"。这一条要写进 `stages.rs` 的文档。
2. **同形状的雷还剩一个,已量、未修**:`declarations.rs:740` 的 `declared_template_parameters_of`
   也是 `root.descendants()` + 过滤 `TemplateDecl`,**每个 class 绑定一次**。本语料上它只值 66 ms
   (`returns` 那一段),但 class 密集的头文件里就是下一个 66 秒。它的语义有讲究(模板声明与它引入的类是
   **兄弟**而不是父子,所以要看"父节点的 `DeclSpecifierSeq` 是否含这个偏移"),修它要把那一层关系也搬进形状表,
   所以**单独一轮**,不在本轮顺手改。

---

## 21. "做好索引"这条:Claude 的评审 §3.1 已经**过时**,而它要的东西已经在代码里

用户这一轮的吩咐是"做语义分析,做好索引"。先量了索引那条线,结论要先说清楚:

**§3.1(A3:`ProjectIndex` 缺少名字倒排)描述的状态已经不存在了。** 现在的 `ProjectIndex` 有:

```text
order: BTreeMap<u32, String>      + sequence: HashMap<String, u32> + next_sequence
names: NameIndex                  ← 倒排(见 crate::index::names)
files_defining_macro / files_declaring / symbols_matching   ← 都是表查询
```

读数(`index_scale`,5 000 文件 / 130 000 条声明 / 30 050 个不同名字,release,一次运行内对照):

| 查询 | 表 | 全扫 | 比值 |
|---|---|---|---|
| `symbols_matching("wid", 100)` | **42.6 µs** | 56.4 ms | 1 300× |
| `symbols_matching("e", 100)` | **184 µs** | 53.1 ms | 290× |
| `files_declaring("Widget7_1")` | **884 µs** | 3.2 ms | 3.6× |
| `declarations_in("ns7")` | **838 µs** | 3.1 ms | 3.7× |
| 一次编辑(forget + insert) | **285 µs** | — | — |

所以**不需要再做一遍**:门禁虽然还没达到(< 20 ms 早已满足,`files_declaring` 那一档比全扫只快 3.6×
是因为全扫本身只走可见闭包),但这条线已经不是瓶颈。

### 这一轮实际做了什么

1. **修掉了 27 秒的回归**(§20):`using_alias_target` 从"每绑定一次全树遍历"改成形状表查询。
2. **补上审阅点名缺失的那条语义测试**(§9.6 第 3 条:`using X = Y;` 这条路没有专门的测试):
   `a_using_alias_member_declares_the_type_after_the_equals` 与
   `two_using_aliases_in_one_class_keep_their_own_targets`。两条都钉**相邻**声明不被别名污染、
   以及同类里两个别名各归各的 —— 也就是我这次改动的两种失败方式。

### 一条测试写法上的教训(值得记)

我第一版把这两条测试写成问**声明本身**的类型(`size_type value;` 里 `value` 的类型),期望
`unsigned long`,结果得到 `size_type` —— **测试对着正确的代码红了**。原因不是代码:声明答的是**文件写下的拼写**,
穿透别名发生在**使用**那一侧(`w.value`)。所以问题问错了层,不是答案错了。

规则:**"这个名字是什么类型"有两个读者,问声明和问使用是两个问题**;写测试时先确认问的是哪一个 ——
这正是 §8 第 19 条("同一个事实有两个读者")的镜像:那次是两个读者给出不同答案,这次是一个读者被问了它不回答的问题。

### 下一步(按 Claude 的执行顺序,取其中价值最高的)

1. **`Ambiguous` 去重(2 034 次)** —— 语义线上最大的未做项;先量"这些候选里有多少其实是同一个实体
   (同名 + 同类 + 同一文件/同一模板的多次声明)",再定去重键。**先量再定**。
2. **A1 依赖作废判据**(`invalidate_dependents` 用编辑**前**的摘要判断是否定义宏,漏掉新增 `#define`、
   增删 `#include`):改成"指令事实前后是否相同",同一个函数还能复用于阶段 3 的边界规则。
3. **A2 缓存清扫**:缓存目录只写不删,每次指纹变化留一整套孤儿分片。
4. **A4**:`Session.units` 无上限、`units_for_the_pass` 的 `candidates.contains` 是 O(N²) —— 都是我这轮的代码。

---

## 22. `Ambiguous` 去重:我实现了,三条测试把我按住了 —— **规则是错的,撤回**

按 §21 的顺序做第 1 项,但我**先写实现、后量**,顺序反了。做法是给 `definitions` 加一条"同一个实体只留一条"的
收敛(判据 = 消费者能读到的每个字段都相同:`name/scope/local/kind/type_of/returns/bases/parameters`),
本意是让 `cin` 那种"一个头里写两遍"的名字不再 `Ambiguous`。

三条既有测试立刻红,而我读完之后认为**它们是对的、我的规则是错的**:

```text
two_declarations_of_one_name_are_a_list_for_a_consumer_that_shows_one
    /p/one.h: "int count;"      /p/two.h: "int count;"
    断言:definitions("count") 是**两条**的名单,顺序按文件
    理由(测试自己的注释):"`Ambiguous` 是'哪一个'的诚实答案,而能显示名单的客户端永远不必被告知它
                        —— **这两条都是真答案**"
```

三条各自否掉我的一个假设:

| 测试 | 我错在哪 |
|---|---|
| 两个头各写 `int count;` | **跨文件的两条不是"同一条记录两遍"**,是两个位置;藏掉一个就是少给一个答案 |
| `struct Widget` 在两个头里 | 一个类的**身份不在 `DeclFact` 里**(没有成员、没有基类时,两条看起来一模一样),所以"字段全同 ⇒ 同一实体"对类是假的 |
| 重载集 `f(int)/f(double)/f(char)` | 这条**没被我的规则破坏**(参数不同 → 不合并)✓,但它证明了这条规则的边界在哪:参数表是唯一真正能分辨函数的字段 |

所以真正的问题是:**`DeclFact` 里没有足够的信息去判断"同不同一个实体"** ——
函数要看参数表(有),变量要看类型(有),类要看成员(没有)。而 §9.3 那句话里的"同一声明被多个文件重复记录"
到底指哪一类,是**必须先量的东西**:得把 2 034 次 `Ambiguous` 按"候选之间**差在哪个字段**"分类
(0 个字段不同 = 真重复 / 只在参数上不同 = 重载 / 只在文件上不同 = 两个位置 / 差在类型或基类 = 真不同实体),
有了这张分布表才能定键。

**这一轮的状态**:`project.rs` 已 `git checkout` 回退,548 条 lib 测试全绿;
上一轮的回归修复与两条别名测试**保留**(那三处是 `declarations.rs` / `tests/types.rs`,与本次回退无关)。

**教训(这一轮第四次同一个)**:这一整天里"先写实现后量"把我按住了四次 ——
§11 单元读数先于账目、§20 的 27 秒、"我以为形状表是元凶"、和这次的去重键。
**先量再定不是流程装饰**:这四次里有三次是量出来的结论与我的判断相反。

---

## 23. `Ambiguous` 去重:量完之后规则自己出来了(19 → 12 个歧义标识符)

按 §22 的结论,先量"候选之间差在哪个字段"。量法是在 `workspace_probe` 里把每个歧义名字的候选**两两配对**,
按三档分类(同一个文件 / 跨文件 / 模型里什么都没记)。第一次分类器写错了 —— 它把"模型分辨不出"和"真是同一条"
混在一起,`std::getline` 的 6 对就跑进了"同一文件"那一档;按 kind 分开之后读数才对:

```text
19 个歧义标识符,候选两两配对:
   97 对:同一个文件里、且每个可读字段都相同(其中 getline 那类仍是"模型没记参数"的假相同)
    0 对:跨文件
```

**这两行直接决定了规则**,而且是三条各自独立的理由:

| 条件 | 为什么 |
|---|---|
| **必须是同一个文件** | 跨文件是**两个位置**:`int count;` 在两个头里是两条真答案,项目里已有测试钉住("这两条都是真答案");而且量出来跨文件的一对都没有 —— 重复**全在头文件内部**(MSVC 的 `<iostream>` 把 `cin` 写了两遍,一遍朴素、一遍在 `_EXPORT_STD` 后面) |
| **能分辨的字段必须"有值",而不只是"相等"** | 变量的**类型**、函数的**参数表**。两边都空不是一致:`std::getline` 在 `<string>` 的四个重载共享返回类型、而参数表这个模型没记 —— "它们相同"就是从沉默里编出一个身份,重载集会整片塌成一条。所以**参数表读不到的函数永不合并** |
| **其余字段全等** | `the_same_declaration`:只排除"写在哪"(`range`)与"读得多好"(`clean`/`guard`) |

类**故意不参与合并**:这个模型不记成员表,所以同名两个类既分辨不出、又确实不同。把 `struct S;` 与
`struct S { … };` 合成一条的不是这条规则 —— 那对在这个模型里本来就是**一条** `DeclFact`。

**读数(用户的真实工程,同一个探针):**

| | 改前 | 改后 |
|---|---|---|
| `std::cout` 的 `Ambiguous` | 4 处 | **解决** |
| `std::cin` 的 `Ambiguous` | 3 处 | **解决** |
| `definition` 答"在头文件里解析到" | 50 | **57** |
| 歧义标识符 | **19** | **12** |

剩下 12 个**全部**是重载集(`find` 12 条、`data` 12、`rfind` 9、`back` 7、`getline` 4、`eof` 4、`substr` 3、
`stoul` 2)与类模板+特化(`char_traits` 14)—— 正是模型不该合并、也合并不了的那些。

**这一条修的是代码自己抱怨过的那个症状**:`declared_type_of` 的注释里写着"`std::cin` 被声明两次,
所以每个用到它的地方都丢了类型:8 个偏移"。类型那一侧早就有了 `agreeing_type` 这个绕法,
**名字这一侧现在才跟上**,两侧终于一致。

`cargo test -p cpp_code_analysis` 全部 target 绿(548 lib + 集成),clippy 零警告。

---

## 24. 下一步的前提被量否掉了:"每次按键重新走查整个单元"**不成立**

§5 阶段 3 与 Claude review §6 第 3 条都写着"`buffer_changed` 每次按键 `units.clear()`,所以下一次熟读要把
整个闭包(138 文件)重走一遍 —— 最大的单点浪费"。按纪律先量再改,量法是现成的:`workspace_probe` 里
`did_open` → `index_everything` → 连做 10 次**函数体**编辑(`did_change` + `index_everything`),
`StageTimes` 在每个阶段上都有计数器。

```text
第一次:10 次函数体编辑 1.271 s 墙钟 | walk 0ns | closure 0ns | parse 4.6ms | sweep 2.6ms
第二次:10 次函数体编辑 0.556 s 墙钟 | walk 0ns | closure 0ns | parse 0ns   | sweep 0ns
```

**`walk` 两次都是 0** —— 单元**没有**被重新走查。`units.clear()` 确实在跑,但下一次
`translation_unit_of` 走的是**磁盘缓存**(`TranslationUnitCache`),而那条路今天没有付出一次走查。
所以"最大的单点浪费"这句话**在这份代码上不成立**:这条前提是 review 写代码之前的状态,和 §3.1/A2/A1 一样过时
(见 §21:那三条已实现)。

**能确定的**:一次按键的代价是 **55–127 ms**(两次读数相差一倍,冷热缓存之别),`parse`/`sweep` 只在第一次
非零(4.6 + 2.6 ms),所以钱不在解析上,也不在走查上。

**不能确定的**:花在哪一段。两次读数不一致,而且探针里那张阶段表印在**普查之后**、我的编辑循环之前——
**instrument 的时间窗口没罩住被测的东西**,这是 §8 第 15 条那个病的同一个形状(插桩插在候选怀疑对象上,
量出来的却是别人的时间)。

### 收紧窗口之后的归因(同一天,同一条命令)

修法是把循环写成**两遍**:第一遍热身(结果丢掉),第二遍才计时,并且印**整张**阶段表而不是四段。

```text
10 次函数体编辑(warm,只量第二遍):阶段合计 45.7 ms —— 即 4.6 ms/次
   encode 14.6(32%)| parse 5.3 | bodied-scan 4.0 | render-parse 4.0 | read 3.9
   include-scan 3.1 | sweep 2.4 | render-sweep 2.4 | index-insert 0.4 | 其余 <1
   walk 0 | closure 0        ← 单元没有被重新走查
```

**结论:一次函数体按键 4.6 ms,最大的一项是把编辑后文件的摘要写盘(1.5 ms/次)** ——
既不是单元走查(0),也不是解析(0.5 ms)。而上一轮那两个 `0.556–1.271 s` 是**冷跑**:
**一个随缓存状态变化的数不是对代码的测量**。这条和 §2.1 记的"仪器先修好再读"是同一件事,
只是这次错在我自己的探针里。

所以"每次按键是最大的单点浪费"这条**彻底不成立**,阶段 3 的边界规则**不是**为了性能而必要;
它剩下的价值只在语义那一侧(见 §5 阶段 3 的原意:一次函数体编辑不该让任何**别的**文件的读数作废),
而那需要单独量一次"编辑后有多少**依赖者**的读数被丢掉",不是拿按键延迟当理由。

**所以这一轮不实现那个优化**:前提被量否掉之后,先做的是**把窗口收紧**(在编辑循环前后各取一次
`StageTimes`,并把两次读数打出来),再看 55–127 ms 落在哪一段;归因之后才谈改不改。

**今天第五次"先量再定"否掉了计划自己的判断**(前面四次:§11 单元读数先于账目、§20 的 27 秒、
"形状表是元凶"、§22 的去重键)。这五次里有四次,量出来的结论与写到计划里的判断**相反** ——
这不是纪律的装饰,是这份文档最该有的那一半。

---

## 25. A4 也过时了:我实现了它,编译器才告诉我 `UnitTable` 早就有上限

这一轮的清单上写着 review 的 A4(§3.2:`units` 没有上限)。我照着写完了才去编译,得到:

```text
error[E0599]: no method named `len` found for struct `UnitTable`
error[E0599]: no method named `remove` found for struct `UnitTable`
```

**`self.units` 不是 `HashMap`,是 `UnitTable`** —— 而它从 [117ec18] 起就是一张**有上限的 LRU 表**:

```rust
const MAX_UNITS: usize = 16;
struct UnitTable { held: HashMap<String, (u64, Arc<TranslationUnit>)>, clock: u64 }
// insert(): 不在表里且已满 16 时,先淘汰 `used` 最小的那一条
```

我加的那份是 `MAX_HELD_UNITS = 8` + **按 `HashMap::keys()` 任意顺序**淘汰 —— 比已有的更小、且把
LRU 换成了随机。**它一次都没跑过**(编译不过),已 `git checkout` 撤回。

所以 A4 的状态和 A1/A2/A3(§21)一样:评审的**风险表**(第 361–364 行)写的是改动**之前**的状态,
而它自己的**修复表**(第 19 行)已经把这三件事都列为"已做"。**看风险表要先看修复表** —— 这条今天用掉了
三次,这是第四次。

**没做的一件小事**:`MAX_UNITS` 这条上限**没有测试**。写它的测试要造 17 个真单元(17 个闭包),
代价不划算;它的边界规则(`layout` 变了才清 `units`)由 `a_unit_survives_typing_in_a_body_and_not_a_change_to_a_directive`
挡着。登记,不修。

### 顺带发现:围栏早就建好了,而且**接进了** `read_the_unit`

§19 的"下一轮第一件事"里第 2、3 条已经不存在了 —— 代码里(117ec18 起)有:

```text
brace_crossings(tree, stream)        ← 找出**跨两个文件**配对上的括号
RenderedUnit::without / ::only       ← 把泄漏文件的 token 抽出去,再单独读它
gate: if indexed.crossings == 0      ← 门已经不是"errors != 0",而是"还有没有跨文件的配对"
UnitReading { quarantined, crossings }  ← 读数自己报告它隔离了谁、还漏不漏
```

两条测试挡着它:`a_file_whose_scope_leaks_is_read_alone_and_the_program_is_still_read`、
`a_program_whose_files_keep_their_scopes_quarantines_nothing`。所以 **§19 第 3 条("渲染解析出错就把出错点之后
标成作用域不可断言")不是待做项,而是已经做过的另一条路**:不是"出错就不落盘",是"**跨文件的配对**才不落盘,
漏的那一个文件拿出来单独读"。这比原提案更好,理由是它**只**隔离真正的病(§19 ④:token 是平的,配对才漏)。

**但接线开关还关着**(`session.rs:1219` 的 `// self.read_a_looked_at_unit();`),理由写的是**围栏之前**那次测量
(2008 → 1959、`std::size_t` 从"在头文件里"变成"索引里没这个名字")。那条理由的前提——"不知道 49 个名字
为什么消失"——已经在 §19 查清并被围栏修掉了,所以这一轮的活是:**用围栏后的代码重测那三个读数**,
对了就把开关合上。量它的工具也已经在库里:`examples/unit_read.rs`(逐文件读数 → 单元读数 + 围栏报告 → 再读数)。

### 库代码里留着一台仪器(已删)

`index_unit_rendering` 里留着两处调试输出,是 15a40b0 提交进来的:

```rust
eprintln!("TMP round {round}: ...");          // 每次单元读数,每一轮解析
for k in 1..=20 { ... CppParser::parse(&program.text[..cut]) ... }   // 把整个程序重解析 20 遍
```

第二处不是打印,是**把 1.8 MB 的流按 20 个前缀各解析一遍**,每次单元读数都跑。删掉了(14 行),
**删之前没有跑它**:它回答的是"解析代价落在流的哪一段",而现在没有任何决定依赖这个数(单元读数没接进泵,
按键代价见 §24)。它属于例子、不属于索引路径 —— 这一族("实验仪器最终住进了产品")今天第三次出现(§8 第 15 条)。

**今天第六次:读代码推翻了计划里的判断**(A4 不存在)。

### 围栏之后的重测:三个读数变好了,而**代价**是十倍 —— 现在修掉了

`examples/unit_read.rs`(它本来就是为这个问题写的)在同一个工作区上,围栏之后的读数是:

```text
per file   declarations_in("std") =  1959 | std::size_t Unknown(NotDeclaredHere) | std::string in xstring
unit read in 12.15s: 138 files filed, 332755 tokens, 0 missing, 0 unplaced
  errors 26 | crossings 0 | quarantined [sourceannotations.h, type_traits, atomic, memory] | braces 0
with unit  declarations_in("std") =  2452 | std::size_t Unknown(Ambiguous)          | std::string in xstring
```

**"加熟读反而少 49 个名字"已经不成立了** —— 现在是 **+493**(1959 → 2452),`std::string` 依然在 `xstring`。
`crossings: 0` 且隔离了 4 个文件(§19 只认出 1 个:`sourceannotations.h` 不是唯一的泄漏源)。
`std::size_t` 从"这个文件里没有这个名字"变成 `Ambiguous` —— 两个读数**都是"不知道"**,但后者说的是
"索引里有好几个 `size_t`",这一条要单独查(见下)。

**新问题是代价:12.15 s**,而 per-file 那条路整个冷启动才 1.17 s。阶段表说钱花在哪:

```text
[cooking] 10220 ms = render-parse 971 + render-sweep 9249
   detail: facts 9108  ← 而 type-of 67 + alias 0.4 + returns 39 + bases 3 + shapes 87 ≈ 197
```

**`Facts` 里 8.9 s 没有归属** —— 因为 `fact_for` 里唯一没有被计时的那件事就是它:
`declared_template_parameters_of` 对**每一个类绑定**做一次 `root.descendants()`。在 per-file 的树上是 66 ms,
在单元读的树上是 332 755 个 token × 几千个类绑定。**这是同一个病的第四次**(§20:`declarator_declaring`、
`declared_type_of`、`using_alias_target`,加上登记未修的这一个)。

修法不是加缓存,是**把那条关系放进 `DeclarationShapes` 的那一趟**:`template <…>` 与它引入的类是**兄弟**,
中间那层 `DeclSpecifierSeq` 自己不是 shape,所以遍历时顺手把 `(引入的 specifier 范围, 参数表节点)` 记下来
(§20 修 `using_alias_target` 的同一个手法)。查询因此变成"扫这个文件的模板声明",语义**逐字保持**:
按文档序取**第一个**包住这个名字的 —— 嵌套类模板取到的是**外层**那个,和改之前一样(那条我没动,登记)。

```text
                    改之前        改之后
unit read          12.15 s       3.08 s
  render-sweep     9249.1 ms     482.6 ms
    facts          9108.0 ms     335.9 ms
    template-params  (未计时)      1.8 ms      ← 顺带加了这个 detail 阶段,否则它永远藏在"其余"里
三个读数          1959→2452      1959→2452    ← 一模一样
```

**代价变了,答案没变** —— 这才是想要的形状。548 + 全部集成测试绿。

### 接线这道门:**四项里有三项变好,第四项是"另一种不知道"**,而代价是 +3.2 s 启动

`examples/unit_read.rs` 现在把**同一个进程里、读数前后**的 `definition` 普查也打出来(`Ambiguous` 按名字列出)。
这是"总量会盖住'一个文件赚了另一个文件亏了'"那条纪律要的形状:

```text
                     per file    with unit
resolved here            55          55
resolved in a header     53          56     ← +3
the index has no such name 12          8     ← −4(四条从"没这个名字"变成能解析)
UnparsableName            4           4
Ambiguous(用途计数)       14          15     ← +1
   新增的那一个:std::size_t —— 它以前是"没这个名字"
declarations_in("std") 1959        2452     ← +493
```

**围栏之前那条"答案变坏"的理由已经全部消失**:`std` 名字是**多**了 493 个,不是少 49 个;
四个找不到的名字变成找得到;唯一变动的 `Ambiguous` 是 `std::size_t`,而它**以前也是"不知道"**
(`NotDeclaredHere`),现在只是把"不知道"换成了"索引里有两个 `size_t`"。按项目的规则(两个文件各声明一次
= 两个答案,`two_declarations_of_one_name_are_a_list_for_a_consumer_that_shows_one` 钉着),这是**规则的正确输出**,
不是缺陷;跨文件"同一个实体"的合并是 Claude 清单上 P0 的另一条(cross-file references)。

**所以这道门过了。但开关这一轮没合**,理由是代价换了个位置:

```text
单元读数            3.18 s(修完模板参数之后)
  render-parse       912.8 ms     ← 把 1.8 MB 的流解析一次,这是这一层的地板
  render-sweep       530.8 ms
  其余               ~1.7 s       ← **没有阶段覆盖**:cook_the_unit 的渲染、按文件分摊事实、流本身
per-file 冷启动      1.17 s       ← 接线之后第一次 drain 会变成 ~4.4 s
```

这一次不是"答案不对",是**启动时间**:用户这一轮刚刚说过启动性能是重点(Claude 那半天做的就是这个),
拿 +3.2 s 的首屏换一批头文件的熟读事实,是**产品取舍**,不是我能替用户定的。
而且没有阶段覆盖的那 1.7 s 是同一个病的**第五次**(§24:插桩插在候选怀疑对象上,量出来的却是别人的时间)。

**所以下一件事**:把单元读数剩下那 1.7 s 归因(`cook_the_unit` 之后到落盘之间),再决定接线还是改成
"某个请求点名了头文件才读这个单元"—— 后者已经有先例(`want_cooked_reading` / `want_the_closure_cooked`
就是把"整个闭包"收窄到"打开的文件 + 被点名的文件"的那次测量)。

### 那 1.7 s 归因完了:它就是"把整个程序读一遍",而阶段表现在罩得住整个读数

三个新阶段(都在 `units` 家族里,和 `walk`/`closure` 并列):`unit-render`(`cook_the_unit` 的 lex + 宏展开 + 拼接)、
`unit-fence`(`brace_crossings` + `without`/`only`)、`unit-files`(按文件分摊事实)。

```text
unit read 2.67 s(同一台机器,这次运行)
  [units]     1452.1 ms  56.6%
    unit-render  1302.8   ← 138 个文件、332 755 个 token 的 lex + 展开 + 拼接
    unit-fence    142.8
    unit-files      6.5
  [cooking]   1111.6 ms  43.4%
    render-parse  694.0
    render-sweep  417.6
  stages total 2563.8 ms ≈ 2.67 s 墙钟(96%)      ← 之前是 1.44 s / 3.18 s(45%)
```

**没有"浪费"可以再砍了**:最大的两项就是编译器也要做的那两件事 —— 把 2.86 MB 的闭包预处理成 1.80 MB 的流
(1.30 s),再把流解析一遍(0.69 s)。同一个单元上 `cl /Zs`(语法+语义)+ `cl /E`(预处理)**各一遍 = 1.91 s**,
也就是说**整个单元读数(2.67 s)是编译器自己前端代价的 1.4 倍**,而它比编译器多做的事是:每个事实映射回它被写下的
文件、跨文件括号围栏、把熟读事实按文件落进索引。这不是"慢",这是**这件事本身的价钱**。

### 决定:**不接线**,并把 `session.rs:1213` 那条**已经变成假话**的理由换掉

留一个过时的理由在代码里,就是这个 session 里反复抓到的那个病(§21/A1–A4、§25 开头)。那条注释今天还在说
"接了线 `std` 少 49 个名字、`std::size_t` 从解析到变成没这个名字" —— **现在两句都是假的**(+493、`Ambiguous`)。
所以换成了量出来的那一条:

> 代价:2.7 s,而它是**一个不可分的步骤**(渲染 + 解析 + 落盘),放进 `advance` 的 idle 分支就是**握着 writer
> 等 2.7 s** —— 首个 drain 之后每一次打开文件的空档都会这样。这正是 `want_the_closure_cooked` 当初被收窄要保护的那笔
> 预算(11.7 s/启动),而它换来的东西,按需熟读(`want_cooked_reading`)对**被点名的**文件已经能拿到。
> 所以能力是**被请求的,不是被排程的**:`read_the_unit` / `read_a_looked_at_unit` 是 API,`units_read` 保证
> 每个文件每次指令变动只读一次。

**这条决定本身是可推翻的**,而且推翻它的条件是清楚的:把"一次不可分的 2.7 s"变成"每次 `advance` 一片"
(比如按 frame 分片拼接),或者证明首屏那 2.7 s 买到的答案比 `want_cooked_reading` 多得多。两条都还没量。

### 顺带关掉一个"登记未修":嵌套类模板的参数表取错了那一个

改这一趟时我**故意**把语义保持原样(文档序取第一个 = 最外层),并记了一句"嵌套类模板取到外层,登记"。
现在用一条测试把那条登记变成事实:

```rust
template <class _Ty> struct outer { template <class _Uty> struct inner { _Uty u; }; };
// template_parameters_of("std::outer::inner")
//   改之前:["_Ty"]    ← 外层那个,配对的是 outer 的参数
//   改之后:["_Uty"]   ← 引入 inner 的是内层那个
```

测试 `a_nested_class_template_declares_its_own_parameters` 先跑出来 `left: ["_Ty"]`,再改成取**最内层**
(表按文档序,最后一个包住名字的就是引入它的那个)才绿。**旧的那次 `root.descendants()` 走查也给不出正确答案**
—— 它取第一个,也就是最外层 —— 所以这是**关掉了一个真缺陷**,不是这次重构引入的。

真语料上它是**惰性的**:同一个工作区重跑 `unit_read`,三个读数和普查一个字没变(1959/2452、53→56、12→8、
`size_t` 仍是 `Ambiguous`),说明这份闭包里没有嵌套类模板 —— 也就是说这条修复今天赚不到钱,但它有测试,
而且下一次有人写嵌套模板时不会再错。

---

## 26. 用户点名的三件事:"语义分析啥都没做" —— 逐条量,三条里两条是**我们的**,一条是**配置**

用户的原话:`<format>` 补全里没有 `format`、没有参数列表、hover 也没结果;`private` 这些作用域没搞好;
`auto` 没有推断。"你语义分析啥都没做就来搞重命名合适吗" —— 合适不合适不用争,把三条各量一遍。

量它的工具是新的 `examples/editor_probe.rs`:**把用户的文件当缓冲区打开**(不碰磁盘),前面加一行
`#include <format>`,后面加几行问题代码(`auto`、`std::format(...)`、`std::string::`),再打印
补全 / hover / signature 三样东西的答案,**并且在每个答案旁边打印索引自己知道什么** ——
"补全是空的"和"这个名字根本不在索引里"是两个缺陷、一个症状。

### ① `<format>`:编译器自己说这个项目里 `std::format` **不存在**

```text
cl /nologo /Zs /EHsc format_probe.cpp                    (项目现在的配置:没有 /std:)
  format(43): warning STL4038: The contents of <format> are available only with C++20 or later.
  format_probe.cpp(5): error C2039: "format": 不是 "std" 的成员
cl /nologo /Zs /EHsc /std:c++20 format_probe.cpp         → exit=0
```

这个项目里没有 `CMakeLists.txt`、没有 `compile_commands.json`,`.vscode/settings.json` 是 **2 字节**,
所以没有任何 `/std:` —— MSVC 的默认是 C++14,而 `<format>` 的全部内容在 `<format>(42)` 的
`#ifndef __cpp_lib_concepts` 之后。**分析是对的**:`definition("std::format")` 答 `NotDeclaredHere`,
和编译器答 `C2039` 是同一句话。

索引里的证据也摆着:`.cppls` 之外,`include/format` 的 **raw** 摘要 1109 条声明(逐分支读数,里面有
`std::format_error`、`std::formatter`)、**cooked 读数 0 条**(照编译器看,这个头是空的)。两者都对。

**所以这一条要改的不是补全,是"用户凭什么知道"**:项目没有构建配置时,分析用的是**编译器默认标准**,
而这一点今天没有任何地方说出来。这是登记项(见文末),不是接线项。

### ② 补全里"没有 format"的**真原因**:列表在**文件顺序**上被砍,而客户端被告知"完整"

量出来的三件事,一条比一条具体:

```text
① std:: 补全:175 个名字,truncated **false**
   带 names: cin / cout / getline / make_optional / nullopt / stod …
   不带 names: string、basic_string、size_t —— 而索引里**有**这三个
② std::string:: 补全:**0** 个名字(而 members_of("std::basic_string") = 202)
③ 那 175 是怎么来的:declarations_in_where 的预算是 MAX_COLLECTED_NAMES = 400,
   按**可见文件的遍历顺序**花掉;用户的直接包含是大头,<xstring>(string 所在)排在后面
   → 400 花完就 break,后面再有什么名字都进不来
```

于是 `crates/cpp_ls/src/handlers/completion/mod.rs` 里那句

```rust
is_incomplete: session.pending() > 0 && !found.truncated,     // 旧
is_incomplete: session.pending() > 0 || found.truncated,      // 新
```

是**这条投诉的另一半**:列表被预算砍到 200 条,**却告诉客户端"这是完整的"** —— 于是 VS Code 不再发请求,
只在收到的 200 行里本地过滤,用户输入 `std::str` 得到**空**。而前缀过滤是在**索引层、克隆之前**做的,
同一个请求带上 `str` 会精确答出 `string`。旧的注释写着"被砍的列表再问一次还是同一个列表",这句只在
**客户端用同样的前缀再问**时成立,而没有客户端这么做。

三处改动:

| 改什么 | 在哪 | 量出来的效果 |
| --- | --- | --- |
| 一个**具名作用域**用新的预算 `MAX_COLLECTED_IN_A_SCOPE = 4096`(全局名字空间仍是 400) | `index/project.rs::declarations_in_where` | `std::` 收集 2146 条、送 200 条、`truncated: true`;`size_t` 回到列表里 |
| 限定名指向**类型**时问 `members_of`(别名、模板拼写、基类都在里面) | `index/project.rs::names_in_a_scope` 的 `None` 分支 | `std::string::` **0 → 63** 个成员,带 `_CONSTEXPR20 void (…)` 这样的 detail |
| 被砍的列表要如实报 `isIncomplete` | `cpp_ls/handlers/completion` | 输入 `std::str` 由"空"变成命中 |

两条新测试(**先证明它们会红**):`a_name_in_a_late_file_of_a_namespace_is_still_offered`
(旧预算下:420 个名字,`zebra` 恰好被砍掉 —— 红)、
`the_members_of_a_class_template_reached_through_an_alias_are_offered`。

### 我第一版改错了:一条"能用"的假修复

第一版是"先问 `members_of`,失败再退回 `declarations_in`"。它让 `std::string::` 从 0 变 63、让 `std::`
从 175 变 200,**看起来全对了** —— 但测试在旧预算下**照样绿**,这才露了馅:

```text
OFFERED 421 | fillers 420 | … zebra        ← 400 的预算根本没生效
```

因为 `members_of` **对名字空间也照样回答**(它列的是"scope 等于这个名字"的声明),所以每一个
`std::`、`ns::` 限定符都被送进了**没有预算、没有隐藏去重**的成员查询。改成先问索引
`definition(spelling).kind == DeclKind::Type`(名字空间是 `DeclKind::Namespace`)之后,测试立刻红了:

```text
with the old 400 budget:  420 names offered, none of them `zebra`   ← 复现
with MAX_COLLECTED_IN_A_SCOPE:   421 names, `zebra` last            ← 修好
```

**"先量再定"今天第七次推翻了我自己的判断** —— 这一次推翻的是我上一小时刚写下的"修复"。

### 还没做的三件(用户点名的后两件 + hover 的一个)

1. **参数列表根本不在事实模型里**:`DeclFact.parameters` 是**类模板**的参数名,函数的形参一个字段都没有,
   所以 `detail_of` 只能写 `returns (…)` —— 补全详情里那个 `(…)` 是真的空,不是显示问题。
   跨文件 signature help 也一样要有它(`signature.rs` 今天是现场解析声明所在文件,一个请求一次解析)。
2. **`auto`**:`auto y = x.back()` 现在答 `_NODISCARD _CONSTEXPR20 reference` —— 既没剥掉库自己的宏,
   也没把 `reference` 换成 `char&`。初始值表达式那一步是通的(`x` 答 `std::string`),卡在**类型拼写**上。
3. **`private` / `protected` 今天完全没有建模**:没有任何地方记录成员的访问级别,所以补全和解析都不会过滤。
4. 顺带:`type_at("std::string::size_type")` 仍是 `UnknownType`(限定名穿过别名的那条路,和 ② 是同一个病,
   但走的是 resolve 而不是 completion)。

顺序:参数列表(用户点了两次,而且是 `(…)` 的直接原因)→ `auto` 的类型拼写 → 访问级别 → 限定名解析。

---

## 27. 内建默认配置:没有构建配置时,**以编译器支持的最新标准**去问它,并把这件事说出来

用户的吩咐:"如果目标工作区没有 `.cppls.toml` 和 `compile_commands`,我们可以内建一个默认配置,并以最新的
C++ 标准去访问编译器获得对应的宏变量"。

### 改之前的链条,以及它为什么会给出一个"没人要的答案"

```text
discover_with → standard_for(commands, for_file) → 没有数据库 = **None**
             → cl 被问的时候没有任何 /std:  → 它答自己的默认 = _MSVC_LANG 201402L
             → <format> 的 #ifndef __cpp_lib_concepts 不成立 → 整个头是空的
```

也就是说:**"没有配置"被翻译成了"编译器默认",而编译器默认是 C++14** —— 一个谁也没要求过的标准。
更糟的是另一头:`.cppls.toml` 里写的 `-std=c++17`(或 CMake 的 `CMAKE_CXX_STANDARD`)**根本不会传到
编译器那一次调用**里(`standard_for` 只读数据库),于是宏表答的还是默认标准,而
`compilation_environment` 又把配置推导出来的 `__cplusplus` **盖在**编译器自己的宏表上。

### 三处改动

| 改什么 | 在哪 |
| --- | --- |
| 没人说标准时,按"编译器能接受的最新"去问:`/std:c++latest`(MSVC)、`-std=c++23`→`-std=c++2b`→不带参数(GCC/Clang) | `include/toolchain.rs`:`ASSUMED_STANDARD_MSVC` / `ASSUMED_STANDARDS_GNU` + `standards_to_try` |
| 项目自己说的标准(数据库**或** `.cppls.toml`/CMake)一定传到编译器那一次调用 | `BuildStatement { commands, standard }`(原来 8 个参数,clippy 顶回来之后按项目的老规矩收成一个类型) |
| **编译器自己的宏表赢过配置推导出来的那张表** | `index/environment.rs::compilation_environment`:两段换位 |

第三处是这个功能的**前提**,不是搭头:配置那张表是"从 `-std=` 拼出来的",它给 `c++23` 写的是 `202302L`,
而这个 `cl /std:c++latest` 自己答 `202100L`(g++ 11 的 `-std=c++23` 也答 `202100L`);
**旧顺序会让 `#if __cplusplus >= 202302L` 答出一个自信的 true**,而编译器答 false —— 这是这一层唯一不接受
的失败方式。换位之后配置的表退回它本来的位置:**编译器没被问成时的兜底**。

### 读数(用户的工程,release,同一个进程里前后对照)

```text
standard = Some("c++latest") | toolchain Some("c++latest")
note     = "no build configuration states a language standard (no compile_commands.json, no CMakeLists.txt,
            no .cppls.toml), so the compiler was asked for its newest (`c++latest`); a project that builds
            as an older standard says so in .cppls.toml"

include/format 的熟读读数          Some(0) → **6** 条(名字现在真的在索引里)
definition("std::format")          NotDeclaredHere → **Ambiguous**(四个重载,诚实)
std:: 补全                         **format / format_to / format_to_n / formatted_size / vformat 都在**
std::string:: 补全                 0 → **64**
```

"Ambiguous 四个重载"不是失败:那个名字现在**存在**了,而"该用哪一个"是下一节的问题(签名与参数列表)。

### 顺带把"默认配置"变成了**看得见的**东西

一个内建默认值最怕的是"用户不知道自己在用什么标准"。现在它有三条出口:`Toolchain::standard`(报告里读数)、
`Toolchain::note`(上面那句话,写在一行里)、`session.config().standard`(分析真正在用的那个);
要覆盖它,就是 `.cppls.toml` 的 `[compile] args`,和覆盖别的任何东西一样。

**两条新测试**:`a_project_that_states_no_standard_is_asked_for_the_newest`(含 `__cplusplus` 落到配置里)、
`a_standard_the_project_states_is_not_replaced_by_the_assumption`。552 + 全部集成测试绿,clippy 干净。

---

## 28. 参数列表进了事实模型:补全里那个 `(…)` 原来是**真的空**

用户点名的第二件("也没有他的参数列表")。查下去发现比"显示得不好"严重:`DeclFact.parameters` 是**类模板**的
参数名,**函数的形参一个字段都没有** —— 所以 `detail_of` 只能写 `returns (…)`,那个 `(…)` 不是省略号,是空白。

### 改动

| 改什么 | 在哪 |
| --- | --- |
| `DeclFact` 多一个字段 `parameter_list: Option<String>`:函数声明时写的形参表,**连括号**,按文件拼写 | `summary.rs`、`summary_codec.rs`(CODEC_VERSION 17 → **18**) |
| 读法:名字所在位置**最内层 `Declarator` 祖先**的 `ParameterList` 子节点 —— 和 `inlay::parameter_list_of` 同一个读法(`void (*f(int a))(int b)` 有两个表,函数体里的变量上面还挂着外层函数的表) | `sema/declarations.rs::parameter_list_at`(由 `fact_for` 和 `fact_from_binding` **两条生产者**都填) |
| 布局被抹掉:`<istream>` 的 `getline` 原本带着 `\r\n` 和四个空格缩进,熟读读数还带着渲染器的"每 token 一个空格"(`format_string < _Types ... >`) | `inlay.rs::parameter_list_text`:空白折叠成一个空格,`(` 后 / `)` 与 `,` 前不留空格;`<`、`>`、`...` **故意不动**(`(bool _B = 1 < 2)` 也是参数表) |
| 补全详情:`returns (…)` → `returns (const format_string < _Types ... > _Fmt, _Types && ... _Args)` | `completion/mod.rs::detail_of` |

### 顺带修好一条**从来没有生效过**的实体规则

`the_same_declaration_in_one_file` 要求"能把两条声明分开的字段必须**在场**",函数的那个字段写的是
`!parameters.is_empty()` —— 而 `parameters` 是**模板**参数名,普通函数永远没有。**所以函数从来没被合并过**:
MSVC 那种"`void f(int);` 写一遍、再在 `_EXPORT_STD` 后面写一遍"的写法,在索引里一直是两个答案。
现在证据换成了参数表本身,并且参数表也进了 `the_same_declaration` 的比较:

```text
namespace ns { void f(int); void f(int); void f(double); }
    旧规则:3 个实体(一个都没合并)      ← 新测试在旧规则下**红**:left: 3, right: 2
    新规则:2 个实体(重复的 (int) 合一,(double) 是重载)
```

**这是这个 session 里第三次"某条规则其实一直没生效"**(前两次:§22 的去重键、§26 的 `members_of` 假修复)。

### 读数(用户的工程,release)

```text
std:: 补全      format    string (const format_string < _Types ... > _Fmt, _Types && ... _Args)
                vformat   string (const string_view _Fmt, const format_args _Args)
                getline   basic_istream<_Elem, _Traits> & (basic_istream<_Elem, _Traits>&& _Istr,
                                                          basic_string<_Elem, _Traits, _Alloc>& _Str, const _Elem _Delim)
std::string::   push_back _CONSTEXPR20 void (const _Elem _Ch)
                size      _NODISCARD _CONSTEXPR20 size_type ()      ← 无参函数说 `()`,不再说 `(…)`
```

**还剩一件**(下一条):`std::format(` 的 **signature 仍然是"没有"** —— 它有四个重载,而 `signature_at` 要求
**一个**被调用者,`callee_of_a_call` 答 `Ambiguous` 就退出了。LSP 的 `SignatureHelp.signatures` 本来就是**列表**,
所以正确的形状是"把这一组重载都发过去";hover 也一样(今天对 `std::format` 只能说 `Ambiguous`)。
现在事实里有参数表了,这两处都有东西可写。

---

## 29. 重载集合:把"我不猜"从**空弹窗**改成**一份列表**(推翻一条写在代码里的决定)

上一条的收尾。两处的实际行为:

```text
std::format(           signature:**没有**(四个重载 → callee 解析 Ambiguous → 直接退出)
hover std::format      **没有弹窗**(Ambiguous 落到"表达式"那个问题,而这个名字不在表达式里)
```

而 `cpp_ls/src/handlers/signature_help/mod.rs` 顶上写着一条**明确的决定**:"本服务器只发**一个**签名,因为
它只解析出一个;几个候选之间选一个**就是重载决议**,而一份"让读者自己挑"的签名列表不能是猜的。"

**那条论证对"挑"是对的,对被它推出来的结论是错的**:列表的替代品不是"一个签名",而是**没有**。
`std::format` 是四个声明,于是 `std::format(` 在现代 C++ 最常见的那个调用上弹出**空**。发四个什么都不声明 ——
客户端堆叠显示、读者用方向键选,这正是 `activeSignature` 存在的理由。所以这一轮推翻了那条决定,
并把理由写回原处。

### 改动(三处)

| 改什么 | 在哪 |
| --- | --- |
| **复数名字解析**成为原语:`definitions_at` 返回**同一个作用域里的全部绑定**(C++ 的普通查找在第一个声明了该名字的作用域就停,但那个作用域里的**所有**重载都算数);`definition_at` 变成它的"取第一个" | `sema/resolve.rs`(`first_binding_of` → `bindings_named`,两个调用点都改成复数) |
| `callees_of_a_call`:名字走复数查询(`ProjectIndex::definitions`),**成员调用保持单数**(`members_of` 是"整个成员表"而不是"一个名字的声明们" —— 登记为下一步) | `index/project.rs` |
| `signatures_at`:每个候选一个 `CallSignature`,**同一个文件只解析一次**(`<format>` 的四 `format` 在一个头里,四次解析是三次多余) | `signature.rs`、`session.rs`(`Session::signature_at` → `signatures_at`) |

LSP 侧:`signature_help_of` 收一个切片,**每个签名带自己的 `active_parameter`** —— 四个 `format` 的参数个数
不一样,一个全局计数在读者切换签名的那一刻就会指错。hover 侧:`Ambiguous` 不再落到表达式那个问题,而是走复数查询,
把每个声明的 `returns name(参数表)` 列成一个代码块(从**事实**拼,不解析任何文件)。

### 读数(用户的工程,release)

```text
--- signature ---
label: std::format(const format_string<_Types...> _Fmt, _Types&&... _Args)
label: std::format(const wformat_string<_Types...> _Fmt, _Types&&... _Args)
label: std::format(const locale& _Loc, const format_string<_Types...> _Fmt, _Types&&... _Args)
label: std::format(const locale& _Loc, const wformat_string<_Types...> _Fmt, _Types&&... _Args)
  每个都带 parameters / active Some(0) / declared in .../include/format

--- hover on an overloaded name ---
definition(std::format) = None        ← 一个名字回答不了
definitions: 4 declaration(s)         ← 弹窗就是用这四行拼的
   string  format(const format_string < _Types ... > _Fmt, _Types && ... _Args)
   wstring format(const wformat_string < _Types ... > _Fmt, _Types && ... _Args)
   string  format(const locale & _Loc, const format_string < _Types ... > _Fmt, _Types && ... _Args)
   wstring format(const locale & _Loc, const wformat_string < _Types ... > _Fmt, _Types && ... _Args)
```

**一个副作用值得记下来**:同一个声明现在有两种拼写 —— signature 的 label 来自**那次解析的文件原文**
(`format_string<_Types...>`),补全详情来自**熟读事实**(`format_string < _Types ... >`,渲染器每 token 一个空格)。
两个都是"文件怎么写",只是两个读数;要不要统一是**排版策略**,今天没动。

**测试**:`tests/signature.rs` 两条新测试(缓冲区里的重载集合、同一个头里的重载 + 每个签名自己的 active parameter)、
LSP 侧一条新测试(每个重载带自己的 active parameter)+ 两条适配。看齐:554 + 全部集成测试绿,clippy 干净。

**下一步**(登记,顺序按价值):① **成员调用的重载集合**(`s.push_back(` 今天只报 `direct_member` 挑的那一个);
② `auto` 的类型拼写;③ `private`/`protected` 的访问级别。

---

## 30. 成员调用的重载集合:9 个 `append`,以及签名被折行的那个问题

第 ① 条。`callees_of_a_call` 的成员分支原来退回单数(`direct_member` 挑一个),现在走 `members_of` ——
**整个成员表、含基类** —— 再按写下的名字过滤:

```text
s.append( → 9 signature(s)                ← MSVC 的 basic_string::append,一个不少
std::format( → 4 signature(s)             ← 上一轮那条,没退化
```

顺带补了两条测试:同一个类里的两个成员重载、**继承来的**成员重载(基类声明、派生类调用)。

### 折行:签名 label 里的换行

第一次量出来是这样的(9 条里 3 条):

```text
append(
        _In_reads_(_Count) const _Elem* const _Ptr, _CRT_GUARDOVERFLOW const size_type _Count)
```

`signature.rs` 用的是节点原文,而原文是**给读文件的人排版**的。这和我两轮前给事实的 `parameter_list` 做的净化
是**同一条规则**,所以现在只有一份实现:`inlay::tidy_list_text`(空白折叠、`(` 后 / `)` 与 `,` 前不留空格)。

**但签名多一个约束**:它的 `parameters` 是 label **内部的字节区间**(客户端拿它加粗),所以净化必须**带回一张
字节映射** —— `map[i]` = 原文第 `i` 个字节落到了哪;否则去掉换行的同时把高亮挪到了错的字节上,那比换行更糟。
`tidy_list_text` 因此返回 `(净化后的文本, map)`,`parameter_list_text` 用前者、`signatures_at` 两个都用。

新测试 `a_wrapped_parameter_list_is_one_line_and_the_spans_follow_it`:声明跨两行 → label 一行,且
按 span 切出来的两段仍然正好是两个参数。

**看齐**:554 + 全部集成测试绿(签名套件 15 条),clippy 干净。

---

## 31. C++ 模块:**语法层已经有了,分析层一个事实都没有** —— 以及按编译器的设计

用户在 ① 做完之后要求"考虑支持 C++ 的模块,需要针对不同编译器"。先把现状量清楚(读代码,不是猜):

**parser 已经支持模块语法** —— `ModuleDecl` / `ImportDecl` / `GlobalModuleFragment` / `PrivateModuleFragment`,
以及 AST 取用:`CppModuleDecl::{is_interface, get_name, get_partition}`、`CppModuleName::get_name_text`、
`CppImportDecl::{get_name, get_header_name}`。**分析层是零**:没有"这个文件声明了模块 M"的事实,没有
`import M;` 的边,没有模块图,`visible_files` 只看 `#include`。所以 `import M;` 之后,模块里的名字全部不可见 ——
和 `<format>` 那次一样,是"静默地什么都没有"。

### 决定设计的那条差别:`import M;` **不带来任何文本**

能带来文本的只有两样:

1. **编译器的产物(BMI)**:MSVC `.ifc`、Clang `.pcm`、GCC `gcm.cache/*.gcm`。它们都是**序列化的 AST**,
   和编译器版本、标志一一对应(格式不保证兼容),读它们等于复刻一个编译器的内部格式 —— 和本项目
   "落盘的是事实不是树"那条原则正面冲突。
2. **项目里的接口单元源码**:`export module M;` 所在的那个 `.ixx` / `.cppm` / `.cpp`。**这是我们能读的**,
   而且项目扫描已经在读它。

**所以设计是**:模块是**第二条可见性边**,和 `#include` 并列,而不是第二个前端:

```text
新的 summary 事实   文件 → 声明的模块名 / 是否接口单元 / 分区 / 全局模块片段
                    文件 → import 的模块名(含 `import <header>;` 头单元、`import :part;`)
新的 ModuleGraph     模块名 → 接口单元文件(项目内扫描;两个接口单元同名 = 歧义,如实报告)
visible_files        `#include` 之外,`import M;` 解析到的接口单元的**导出**名字也算可见
```

**"导出"必须建模,否则更糟**:接口单元里只有 `export` 的名字对 importer 可见,所以事实需要一个
(按模块链接)的可见性位;把模块内的私有名字泄漏出去,比现在"什么都没有"更难查。

**诚实的分界**:`import std;` 和第三方预编译模块在项目里**找不到**声明它们的文件。这时不能假装可见,
要给一条明确的诊断("这个模块不在本项目里;BMI 这种格式本分析不读"),并**按编译器说清去哪找**。
这和 §27 给 `<format>` 加的那条 note 是同一个形状:用户有权利知道"为什么这里是空的"。

### 按编译器的差异(只影响怎么构建,不影响上面的设计)

| | MSVC | Clang | GCC |
| --- | --- | --- | --- |
| 接口单元扩展名 | `.ixx` | `.cppm` | `.cppm` |
| 编译接口单元 | `/std:c++20 /interface` → `.ifc` | `-std=c++20 -x c++-module --precompile` → `.pcm` | `-fmodules-ts -x c++-module` → `gcm.cache/M.gcm` |
| 消费方怎么找到它 | `/reference M=x.ifc`(或 `/ifcSearchDir`) | `-fmodule-file=M=x.pcm` | **没有标志**:按名字在 `gcm.cache/` 里找 |
| `import std;` | 支持(`/std:c++latest`) | 要先构建 std 模块 | 要先构建 std 模块 |

所以要做的三件"针对不同编译器"的事,都很小:① `.ixx`/`.cppm` 要进 `workspace.source_extensions` 的默认值;
② 命令行里认出这些模块开关(别当成未知参数、更别当成 include 路径);
③ 那条"找不到模块"的诊断按编译器给出去处(`/reference`、`-fmodule-file`、`gcm.cache/`)。

**第一步(小,可量)**:在 `#`-指令扫描旁边加一趟**模块扫描**(一个文件声明的模块名 + import 列表),
进 summary 的新字段(CODEC_VERSION 19),然后 `examples/modules_probe.rs` 在一个模块工程上打印模块图和
每条 import 解析到什么。**不碰 BMI**,也不假装能读它。

---

## 32. `auto`:三个各自独立的缺陷,量出来两个共同原因

用户点名的第 ② 条。起点是一个数字:`auto y = x.back();` 的 hover 答 **`_NODISCARD _CONSTEXPR20 reference`**。

我先把"这可能是两个不同的问题"变成一次实验:如果**把声明所在的头按编译器的方式读一遍**(熟读),答案会不会变?
答案是**不会**(熟读读数存在、`back` 有 3 条,但 `type_at` 还是老样子),于是把两个读数的事实并排打出来:

```text
raw fact:    returns Some("_NODISCARD constexpr const_reference")
cooked fact: returns Some("const_reference")
```

**同一份文本的两个读数对同一个声明说了不同的话** —— 熟读那份是对的,而查询用的是 raw 那份。三个缺陷因此定位:

| # | 缺陷 | 修法 | 读数 |
| --- | --- | --- | --- |
| ① | **返回类型是"文本剥关键字"读的**:`DECLARATION_SPECIFIERS` 里没有库自己的宏(`_NODISCARD`、`_CONSTEXPR20`),所以整个宏前缀留在拼写里 | 换成**按语法读**(`read_specifiers`,和 `type_of` 同一个读法):宏站在那里就是一个 `NameExpr`,而"没有 C++ 类型是连续两个非限定名字",所以**取最后一个名字**,别的按节点种类丢掉 | `back` 的 returns:`_NODISCARD _CONSTEXPR20 reference` → **`reference`**;补全详情里 `size` 从 `_NODISCARD _CONSTEXPR20 size_type ()` 变成 `size_type ()`,`push_back` 从 `_CONSTEXPR20 void (…)` 变成 `void (…)` |
| ② | **嵌套别名走查只跑在成员变量的 `type_of` 上,没跑在调用的 `returns` 上** | 把那段走查提成一个闭包,两处都走(`v.front` 和 `x.back()` 是同一个问题) | `x.back()`:`reference` → **`value_type&`** |
| ③ | **走查要求"整条拼写是个裸名字"**,而 `value_type&` 不是 —— 于是差一步停下 | 改成看**类型的 base**(`Type::class_name()`,引用是透明的),替换时**保留操作符**:新增 `Type::replacing(name, with)`(和 `substituted` 同一趟结构走查,区别只是替换的是"名字"还是"模板参数") | `x.back()`:`value_type&` → **`_Elem&`** |

②③ 合起来是 MSVC 的 `<xstring>` 真实需要的两步链:`reference` → `value_type&` → `_Elem&`。
每一步都是同一个类的成员别名,而每一步之后**都不是**一个裸名字。

### 这一轮**没做完**的两件,以及它们各自为什么

量到 `_Elem&` 就停了,剩下两步各有各的原因,都不是"再试一次":

1. **`auto y` 自己还是 `reference`**,而同一行的 `x.back()` 已经是 `_Elem&`。因为"一次调用有什么"有**两个读者**:
   `what_a_call_has_in`(成员访问那条路,**这轮加了走查**)和 `NamedDeclaration::what_a_call_has`(`auto` 的推断从
   `initializer_type` → `type_of_a_call` 走这条,**没有走查**)。按这个项目自己的规矩,两个读者必须合成一个 ——
   合成需要把 `index`/`scopes`/`path` 传给后者,是一次重构而不是一个补丁。
2. **`_Elem&` → `char&`**:类是通过**别名**到的(`std::string` → `basic_string<char, char_traits<char>, allocator<char>>`),
   而 `resolve_aliases` 只解析**名字**、把目标里的模板实参丢了 —— 于是 `member_bindings` 拿到空配对,
   `_Elem` 没东西可换。这条同时解释了另一个老现象:`type_at("std::string::size_type")` 是 `UnknownType`。

两条都登记在下面,顺序就是它们应该被做的顺序(先 1 再 2:1 是一个答案有两条实现,2 才是缺一步)。
**看齐**:554 + 全部集成测试绿,clippy 干净。

**下一步**:`auto` 的剩余两步 → ③ `private`/`protected` 访问级别 → §31 的模块第一步。

---

## 33. `auto` 补完:`auto y = x.back()` 现在答 **`char&`** —— 也就是编译器会说的那个

§32 登记的两步都做完了,而且第二步比登记的更宽:**同一段逻辑有两个调用点**。

### 第一步:一个答案的两条实现合成一条

`what_a_call_has` 现在收 `(index, scopes, root, path)`,对自己的 `Indexed` 分支跑**同一个** `finish_a_nested_name`
(从闭包提成自由函数),类取 `DeclFact::scope` —— 成员声明所在的那个类。于是:

```text
改之前   `x.back()` → `_Elem&`      `auto y` → `reference`      ← 同一个问题两个答案
改之后   `x.back()` → `_Elem&`      `auto y` → `_Elem&`         ← 一个读者
```

### 第二步:别名**带来的**模板实参

`resolve_aliases` 走别名链时用 `base_type_name(&target)` —— 那一步**正是把实参丢掉的地方**。
新增 `resolve_aliases_with_arguments`(旧的变成它的 `.0`):每步把目标**按类型读**,目标是
`basic_string<char, char_traits<char>, allocator<char>>` 时把这三个实参带回来;若某步的目标没有实参
(裸名字),就沿用上一步的。

然后**一个共享的取用点** `member_access_class_and_arguments`:**use 写了实参就用 use 的**(`std::vector<int>`),
**没写就用别名带来的**(`std::string`)。这个函数之所以存在,是因为这个"半件事"原来在两个调用点上各写了一遍:
`type_of_expression`(成员当值读)和 `declaration_of_a_callee`(成员被调用)—— 上一步刚把两处的**走查**合成一个,
这一步把两处的**配对**也合成一个,**它们本来就是同一个答案的两半**。

```text
x.back()    `_Elem&` → **`char&`**        (别名实参到位)
auto y      `_Elem&` → **`char&`**        (第二处配对也修了)
```

### 完整链条(全部量在同一台机器、同一个进程里)

```text
原始读数                     _NODISCARD _CONSTEXPR20 reference
① 返回类型按语法读            reference
② 调用的返回类型也走走查       value_type&
③ 看 base 而不是整条拼写       _Elem&
④ 别名带来的模板实参          char&        ← 编译器对 `std::string s; s.back()` 的答案
```

**新测试** `a_member_of_a_class_reached_through_an_alias_gets_the_aliass_arguments`:一个 `basic_string`
(带 `value_type`/`reference` 两层别名)+ `using string = basic_string<char>`,同时钉住"成员当值读"和
"成员被调用后 `auto` 接住"两条路 —— 两条路曾经给出不同答案,所以要一起钉。

**仍然登记的一件**:`type_at("std::string::size_type")` 还是 `UnknownType`。**这条不是同一个病**:
它走的是 `::` **限定名解析**(`sema/resolve.rs`),不是成员访问 —— 别名实参那步修不到它。
**看齐**:554 + 全部集成测试绿(类型套件 26 条),clippy 干净。

---

## 34. 访问级别:事实里多一个字节,补全里少一半错名字

用户点名的第 ③ 条("`private` 这些作用域都没搞好")。改之前:**模型里根本没有访问级别** ——
`DeclFact` 没有这个字段,`public:`/`private:` 那两个 `AccessSpecifier` 节点谁也没读。

### 改动

| 改什么 | 在哪 |
| --- | --- |
| `Access { Public, Protected, Private }` + `DeclFact.access: Option<Access>`(`None` = 不是类成员,或类体没读到) | `summary.rs`,codec 里一个 `u8`(**CODEC_VERSION 18 → 19**) |
| **读法**:`DeclarationShapes` 那一趟里维护一个"类体栈"(`(结束偏移, 当前级别)`),遇到 `ClassBody` 压栈、遇到 `public:`/`private:`/`protected:` 改栈顶,**每个 shape 记下当时在force的级别**;没有标签之前的默认值由类键决定(`class` 私有,`struct`/`union` 公开) | `sema/declarations.rs`(`Shape::access` + `DeclarationShapes::access_at`) |
| 缓冲区的生产者(`fact_from_binding`)用树上的孪生读法:从名字往上找最内层 `ClassBody`,再扫它自己的子节点取**名字之前最后**一个标签 | `sema/declarations.rs::declared_access_at` |
| **过滤**:`offerable_members` —— 保留规则(实现保留名)和访问规则**合成一个函数**,完成层和 `member_completions_at` 两个调用点都走它 | `index/project.rs`、`completion/mod.rs` |

**"谁能写这个名字"**:游标所在的类 + 它**派生自**的类(`classes_the_cursor_is_in`,从作用域取最内层类,再沿
`bases_of` 走出去,有界)。`private` 只有该类自己能写、`protected` 还有它的后代 —— 一个列表同时答两个问题,
因为检查的是"成员所在的那个类**在不在**这张表里"。`None`(没读到类体)**照常提供** —— 藏起一个判断不了的
名字,比多给一个错名字更糟。

**计数而不是静默丢弃**:`MemberCompletions::hidden_by_access` 是"因为访问级别没被列出来几个",
于是"这个类没有成员"和"它的成员在这里全是 `private`"是两句不同的话 —— 后者有一个读者能动手的修法。

### 读数与测试

```text
std::string:: 补全   64 个成员,**一个没少**(MSVC 的 basic_string 成员都是 public —— 真语料上零代价)
两条新测试(先证明会红):
  a_private_member_is_offered_inside_the_class_and_not_outside_it
     类外 `w.`  → ["shown"]            ← `class` 的默认成员是 private,`int hidden;` 就在里面
     类内 `other.` → ["grow","hidden","shown"]
  a_protected_member_is_offered_in_a_derived_class
     派生类内 `other.` → ["grow","guarded","open"]
     自由函数 `d.`     → ["open"]      ← `protected` 与 `public` 的区别就在这一行
```

**登记的两件**(有意不做):`friend`(模型里没有,朋友关系会被当成不可见 —— 与"少一个名字好过多一个写不出的
名字"一致),以及成员**定义在类外**时(`void C::f() {}`)访问级别的取用(它的 `ClassBody` 不在名字的祖先上)。

**看齐**:556 + 全部集成测试绿,clippy 干净。

---

## 35. 模块:把"支持"拆成三个问题,只有两个能在这条路上答

用户第二次提到模块,所以把 §31 的设计再往下一步 —— 到"可以开始写"的粒度。

### 一个模块,读者会问三件事

```text
① 这个文件声明/导入了什么?        export module M;  import M;  import <header>;  import :part;
② `import M;` 让哪些名字可见?      M 的接口单元里 **export** 的那些
③ `M` 定义在哪个文件里?            项目里哪个文件的文本写着 `export module M;`
```

**① 和 ③ 是我们能答的** —— 语法层已经会读(`ModuleDecl`/`ImportDecl`/`GlobalModuleFragment`/
`PrivateModuleFragment`,以及 `CppModuleDecl::{is_interface, get_name, get_partition}`、
`CppImportDecl::{get_name, get_header_name}`),而"扫项目里的源文件、按模块名建一张表"就是 ③。

**② 只能答一半**,而这一半的边界必须说清楚:`import M;` **不带来任何文本**。能得到 M 的名字只有两条路:

* **BMI**(MSVC `.ifc` / Clang `.pcm` / GCC `gcm.cache/*.gcm`):序列化的 AST,**和编译器版本、标志一一对应**
  (格式不承诺兼容)。读它 = 复刻一个编译器的内部格式,和本项目"落盘的是事实不是树"正面冲突 —— **不做**。
* **接口单元的源码**:如果那个文件**在本项目里**,我们就能读懂它导出的声明。这是我们走的路。

所以产品的承诺是:**`import M;` 的模块在本项目里 → 名字可见;不在(标准库的 `import std;`、预编译的第三方
模块)→ 明确说"读不到",并给出该编译器下"去哪找"的一句话**。这和 §27 给 `<format>` 加的那条 note 是同一个形状。

### 落地的形状(四步,每步都能单独量)

```text
第 1 步  模块扫描进 summary                                   CODEC_VERSION 20
         文件 → 声明的模块名 / 是否接口单元 / 分区 / 全局模块片段
         文件 → import 的模块名(含 `import <header>;` 头单元、`import :part;` 分区)
         (就是 §31 说的第一步;`#`-指令扫描旁边加一趟,不动 BMI)

第 2 步  ModuleGraph 进 ProjectIndex                          和 include graph 并列
         模块名 → 接口单元文件(项目内扫描;两个接口单元同名 = 歧义,如实报告)
         `import M;` 的解析结果(项目内 / 不在项目内),按文件查

第 3 步  "导出"位 + visible_files                            这条最难,也最危险
         事实需要一个"按模块链接可见"的位,否则接口单元里**非导出**的名字会泄漏给 importer ——
         那比现在"什么都没有"更难查(用户会以为能写,编译不过)
         `visible_files`: `#include` 之外,把 `import M;` 解析到的接口单元的导出名字算进来

第 4 步  examples/modules_probe.rs                            量它的工具
         在一个模块工程上打印:每个文件声明/导入了什么、模块图、每条 import 解析到什么、
         以及"这个 import 读不到"的诊断
```

### 按编译器的差异:**只影响第 1、2 步的边角,不影响设计**

| | MSVC | Clang | GCC |
| --- | --- | --- | --- |
| 接口单元扩展名 | `.ixx`(也接受 `.cpp`) | `.cppm` | `.cppm` |
| 编译接口单元 | `/std:c++20 /interface` → `.ifc` | `-std=c++20 -x c++-module --precompile` → `.pcm` | `-fmodules-ts -x c++-module` → `gcm.cache/M.gcm` |
| 消费方怎么找到它 | `/reference M=x.ifc`,或 `/ifcSearchDir` | `-fmodule-file=M=x.pcm` | **没有标志**:按名字在 `gcm.cache/` 找 |
| `import std;` | 支持(`/std:c++latest`) | 要先构建 std 模块 | 要先构建 std 模块 |
| 头单元 | `import <vector>;` | 同 | 同 |

要做的事很小:① `.ixx`/`.cppm` 进项目扫描的默认扩展名(现在只认 `.cpp/.cc/.cxx/...`);
② 命令行里认出这些模块开关 —— **别当成未知参数,更别当成 include 路径**(`/reference M=x.ifc` 的第二个词长得
很像路径,`-fmodule-file=M=x.pcm` 里有个 `=`,这两个都是"认错了就会静静答错"的形状);
③ 上面那条诊断按编译器给出去处。

**不做的,以及为什么**:不读 BMI(理由见上)、不做 `import std;` 的特殊照顾(它的 BMI 由构建系统生成,
本项目看不到)、不做模块的**语义**(导出名字的可见性用"这个接口单元的 `export` 前缀"来近似,而不是做模块
链接/可达性分析)。

**下一步的顺序**:模块扫描(第 1 步)→ 模块图(第 2 步)→ 导出位与 `visible_files`(第 3 步)。
在动手之前想先量一件事:**这个工作区里有没有模块**(用户的项目没有)—— 没有真语料的话,第 3 步的"导出位"
就只能在合成 fixture 上验证,这一点要提前说清楚,免得又出现"量了半天是空跑"。

---

## 36. 按标准手写一个模块来测 —— 它立刻找出两个真缺陷,而且**图早就在那儿了**

用户说"你自己根据 C++ 标准写个模块来测试就好了"。做了,而且这一步的价值立刻兑现。

### fixture:`tests/fixtures/modules`,由**编译器**证明是合法 C++20

```text
mathlib.ixx      export module mathlib;         接口单元,一个 export 的命名空间 + 一个**不 export**的函数
mathlib.cpp      module mathlib;                实现单元(`export` 缺席)
geometry.cppm    export module geometry;        再导出:`export import mathlib;`
                 export import mathlib;
main.cpp         import mathlib; import geometry;
                 #include <cstdio>                混合写法,过渡期的日常形状
```

用 `target/build_modules.bat` 真编译并**真跑**:`cl /std:c++latest /EHsc /c /interface …` → 链接 →
输出 `7 12 12`(三个函数的结果都对:`add(3,4)`、`multiply(3,4)`、`area_of(3,4)`)。脚本里三条**量出来的**编译器事实,正是 §35 那张表预测的边角:

```text
/interface 单独用**仍然会链接**          → 必须加 /c            (否则 LNK1561)
MSVC **不认 .cppm 扩展名**               → 当对象文件报 LNK1107  → 必须 /TP
模块分区(module geometry:shapes;)       → C7621,还要额外的 /reference 编排 → 本轮从 fixture 里去掉
```

### 然后探针(`examples/modules_probe.rs`)把状态一次说清

```text
① 语法读数             四个文件全对:接口单元 / 实现单元 / 再导出 / 混合写法 —— 而且项目扫描**已经**包含 .ixx/.cppm
② import 图            改之前:main.cpp --mathlib--> **"no file in this project declares module mathlib"**
                       ← 命名约定提出了 mathlib.ixx,而那个拼写被当成**相对进程目录**的路径去读了
③ 产品问题             definition("mathlib::add") = None;`mathlib::` 补全 0 项
```

**②是两个真缺陷里的第一个**,而且它一句话就能说清:`search_directories` 的第一个目录是**空路径**,注释写着
"导入文件所在目录**在解析时**会拼上去" —— 但 `resolve_module`/`resolve_partition` 是**直接读**候选文件的,
`join_normalized("", "mathlib.ixx")` = `mathlib.ixx`,于是 `DiskFiles::read` 拿它去问**进程的当前目录**。
修法:把导入文件的目录传进 resolve,在那里拼(头单元那条路本来就是这样做的)。

**库里早就有的东西比计划写的多**:`sema/modules.rs` 是完整的模块图 —— `ModuleGraph`、`ModuleScanner`
(索引 + 命名约定两步解析,候选文件**必须自己声明那个模块**才被接受)、七种诚实的失败(`UnknownModule`、
`UnknownPartition`、`UnknownHeaderUnit`、`NotDeclaredHere`、`NoModuleToPartition`、`PartitionMismatch`)、
`dependents_of`(改动作废用),以及"**不携带宏环境**"这条明确的设计。`tests/modules.rs` 有 33 条测试。
所以 §31/§35 的"第 1、2 步"**是过时清单** —— 我上次写它们之前没读代码,这是同一个病第二次犯在我自己身上。

### 缺的是**接线**:`import` 是第二条可见性边,而可见性走查只读 `#include`

三处改动:

| 改什么 | 在哪 |
| --- | --- |
| `ModuleReading { module, partition, is_interface, imports }` 进 **summary**(分区与头单元明确不收:分区属于导入者自己的模块,头单元走 `#include` 那套搜索) | `summary.rs`,codec **CODEC_VERSION 19 → 20** |
| 索引时从树上读一次(`ModuleInfo::from_tree`)填进 summary | `index/mod.rs::modules_of` |
| `ProjectIndex::module_interfaces`:模块名 → **接口单元**文件(实现单元不记:`import m;` 要的不是它);`visible_files` 在 `#include` 之外沿 import 走,解析不到就**什么都不加、什么都不声称**(`import std;` 与预编译模块落在这里) | `index/project.rs` |

**读数(同一个 fixture,同一个进程)**:

```text
definition(mathlib::add)      = .../mathlib.ixx      ← 之前是 None
definition(mathlib::multiply) = .../mathlib.ixx
definition(Point)             = .../geometry.cppm    ← 经由 geometry 的 `export import`
definition(manhattan)         = .../geometry.cppm
completion after `mathlib::`  → 3 项:add、**hidden_helper**、multiply
```

**第三项就是那个已知缺口,现在它是可复现的而不是一句话**:fixture 里 `hidden_helper` **没有 export**,
而我们提供了它 —— 因为"导出位"还没建模(§35 第 3 步剩下的那一半)。方向是**多给**而不是少给,这一点
§35 已经写明,现在有一个例子钉着它。

**两条新测试**:`a_module_beside_the_importing_file_is_found_from_that_file`(相对目录那个 bug 的形状)、
`an_import_makes_the_modules_names_visible`(产品那半)。**看齐**:556 + 33 条模块测试全绿,clippy 干净。

**下一步**:导出位(`export` 的声明才可见 —— 有了 fixture,这条现在能在真语料上量:`hidden_helper` 必须消失),
然后按编译器把 `/reference`、`-fmodule-file`、`gcm.cache/` 那条诊断写出来。

---

## 37. 头单元与 `import std;`:标准库的接口单元**是编译器自己带着的源码**,这条路上没有额外成本

用户接着要求两件事:C++20 的 `import <iostream>;`(头单元)和 C++23 的 `import std;`。两件事**是同一个机制**
从上到下看两遍:一个不是本项目文件的接口单元,能不能被读进来。

### 事实先量清楚:MSVC 到底在哪里放 `std`

```text
<VC>/Tools/MSVC/14.35.32215/include/             头文件,加进 include 路径的那个目录
<VC>/Tools/MSVC/14.35.32215/modules/std.ixx      3 194 字节:**`export module std;` + 把每个标准头 #include 一遍**
<VC>/Tools/MSVC/14.35.32215/modules/std.compat.ixx
```

**它在 `include` 的兄弟目录里,不在 `include` 里面** —— 这一条决定了"去哪儿找"。所以
`ModuleScanner::search_directories` 多了一个目录:每条 include 路径的**兄弟 `modules` 目录**。候选文件仍然只是
*提议*(读出来必须自己声明那个模块),所以一个不存在的目录代价是一次失败的 `exists`,不是一次误报。

**`import <iostream>;` 那条路早就是对的**:头单元是**头文件**,用 `#include` 的同一套搜索解析,解析结果记进
summary 的 `header_units`(路径而不是拼写:可见性走查手里只有 summary,搜不了任何东西)。两个 fixture 文件
`header_units.cpp`(`import <iostream>;` + `import "local_math.h";`,由 `target/build_header_units.bat` 真编译、
真跑,输出 `42`)和 `std_module.cpp`,量出来都对。

**`import std;` 那条路缺的是"这个文件不在项目里"**:`ProjectIndex::module_interfaces`(模块名 → 接口单元)
是从**项目扫进来的文件**填的,而 `std.ixx` 在 MSVC 的安装目录里。于是 `std::` 什么都答不出来 ——
而且不是"答错",是**静默地什么都没有**。

### 量出来的两半:先证明它是空的,再证明它不空

`tests/fixtures/modules/std_only.cpp` 是**只为这一个问题**加的 fixture,因为原有的 `std_module.cpp` 同时
`import <iostream>;`,而头单元是 `#include` 的另一种拼写 —— `std::string` 在那种文件里本来就能解析,
那样量出来的数字**说明不了 `import std;` 的任何事**。只 `import std;` 的文件里:

```text
改之前      definition(std::string) = None    definition(std::cout) = None    `std::` 补全 0 项
改之后      definition(std::string) = <VC>/include/xstring
            definition(std::cout)   = <VC>/include/iostream
```

### 实现:一次"请求驱动的读进来",而不是把标准库塞进每个项目

`Session::read_the_modules_a_file_imports(path)`:读出这个文件(跟着它的 import 图)要的模块名,凡是索引
**指不出文件的**,用 `ModuleScanner` 解析成文件(同一个解析器,不是第二份实现),把这些接口单元排进队列的
**open 半边**,然后 `index_everything()`。

为什么是**函数调用**而不是 pump 的一步:冷读是 **401 个文件、6 151 ms**(下面有数),而一个语言服务器
不能为每个打开的工作区付这笔钱,更不该在没人问 `std` 里的名字时付。这和 `Session::read_the_unit` 是同一个
形状:**调用者正要做一件需要它的事,所以由调用者说**。第二次调用什么都读不进来(接口单元已经在索引里了)。

为什么必须在这里 drain **整个队列**而不是"只读我排进去的那几个":第一次实现就是这么写的(用一个"这些文件
还在队列里吗"的谓词收尾),量出来 `std::string` **仍然是 None**。原因不是收尾条件写错了,是**读一个接口单元
不等于读它包含的东西**:`std.ixx` 全部内容就是 `#include` 每一个标准头,名字在那些头里;而且 `advance` 在队列
排空时会跑第二遍(`re_read_where_a_body_decides`),MSVC 的 `std::basic_string` 在 `yvals_core.h` 被读之前
**是文件作用域的 `basic_string`**。所以"读完"这件事的边界只能由队列本身给。

```text
warm(磁盘上有 summary)
   definition(std::string) = None  →  读到 1 个接口单元,547.7 ms  →  <VC>/include/xstring
   definition(std::cout)            = <VC>/include/iostream
   definition(std::vector)          = None                      ← 见下面那条已知缺口
   再问一次                          0 个文件,0.004 ms           ← 设计押在这一行上

cold(把 `.cppls` 删掉,同一个 root、同一套发现流程)
   401 个文件读进来 6 151 ms;第一次请求付的就是这笔钱(实测量到 5 976 ms)
   接口单元自己的熟读 0.2 ms —— 它的 summary **自己一条声明都没有**(3 194 字节全是 #include)
```

### 一次量错的冷读,值得写下来

第一版的冷读是**另开一个 session、root 指向临时目录**。它读进了 `std.ixx`,数出 393 个文件,然后
`std::string` 答 `None` —— 一个**又慢又空**的数字。原因是那个 session 没有第一个 session 发现到的
toolchain:`with_config` 不跑编译器,于是 `_MSVC_LANG`、`_STL_COMPILER_PREPROCESSOR` 这些宏不在,
MSVC 的 STL 头在那套环境下读出来的东西不是同一回事。而 393 和 401 的差、以及"路径拼写不同"这两个方向
我都先追了一遍(前者是巧合,后者不是原因)。

修法就是现在这个形状:**同一个 root、同一次发现,只把磁盘上的 summary 删掉**,并把这个能力作为
`Session::cache_directory()` 暴露出来(理由写在那个方法的文档里:一个把热数字当冷数字报的基准,是
五秒卡顿被当成特性的方式)。这条也顺手给"测之前先问清楚自己在测什么"加了一个实例。

### 接线在哪儿,以及为什么不在 `prepare` 里

`AnalysisState::prepare` 是所有 handler 都调的那个"先把文件读进来"的入口,但**它的时刻不对**:`prepare` 之后
这个文件的 **summary 还可能是旧的或没有的**(编辑会丢 summary,重建是 pump 的事),而没有 summary 就看不见
它 import 了什么、也就什么都读不进来。所以读被接在 `catch_up` 之后 —— 今天接在**补全、跳定义、hover、
signature help** 四个 handler 上,共用 `handlers::read_the_modules(context, path, caught_up)` 一个函数
(`caught_up` 这个参数就是"summary 已经在前面弄干净了吗":补全在同一个请求里已经调过 `catch_up`,另外三个没有,
由这个函数自己补一次)。

### 没做的,以及为什么

* **不读 BMI**。理由 §35 已经写了,`import std;` 这一轮把它钉得更死:MSVC 的 `std` 之所以能读,是因为它
  **带了源码**;一个只发 BMI 的模块仍然只能是"读不到"。
* **`std.ixx` 的导出位**:它 `#include` 进来的那些头里的声明是**通过 `#include` 边**到达 importer 的,
  所以不受"导出位"过滤(过滤只作用于 import 边)。MSVC 的 STL 用 `_EXPORT_STD` 控制哪些声明真的导出
  (只有建 std 模块时才展开成 `export`),而我们不模拟它 —— 方向是**多给**,和 §36 记的同一件事。
* **模块分区**(`import m:part;`)与 `import "header";` 的引号形式:解析和解析器都在,但本轮没有新的真语料,
  分区还是 §36 记的那句"C7621,还要额外的 `/reference` 编排"。

### 测试

`tests/modules.rs::a_module_whose_interface_unit_is_outside_the_project_is_read_in` —— **不碰机器**:
`MemoryFiles` 里摆一个机器的布局(`lib/include` 是 include 路径、`lib/modules/std.ixx` 是它的兄弟、
导入文件在第三个目录),config 里配那条 include 路径。断言四件事:读之前 `std::string` **不是** `Yes`
(否则这个测试是在测别的东西)、读进来 **1** 个接口单元、`interface_unit_of("std")` 指向那个文件、读之后
`std::string` 是 `Yes`;最后再断言**第二次调用读进来 0 个**(幂等 —— 请求驱动的设计就押在这上面)。

`a_names_imported_from_the_standard_module_are_offered_after_the_scope` 是**产品那半**:同一个布局,光标停在
`std::` 之后,断言 `scope == "std"` 且 `string`、`vector` 都在列表里。分两层写是有意的 —— "模块被读进来了"
和"读者看到了名字"是两件事,而只有第二件是用户的问题。

顺带修了一条**会随机器变**的老测试:`a_module_available_only_as_a_bmi_is_unknown` 原来用 `import std;`
当"只有 BMI、没有源码"的例子,而这台机器上 `std` **有**源码。改成 `std.compat`(MSVC 也发它,但不在那条
查找路径上),并且把"为什么不能用 `std`"写进注释。

### 之后补的两件事(同一天,接在 §37 后面)

**① `std::vector` 不是缺口,是我的探针问错了问题。** 它一直被登记成"限定名里类模板的解析缺口",量清楚之后
完全不是:索引里有 **4 条** `std::vector` —— `<format>` 里的前置声明、`<vector>` 里的主模板、`vector<bool>`
的偏特化、`vector<bool, _Alloc>` 的偏特化。`definition`(单数)把"多于一条"如实答成
`Known::Unknown(Ambiguous)`,而我的探针写的是 `.value()`,于是 **`None`** —— 索引答得好好的,是**记录的人
把它读成了空**。

`goto definition` 那条路本来就是对的:LSP handler 用的是 `session.definitions`(复数),4 条给 4 个位置。
所以这一条的修法是**把探针改成问复数**(`describe()` helper,打印声明条数与所在文件),不是改产品代码。
教训和 §36 那条一样,只是这次错的是"量的人":**一个把 `Ambiguous` 显示成 `None` 的探针,会凭空造出一个
不存在的缺口,然后让下一个人去修它。**

**② 按编译器的模块诊断,现在真的到用户眼前了。** 之前 `ImportOutcome::describe()` 是**解析层**的话
("no file in this project declares module `std`"),真实、无用:文件在机器上是**有的**,在编译器自己的目录里,
而那个目录**逐编译器不同**。所以新增:

```text
Toolchain::module_note(module)      按方言给一句"你的编译器怎么找模块"
   MSVC    /reference std=<file>.ifc 或 /ifcSearchDir;`std` 再加一句:源码就在 <VC>/.../modules/std.ixx
   Gnu     gcm.cache/<m>.gcm(-fmodules-ts)/ -fmodule-file=<m>=<file> / -fprebuilt-module-path=<dir>
           两句都写,因为 Dialect 只有两个值而这三种编译器 —— 拿不准的时候把两种机制都说了
Toolchain::standard_module_source()  逐条 include 路径问它的兄弟 `modules` 目录,问不到就给约定位置
Session::notes_about_the_modules(view)  逐条 import 收成 ModuleNote { message, start, end }
```

**接线**:`FileDiagnostics` 多一个 `notes: Vec<ModuleNote>`,和 `errors` **同一次解析**产出(一个消费者问
两次就是为同一棵树付两次钱),LSP 侧发成 `INFORMATION` —— 不是 error 也不是 warning:文件没有任何错,
它只是引用了项目外的模块,而"警告当问题看"的客户端会把构建系统的事报成代码的事。

`lib.rs` 的 `559` 条里新增两条 toolchain 单测(MSVC/GNU/未知方言三句话各断各的,并断言**未知方言不猜开关**)、
一条 `standard_module_source` 的查找测试(自己搭 scratch 树,不碰机器安装),以及集成测试
`an_import_nothing_declares_gets_a_note_on_the_line_that_wrote_it`(断言文案**和范围**:note 的字节区间
正好落在 `import mylib;` 那行 —— 这一半靠 summary 是拿不到的,summary 只记模块名)。

**看齐**:559 库测试 + 全部集成测试绿(模块套件 38 条),clippy 干净。

**下一步**(按价值):① `friend` 与类外成员定义的访问级别(§34 剩下的那一半);② 模块分区的真编译器验证;
③ `import "header";` 的引号形式在真语料上量一遍。

---

## 38. 类外成员定义:**一句"限定名不在这里声明"**,让整个函数体都不在类里

第 ① 条。这是 §34 登记的最后一块,量出来比登记时想的**深一层**。

### 先量:三种写法,只有一种漏

```text
① 类体内声明 + 类体内定义      void grow(Widget& other) { other. }      → 补全列出 hidden ✅
② 类外定义                    void Widget::grow(Widget& other){ other.} → 补全**整个查询拒绝**(Known::No)❌
③ 类外定义,游标在私有成员上  void Widget::set(int v){ hidden = v; }     → 也在类外,同样没绑定 ❌
```

②的形态**每个真实 C++ 工程都在写**,所以它不是边角。

### 根因不是一处,是**三处叠在一起**,而且每一处单独看都合理

```text
1. 语法把限定名的名字放在**说明符序列**里,不放在 declarator 里:
     void grow(Widget& other) { }          DeclSpecifierSeq["void"]          InitDeclarator["grow(…)"]
     void Widget::grow(Widget& other) { }  DeclSpecifierSeq["void Widget::grow"] InitDeclarator["(…)"]
   于是"在 declarator 里找名字"的读者对第二种**找不到名字**,`declared_name` 返回 None。

2. `declared_name` 对限定名**故意**返回 None("int ns::count; 声明的是 ns 里的东西,在这里绑定会
   造出一个不能非限定使用的名字")—— 这条是对的,但它让限定名走的路上没有任何一个环节接手。

3. `is_unnamed_declaration`(最令人头痛的解析那个守门人)问的是 AST 的 `CppDeclaration::get_name_text()`,
   而它对限定名 **也是 None** —— 和它对 `f(x);` 这个调用语句的答案一模一样。于是整个声明被当成
   "走过声明路径但什么都没声明"而**整条跳过**:函数体没有被走,绑定没有,函数作用域没有。
```

第 3 条是真正致命的那一条,也是**只加前两条修不好**的原因:我先把 1、2 修完,测试仍然红,直到发现这个守门人。
判据是 **body**:`f(x);` 没有函数体,而定义按定义就有 —— 一个 `CompoundStat` 直接子节点。

### 修法:三处各修一处,并且修在**语法层**而不是在查询层打补丁

| 改什么 | 在哪 |
| --- | --- |
| 读出 qualified declarator 的 `(qualifier, name)`:最后一个 `::` 之前是限定符,之后是名字(用 `last_identifier` + `name_from_text`,所以 `Widget::~Widget`、`Widget::operator+` 也是对的) | `sema/scopes.rs::qualified_declarator_name` |
| 第二种拼写的同一个读法:名字在说明符序列里,所以从 declarator 往上找 `Declaration`、再取它的 `DeclSpecifierSeq`,并且**只认带 `::` 的 `NameExpr`**(否则 `void f()` 会被答成声明了 `void`) | `qualified_specifier_name` |
| 声明落在**限定符命名的那个类作用域**里,函数作用域挂在它下面 | `ScopeWalker::declarator_as` |
| `is_unnamed_declaration`:有函数体就不是"没声明任何东西" | 同上文件 |
| 名字是**最后一段**(`grow` 而不是 `Widget::grow`) | `declarator_as` |

**为什么修在这里而不是在 `classes_the_cursor_is_in` 里加一条"看看游标所在的函数是不是限定名"**:后者是
workaround —— 它只修补全这一条路,而 `declared_as`、重载解析、`qualification_prefix_of`、跳转定义全都要各自再修一遍,
而且各自会**不一致**。作用域树错了,就该修作用域树。

**为什么不改 parser 的 `get_name_text`**:它是"这个声明引入的名字",而限定名引入的名字**确实**是最后一段 ——
但它在 parser 层是公开 API,`get_name` 有 22 处调用,而这一轮要的只是分析层的一个守门人别读错。真要在 parser 层
把 `ns::Foo<int>::bar` 读对,是另一件事(要处理模板实参里的 `::`),登记在下面。

### 读数(同一个 fixture,同一份代码)

```text
改之前  other.  →  Known::No                                   ← 整个查询拒绝
改之后  other.  →  ["grow", "hidden", "set", "shown"]           ← 私有的 hidden 在里面(因为游标在 Widget 的函数里)
        w.      →  不含 hidden                                  ← 自由函数里同一份成员表,私有的不在
```

**两个方向都断言了**,因为"把访问级别整个关掉"也能让第一行通过。

### 测试

`a_member_defined_outside_its_class_keeps_its_access`(在 `index/project.rs` 的测试模块里,和另外两条访问级别测试
并排)。它同时钉住:类外定义的函数体**在类里**、私有成员**在那里可写**、而**在自由函数里仍不可写**。

**看齐**:560 库测试 + 全部集成测试绿,clippy 干净。

**下一步**(按价值):① `friend`(`friend class X;` / `friend void f();` —— `classes_the_cursor_is_in` 的文档
已经写明它**故意**不建模,所以这一条是"决定要不要改主意",不是修 bug);② parser 层把
`ns::Foo<int>::bar` 这类**带模板实参的限定名**读对;③ 模块分区的真编译器验证。

---

## 39. 带模板实参的限定名:**一个"不在这里声明"的规则,套上了三层**

第 ② 条。§38 修好的限定名读到 `ns::Foo<int>::bar` 就断了,而这是 MSVC STL 和每个模板库的日常写法。

### 三个缺陷,三种"静默",而且都不是"答错"而是"什么都不答"

```text
void Box<int>::grow(int n) { size = n; }   作用域对(Box),但函数体在**文件作用域**走的 → 里面看不见私有成员
int  Box<int>::count = 0;                  **一条 DeclFact 都没有**
int  Box<int>::Inner::deep = 0;            有 fact,但 scope 是 None
```

根因分三层,每层单独看都合理:

```text
1. 作用域的名字里没有模板实参    class Box 的 scope 叫 `Box`,不叫 `Box<int>`。
                                 于是 scope_with_qualified_name("Box<int>::Inner") 找不到 → 回退到文件作用域。
2. 最后一个 `::` 不在文本末尾      `Box<A::B>::grow` 里 `<...>` 内有分隔符,按文本切会切进实参表。
3. `Box<int>::count` 的"名字在哪"  它没有 body,`get_name_text()` 又是 None,于是和 `f(x);` 一样被
                                  `is_unnamed_declaration` 整条丢掉。
```

### 修法

| 改什么 | 在哪 |
| --- | --- |
| 逐段找作用域(段名**不带**实参:`Box<int>` → `Box`),而不是拿整串去比 | `scope_of_a_qualifier` |
| 限定符从**token** 读:跟踪 `<>` 深度,只在深度 0 认 `::`;顺带按 token 类别去掉 trivia | `qualified_name_along` |
| 名字在说明符序列里时:`DeclSpecifierSeq` 的**后代**(`DeclSpecifierSeq > TemplateType > NameExpr`),不是直接子节点 | `qualified_specifier_name_node` |
| 三个条件一起判"这是不是限定声明":说明符里有 `::`、那个 `::` **不在 declarator 里**、declarator 自己没有名字 | `declaration_is_qualified` |
| 守卫:`::` 限定声明**从不在文件作用域绑定名字** —— 要么绑进它命名的那个类,要么什么都不绑 | `declaration_parts` |

最后一条不是新规矩,是 §38 之后必须显式写出来的一条:`tests/scopes.rs::a_qualified_declaration_binds_nothing_here`
早就钉着 "`int ns::Widget::count = 0;` 在本文件既不声明 `ns` 也不声明 `Widget` 时**什么都不绑**" —— 而我第一版
守卫写反了(命中就 return → 该绑的没绑),量出来两条测试红。正确形状是:**限定声明永远不走普通那条路**,
限定符命名的 scope 存在 → 交给 `declarator_as` 绑进那个类;不存在 → 直接返回。

### 读数(同一 fixture)

```text
改之前   fact `count` 不存在;`grow` 的 scope 是 Box 但函数体在文件作用域
改之后   fact grow   scope Box          ← 两条定义都在
         fact count  scope Box
         fact deep   scope Box::Inner
```

**看齐**:561 库测试 + 全部集成测试绿,clippy 干净。

---

## 40. 模块分区:编译器先告诉我**依赖方向是反的**,然后分析层少了一整条边

第 ③ 条,§36 留下的那个 C7621。真编译器跑了一遍,得到两件事。

### 编译器说的(MSVC 14.35.32215,`target/build_partitions.bat`)

fixture 四个文件 + 一个消费者,标准写法(`module shapes:area;` / `export import :area;`),脚本打印 `14 12 16`
= `2*(3+4)`、`3*4`、`4*4` —— 数字本身就是"分区真的被解析、被链接进去了"的证明。

```text
第一版脚本(按文件顺序:先接口、后分区)
   先编 shapes.cppm                        → **C7621: 找不到模块分区 "area"**
   再编 shapes-area.cppm(带 shapes.ifc)   → exit 0

第二版(反过来了)
   先编 shapes-area.cppm(不带任何 /reference) → exit 0
   再编 shapes.cppm,带 /reference shapes:area=shapes-area.ifc → exit 0
```

**依赖方向和文件名顺序相反**:分区是模块的一部分,所以主接口单元要用**分区的 `.ifc`**;而分区本身
**不需要**主接口的 `.ifc` 就能编译。§36 记的"C7621,还要额外的 `/reference` 编排"是对的,但"编排"的方向
当时没量出来 —— 一条**只有编译器能纠正的**猜测。

另外两条也是量出来的:消费者 `import shapes;` **不需要**知道 `:area` 存在(这正是分区的意义);
分区自己的 `.obj` **必须**参与链接(`shapes_area_impl.obj`,否则 `rectangle`/`square` 未定义)。

### 分析层:一条完整的可见性边,谁都没有

`partitions.cpp` 里 `definition("perimeter")` 一直是有的(声明就在主接口单元里),但:

```text
改之前   definition(perimeter) = shapes.cppm
         definition(rectangle) = Unknown(NotDeclaredHere)
         definition(square)    = Unknown(NotDeclaredHere)
改之后   definition(rectangle) = shapes-area.cppm
         definition(square)    = shapes-area.cppm
```

**"什么都没答"而不是"答错",这是这个项目里最难发现的一类** —— 而且方向是**少给**,和本项目到处写着的取向相反。

语法层早就读对了(`shapes.cppm → import partition ':area' re-exported`,shapes-area 两个文件都是
`partition Some("area")`),`ModuleScanner::resolve_partition` 也早就写着、也早就有测试 —— 缺的是
**把解析结果放进 summary**,因为可见性走查**手里只有 summary,搜不了任何东西**:

```text
ModuleReading 多一个 `partitions: Vec<PathBuf>`(已解析,和 header_units 同一个理由)   CODEC_VERSION 22 → 23
索引时用同一个 ModuleScanner 解析(命名约定:shapes:area → shapes-area.cppm)            index/mod.rs::modules_of
可见性走查沿它走一步,和 header_units 并列                                            index/project.rs
```

**为什么不是"把 SummaryKey 改成含分区"**:分区文件**不是**这个文件的一部分——它有自己的路径和自己的 summary,
走查沿着一条边走到它。key 是"这段文本 + 这套编译环境",分区不在其中,也不该在。

### 测试

`a_partitions_names_reach_a_file_that_imports_the_module` —— 不碰机器:`MemoryFiles` 摆出和真 fixture 同名的
三个文件(`shapes.cppm` / `shapes-area.cppm` / `partitions.cpp`),断言接口单元自己的名字**和**分区里的两个名字
都能从 `import shapes;` 的文件里解析出来。文件名用 MSVC 的约定,所以命名解析也被测到。

**看齐**:561 库测试 + 全部集成测试绿(模块套件 39 条),clippy 干净;`target/build_partitions.bat` 打印 `14 12 16`。

---

## 41. 分区的诊断:一句"读不到模块 `area`"是关于一个**不存在的模块**的话

§40 登记的第 ③ 条。分区导入读不到时,以前会走到 `module_note` 那条路上,于是说出一句
**在语言里不成立**的话:"no file declares module `area`" —— 没有任何东西声明一个叫 `area` 的模块,
`import :area;` 要的是**本文件自己那个模块的一个分区**。

### 两句不同的话

```text
module_note(module)               "把接口单元编译成 .ifc 并用 /reference <m>=<file>.ifc 命名"
partition_note(module, partition) "分区不是模块;MSVC 要拿分区的 .ifc 去编译**模块的接口单元**:
                                   /reference shapes:area=<file>.ifc"
```

`partition_note` 里的三件事全部来自 §40 那三条命令(不是标准原文):**主接口**需要分区 `.ifc`(否则 C7621)、
**分区自己**什么都不需要、**消费者**永远不提分区。Clang 那句按 §40 的登记给出
`-fmodule-file=shapes:area=<file>`,GCC 那句说"它被建进模块自己的模块文件里"—— 和 §37 同样的取舍:
`Dialect` 只有两个值,拿不准时把两种机制都说了。

### 接线,以及一个**被测试抓出来的既有缺陷**

`Session::notes_about_the_modules` 现在先看 `partition_name()`,再走模块那条路,并且用
`the_partition_is_read()` 问"这个分区**有没有**文件" —— 问的是 **summary**(这个 session 真读到了什么),
不是命名约定的提议。

写这条测试的时候它红了,而红的原因不是新代码:

```text
SUMMARY src/shapes-area.cppm  module Some("shapes")  partition Some("area")  interface **false**
```

`ModuleReading::is_interface` 写的是 `info.unit == Some(ModuleUnit::InterfaceUnit)` —— 只认**主**接口单元,
于是**每一个分区接口单元**都被记成 `false`,而 `ModuleUnit` 自己早就写着 `is_interface()` 同时认
`InterfaceUnit` 和 `PartitionInterfaceUnit`(并且用另一个函数 `exports_to_importers()` 表达"主接口才是一个模块名
解析到的东西"这个**不同**的问题)。修法是让调用方用那个已有的规则。

**为什么这件事值得写下来**:§40 那条可见性边**没有**依赖这个字段(它沿 `partitions` 走),所以 §40 的
`definition("rectangle")` 照样绿 —— 一个字段写错了整个模块套件也发现不了,直到有人问它。同一个形状
§38/§39 各出现一次:**规则早就在库里写着,错的是调用方**。

### 测试

* 单元(`toolchain.rs`):MSVC 那句话里同时出现 `shapes:area` 和 `/reference`,并且断言它**不**说
  "declares module `area`";GNU 那句不出现 `/reference` 而出现 `gcm.cache/shapes.gcm`。
* 集成(`tests/modules.rs`):分区文件**不在**项目里 → 恰好一条 note,文案点名 `:area` 与 `shapes`;同一个 fixture
  把分区文件**放进去** → 一条 note 都没有。**两个方向都断言**,因为"永远给 note"和"从不给 note"一样错。

**看齐**:562 库测试 + 全部集成测试绿(模块套件 40 条),clippy 干净。

---

## 42. 今天这一轮的收尾:**剩下的活,按价值和理由排好**

今天从"`import std;` 读不到"一路做到"分区有诊断",中间量出**五类缺陷**,其中三类是"规则早就在库里、
错的是调用方"。这一节把没做完的事登记清楚,免得下次从猜开始。

### 一、模块(接着 §35–§41 往下)

| 活 | 为什么值得做 | 已知的坑 |
| --- | --- | --- |
| **分区在 GCC/Clang 上量一遍** | §40 的结论(依赖方向反的、消费者不需要知道分区、分区 `.obj` 必须链)是 **MSVC 的**。Clang 的开关是 `-fmodule-file=shapes:area=<file>`,GCC **没有开关**;这两条现在只是转述文档,没有实测 | 需要在那两台工具链上真跑;本项目只有 MSVC,所以要么装、要么把这条挂着 |
| **`export import` 的方向** | `ModuleReading` 明确不记 `export`,所以**非** re-export 的 `import M;` 现在也会把 M 的名字给出去 —— 方向是"多给" | 记这个位要动 codec 和走查两处;`ImportDeclaration::is_reexport` 语法层已经有了 |
| **分区的实现单元的 `.ifc`** | 现在只解析 `resolve_partition` 给的那个文件(接口单元);实现单元不参与可见性,这是对的 —— 但没写下来过 | 只差一条注释/一条测试 |
| **`import "header";` 的引号形式在真语料上量一遍** | §37 说它和 `#include "…"` 同一套搜索,`header_units.cpp` 里有一个(`local_math.h`),但那是合成 fixture | 真语料 = 一个真用引号头单元的项目 |
| **模块诊断进 LSP 的 `code`/`codeDescription`** | 现在是一句 `INFORMATION`;带 `code` 客户端才能做"这一条能一键修" | 需要协议侧的取舍,不是分析侧 |

### 二、语义(§32–§39 登记的)

| 活 | 为什么值得做 | 已知的坑 |
| --- | --- | --- |
| **`friend`** | `classes_the_cursor_is_in` 的文档**明确写着故意不建模**,方向是"少给";要不要改主意是一个决定,不是修 bug | 改了就要建模 friend 声明和它授予的范围;`is_friend` 的分支现在直接 return |
| **`protected` 与基类链上的访问** | 现在 `protected` 在派生类里可见(有测试),但**继承方式**(`class D : private B`)不参与判断 | 需要 `bases` 里的 access 关键字;`Shape::bases` 现在只记名字 |
| **带模板实参的限定名里,模板实参内的 `::`** | §39 的限定符读取按 `<>` 深度跳过实参,所以 `Box<A::B>::grow` 的**限定符**对,但实参里的 `A::B` 本身没有被解析成类型 | 这是类型解析那条路,不是作用域那条 |
| **重载解析** | 一直没做,也不在这条路上:签名帮助给的是**集合**,挑一个需要实参类型 | 明确不做,记在这里免得被当成缺口 |

### 三、工程(今天反复用到的工具,值得补的)

| 活 | 为什么值得做 |
| --- | --- |
| `target/build_partitions.bat` 之外再加 `build_header_units.bat`/`build_modules.bat` 的统一入口 | 现在三个脚本各写各的 `vcvars` 调用;一个 `build_all.bat` 会让"真编译器验过"这件事更容易重复 |
| `examples/modules_probe.rs` 已经有五节;把"分区"那节补上 | 现在分区是靠 `tests/modules.rs` 和脚本验的,探针没覆盖 |
| 把"调用方用错既有规则"这个模式做成检查 | §38/§39/§41 各一次:错都不在库,在调用方。**没有自动检查**;能想到的最接近的是给 `ModuleUnit::is_interface` 这类函数加一条"调用方必须用它"的注释,以及 codec 版本号那样的强制点 |

### 今天量到的、值得记住的坐标

```text
import std; 冷读    393–401 个文件,6.0–6.5 s(第一次),547.7 ms(磁盘有 summary),0.004 ms(再问一次)
模块套件            40 条;库 562 条;集成全绿;clippy 干净
真编译器验过的      build_modules.bat → 7 12 12;build_header_units.bat → 42;build_partitions.bat → 14 12 16
```

---

## 43. 用户报的两个症状是**同一次 panic**:进度条一直转 + `std` 补全"消失"

用户在真的编辑器里看到两件事,并说"应该都是语义分析全面错了"。量下去发现:**不是两个缺陷,是一个**,
而且是我今天 §39 引进的。

### 症状 → 一个根因

```text
进度条一直转          pump 循环的条件是 pending_work() > 0,而它永远不为 0
补全里 std 什么都没有   客户端被告知 isIncomplete,它就把已有结果过滤/丢弃
```

而 `pending_work()` 不为 0 的原因:用户在项目的 `workspace_probe` 上跑一下,直接拿到

```text
thread 'cppls-index' panicked at src/sema/scopes.rs:2104:
a qualified name has at least one identifier
```

**索引线程当场死了。** 它平时看不见,是因为它跑在一个 detach 的 tokio 任务里:panic 只打到 stderr,
LSP 连接仍然活着、请求仍然有回答(回答的是半成品),于是界面上只剩一个转不完的圈。

### 为什么是我今天的代码:一个"看起来对"的 token 假设

§39 我写 `qualified_name_along` 时假设"名字节点的 `::` 是它的**直接** token"。对 `Widget::grow` 成立,
对 `Outer<T>::grow` **不成立** —— `<`、`T`、`>` 在一个**子节点**里,于是:

```text
1. 深度计数看不到尖括号 → 永远 0 → `<A::B>` 里的 `::` 被当成段分隔符
2. `last_identifier()`(只扫直接 token)对它也返回 None
3. 那一行写的是 .expect("a qualified name has at least one identifier")  →  panic
```

而 `Outer<T>::grow` 这种写法在 MSVC 的 STL 头里**到处都是** —— 这也解释了为什么只有用户的项目炸、
我的 fixture 不炸:fixture 里最长的是 `Box<int>::grow`,而 `Box<int>` 的 `<int>` **恰好是直接的**
`TemplateArgumentList` token……不,更准确地说:fixture 规模小、命中概率低。**这是"我的测试语料不够真"
的又一个实例**,和 §37 那次"探针把 `Ambiguous` 显示成 `None`"同类,只是这次代价落在用户身上。

### 修法:**读整棵子树的叶子,而不是直接 token**;并且**不许 panic**

```text
qualified_name(node) -> Option<..>     整棵子树的 token(按定义就是节点文本),边收集边丢 trivia
   ├ 在"字符 + 每个字符来自哪个 token"上读:最后一个深度 0 的 `::` 才是分隔符(注释里的 `::` 不在其中,因为 trivia 已丢)
   ├ 限定符 = 分隔符之前
   └ 名字   = 分隔符之后,必须是**一个标识符**;否则返回 None(析构/operator/转换函数走别的路)
```

**返回值从"元组"改成 `Option`**,`is_qualified` 为真但名字不是标识符时返回 `None` 而不是崩 —— 这是这条
路上唯一的 `.expect`。顺手查了 `sema/`、`index/`、`completion/` 三个目录非测试代码里剩下的 panic 点:
只剩三条,两条是"刚 peek 过"、一条是锁中毒,都是**真不可能**而不是**碰巧没发生**。

### 读数

```text
用户的项目(138 个文件)          workspace_probe 跑完,不再 panic
pending                          indexed 138 files in 1.45 s | pending 0        ← 进度条会停
std::str 打进去                   8 item(s),truncated false
                                 string / string_view 都在;first = [streambuf, streamoff, streampos, …]
```

最后那一行是**新加进 `editor_probe` 的一节**:用户报的是"打了一半的前缀",而原来的探针只问 `std::`
(空前缀)。**报告里的形状要照着复现,不能照着猜。**

### 回归测试

`tests/scopes.rs::a_qualified_name_with_nested_segments_is_read_without_panicking` —— 语料就是真头的形状:

```text
Outer<T>::Inner::grow     嵌套类 + 模板实参
Outer<T>::grow            模板实参
Outer<A::B>::grow         实参里还有 `::`      ← 直接 token 假设在这里最危险
Outer<Box<int>>::grow     实参里有嵌套模板
Outer<int>::count = 0     变量(没有 body,另一条路径)
```

断言名字落在正确的类里(`Outer` / `Outer::Inner`),而不是只断言"没崩" —— 一个返回空表的实现也能不崩。

### 顺带:把修好的服务器装到用户的编辑器里

用户跑的**不是** `target/release/cpp_ls.exe`,而是扩展目录里的一份**拷贝**
(`~/.vscode/extensions/cppcxy.cppls-0.0.1/server/cpp_ls.exe`,9 月 28 日的构建)。所以源码修好 ≠ 用户看到修好:

```text
1. cargo build --release -p cpp_ls
2. copy target/release/cpp_ls.exe → E:\vscode-cppls\server\cpp_ls.exe   (扩展源码在 E:\vscode-cppls)
3. npx vsce package --no-dependencies --out cppls.vsix
4. code --install-extension cppls.vsix --force
5. 校验:安装后的 exe 与 target/release 的 SHA256 相同,cpp_ls --version 退出 0
```

**这一步以前没有写在任何地方** —— 上面这段就是它现在的位置。装完必须**重载 VS Code 窗口**:已经跑着的
服务器进程是旧二进制,而且它启动时读过的索引还是坏的。

### 登记:下一件该做的事是**让泵的死亡看得见**

这次是两小时的排查,只因为"一个后台任务死了"在界面上表现为"一直在忙"。没有做的那一半:

* 泵**检测到不前进**时(连续 N 轮 `pending_work()` 不变),应当 `finish_progress_task` 并把数字写进日志,
  这样用户看到的是"加载完了,但有 N 个文件没读"而不是一个永动的圈;
* 更彻底的一条是给泵的每一步加 panic 边界。**我故意没做**:分析层在 panic 之后的状态没有人定义过
  (队列一致性、store 与 index 的对齐),把 panic 吞掉会让"索引处于未定义状态"变成"看起来正常" ——
  比一个死掉的泵更糟。要做就先定义那句话,再决定吞不吞。

---

## 44. `std::string` 的成员一个也补不出来:**MSVC 的 `<xstring>` 读不出来**

用户接着报:`std::string` 的对象用 `.` 访问,没有成员。**这次不是我今天引进的** —— `git stash` 掉今天全部改动
之后量同一个文件,数字一模一样。

### 最小复现(不依赖用户的项目)

```text
target/string_probe/main.cpp
    #include <string>
    int main() { std::string s; s. }
```

```text
declarations_in("std::basic_string") = 0
`s.` → 0 item(s), scope "std::string"      ← 对象本身认得出来(`type_at` 答 `std::string`),成员表是空的
```

### 三层量下去,定位到**渲染后的文本仍然读不出来**

```text
① <xstring> 的**原文**直接解析      240 756 字节 → 149 个 parse error,只有 2 个 scope、2 条 fact
                                    (宏这么多文件里,原文读不出来是预期的 —— 吃的是 cook 之后的渲染)
② cook() 成功                      CookedReading { declarations: 337, diagnostics: 0, mapped: placed 674 }
                                    但 337 条里 **没有** `basic_string`,scope 分布是 file scope 262 / std 50 / 其它 25
③ 读**整棵 unit**(154 个文件、499 344 个 token、75 个 error)
                                    `declarations_in("std::basic_string")` 仍然是 0,`s.` 仍然是 0
```

②是关键:**`cook` 没有失败,它成功了 —— 而它渲染出来的文本里 `basic_string` 的类体不存在。** 而且
`cook()` 的 `diagnostics: 0` 说明渲染后的文本本身是"合法"的:它不是读不动,是**没有那个类**。

### 为什么渲染会丢掉那个类:SAL 注解的展开

用逐段前缀解析定位第一个出错的构造,落在 `_Char_traits::find` 的参数上:

```cpp
_NODISCARD static _CONSTEXPR17 const _Elem* find(
    _In_reads_(_Count) const _Elem* _First, size_t _Count, const _Elem& _Ch) noexcept /* strengthened */ {
```

MSVC 的 SAL 注解在 `sal.h` 里是一条**连锁宏**:

```text
_In_reads_(size)  →  _SAL2_Source_(_In_reads_, (size), …)   →  _SA_annotes3(SAL_name, #Name, "", "2") _GrouP_(…)
                     _SA_annotes3 有三个分支:空 / __declspec("…") / [SAL_annotes(…)]
                     _GrouP_ → _GrouP_impl_ → … → _SAL_nop_impl_ → 空
```

`preprocess/cooked.rs` 里写着"**函数式宏一律展开**、没有参数表的才跳过",SAL 正是函数式宏 —— 也就是说这条
链**本该**被展开成 `__declspec("SAL_name(...)")` 或空。而实际渲染出来的文本里它**没有展开完**,留下一个标识符
后面跟括号,解析器于是在这一行断言失败(**"expected ), but get identifier"**),恢复过程从此丢掉了后面的类体。

单独把三个候选形态喂给解析器验证过,三个都**能**读:

```text
const char* find(_In_reads_(_Count) const char* _First, size_t _Count) noexcept;     0 errors
const char* find(__declspec("SAL_name(SAL_annotes,)") const char* _First, …);        0 errors
const char* find([SAL_annotes(Name=SAL_name)] const char* _First, …);                0 errors
```

**所以问题不在"解析器不认 SAL",而在"这条宏链没有被展开成任何上面三者之一"。** 下一步要量的就是那条链断在
哪一环(是 `sal.h` 的多分支重定义按最后一条生效、还是链中间某一环没有带参数表而被跳过),这也是 §42 里
"模板实参内 `::`"那类"渲染与原文的契约"问题的同一个家族。

### 这次我犯的错:**`Remove-Item` 删掉了未提交的 fixture**

清理临时探针时,我顺手把 `crates/cpp_code_analysis/tests/fixtures/` 也删了 —— 而它**从未被提交过**
(每一轮 `git status` 都显示 `??`),所以 git 里没有、回收站里也没有(git 的 `rm`/PowerShell 的
`Remove-Item` 都不进回收站)。**13 个 fixture 文件(§36–§40 全部的真编译器语料)被我清掉了。**

能救回来的只有两份:`target/modules/local_math.h`(`build_header_units.bat` 复制过去的)和当时构建出的
`.ifc`/`.obj`(只是产物)。其余 12 个按本会话的记录逐个重写,并用真编译器重新验证:

```text
build_modules.bat      → 7 12 12     (main.cpp 里三个函数的结果都对)
build_header_units.bat → 42
build_partitions.bat   → 14 12 16
模块套件               40 条全绿
```

重写时还发现**原文里记错了一件事**:§36 起一直写 `build_modules.bat` 打印 `7 7 12`,而 `multiply(3, 4)`
是 12 —— 三处 `7 7 12` 已改成 `7 12 12`。这条数字从来没被复读过,直到文件被删掉、必须重写时才被算了一遍。

**教训(写给下一次)**:`target/` 下的脚本和产物可以随便删,**源码树里未提交的东西不能**。清理之前先看
`git status` 的 `??` —— 那些是删了就回不来的。这条比 §43 的 panic 更贵,因为它丢的是**证据**,而证据重建
之后就不再是"当时量到的那个东西"了。

### 登记:下一件该做的事

| 活 | 为什么 | 起手式 |
| --- | --- | --- |
| ~~**SAL 宏链的展开**~~ **已做,见 §45** | `std::string`/`std::vector`/所有 STL 类的成员补全都靠它;这是用户能看见的最大缺口 | 打印 `cook` 出来的渲染文本,搜 `_In_reads_`;对比 `sal.h` 的三条 `_SA_annotes3` 分支哪一条在 `__cplusplus`/`_MSC_VER` 下生效,以及链中间哪一环没被展开 |
| **把 fixture 提交** | 它们已经是 40 条测试和三个 `build_*.bat` 的依据,却一直是未跟踪状态 | `git add crates/cpp_code_analysis/tests/fixtures` —— 下次再有人清理临时文件,它们不会消失 |
| **渲染与原文的契约** | ②里 `diagnostics: 0` 而类体不见了,说明"渲染成功"不等于"渲染对了";这类丢失现在没有任何检查会发现 | 一个"渲染后文本必须包含源文件里每个类名"的自检(至少对 `_EXPORT_STD` 这类宏包裹的声明) |


---

## §45 SAL 的结论:宏链是好的,病在**解析器**与**失败模式**

§44 把这条路留成"渲染没展开完",并要求先量 `sal.h` 那条链断在哪一环。**量完了,结论与登记时相反。**

### 45.1 渲染是好的

一个真实工程(`main.cpp` 只 `#include <string>` 与 `<vector>`)的单元渲染:**1 175 164 字节**
(`<xstring>` 原文 170 303 字节),里面 `class basic_string` 出现 **1 次**、`_Mystr` 18 次、
**`_In_reads_` / `_SA_annotes3` / `_SAL2_Source_` 各 0 次** —— 宏链**展开得很干净**。
把那份渲染单独喂给解析器:**0 个错误,`basic_string` 的类节点 93 971 字节,类体完整**。

所以 §44 的"渲染成功而类体不见了"既不是渲染的问题,也不是解析器读不动那个类 —— 是**别处的一处错误把整份文件的作用域搞坏了**。

### 45.2 真正的读数:一条错误 → 整个翻译单元

逐层定位(落盘单元流、解析、看跨度、二分)找到的是**同一个失败模式**,而不是一个语法缺口:

```text
一处解析错误(某条 MSVC 头里的写法解析器不认)
  -> 恢复时留下一个没有配对的 {
  -> 那个 { 属于 namespace vc_attributes(sourceannotations.h)或 class _Search_fn(xutility)
  -> 之后拼进来的每个文件都成了它的成员
  -> std::basic_string 的限定名成了 vc_attributes::std::basic_string
  -> std::string 在任何地方都找不到声明(不是"找到了但类型错")
```

这不是"某个类没索引到",是**整份单元读数的作用域被一处错误毁掉**。所以每个语法缺口的代价都被放大了三个数量级。

### 45.3 修好的形状(全部在 cpp_parser,每个都有测试)

| 形状 | 原来的读法 | 现在 |
| --- | --- | --- |
| `[repeatable] [source_annotation_attribute(All)] struct X { ... };` | 单括号被当成**下标**,恢复留下未闭合的 `{` | `at_a_single_bracket_attribute`:只在**声明**的说明符位置认它(判据:标识符 + 可选一层配平括号 + `]`) |
| `class [[nodiscard]] X { ... };` | 属性在**名字之前**,类被读成**匿名**、类体丢给语句规则 | 类头在名字之前先吃属性 |
| `(ts + ... + init)` | 只认 `ts + ...` 与 `(... + ts)`,**二元折叠**把第二个运算符留给调用者 | 补上重复运算符那一半 |
| `requires C<T> [[nodiscard]] T f();` | 约束里的 `[` 被当成下标,约束失败,声明失败 | 约束内部:后缀循环见到 `[[` 停下 |
| `[[msvc::constexpr]] return ::new (...) _Ty[1]();` | 语句位置的属性:**没有**语句规则能从这里开始,声明读不出类型、表达式读不出操作数 | `parse_stat` 在语句之前读属性,再读它后面的语句 |

读数(同一个真实工程,106 个文件):

```text
单元解析错误   61 -> 0
被隔离的文件   sourceannotations.h, xutility, type_traits, xmemory  ->  type_traits
```

### 45.4 归属问题:两个真 bug,一个还没定位

单元解析现在是 **0 错误**,`std::string` 与 `std::vector` 都作为事实出现在熟读里 —— 但 `members_of("std::string")`
仍然失败。追下去挖出两个真 bug,和一个还没定位的:

**(a) 栅栏把字节删了,坐标系就错位了。** `RenderedUnit::without` 原来**删除**被隔离文件的 token 并重建 span 表,
于是 2 111 390 字节的流变成 2 104 969 —— 而**后面的解析仍然用原来那张 span 表**去映射。6 100 字节的位移让之后
每个声明都归到错误的文件上。现在改成**用等长空格抹掉**,三条性质写进了 `without` 的文档与
`taking_a_file_out_of_the_stream_keeps_the_offsets` 这条测试:

```text
文本长度不变        结果里的偏移就是原文里的偏移
spans 是原来那一张  于是 written_span 回答的仍然是还在那里的文本
token 从解析里消失  于是那个文件只贡献长度
```

**(b) 我上一轮的归因是错的 —— `written_span` 本来就是对的。**

我上一轮报"`basic_string` 归到 `__msvc_formatter.hpp`"。追下去发现:`written_at(1858469)` 返回的**就是**
`xstring`(偏移 22677),93 971 字节的类体确实在自己的文件里。我看到的那条事实是该文件里
`class basic_string;` **前置声明**,**一件正确的事**。

这一轮我把"文件由 `span.written`(导航提示)决定,而不是 `span.file`(token 站在哪个文件)"当成一个真 bug
去修 —— **前提是错的**。`ExpandedToken::diagnostic_range` 对宏展开 token 返回的是**最外层调用点**,也就是
**调用文件**里的位置,所以 `span.written` 本来就在 `span.file` 的文本里,两个字段一致。用
`a_declaration_starts_in_the_file_it_stands_in` 那个形状(整个声明由一个跨文件宏展开)量过:旧实现和新实现
给出**同一个答案**,给的都是调用点 `DECLARE_WIDGET`。

所以这一轮**改了实现又按事实收了回来**,只留下真正新增价值的部分:

| 留下 | 为什么 |
| --- | --- |
| `RenderedUnit::file_lengths` | 每个文件自己的文本长度,和 `files` 平行;没有它就无法把"回答落在它命名的文件里"变成一次比较 |
| `RenderedUnit::span_lands_in` | **这条不变量从来没有人检查过**。`file_what_was_found` 现在对每条事实的两半都查,不通过就计入 `unplaced` —— 一条"范围跑出它命名的文件"的答案会被**计数**,而不是被当成答案 |
| 那条测试 | 钉住的是**不变量**而不是某个过去的错:`diagnostic_range` 一旦改成返回宏体位置,它就会红 |

这一条记下来的教训比代码本身值钱:**我连着两轮把"我读数的办法"当成了"产品的缺陷"**。第一次是归因方式错
(把树偏移当文件偏移读),第二次更贵 —— 我按一个没验证的前提改了实现。

**(c) 找到了,而且是一个应用层的真 bug:`mentions_a_qualified_name` 会走进类体。**

`std::string` 查不到的**唯一原因**是这个,和宏、和归属、和栅栏都无关:

```text
declaration_is_qualified(class basic_string { … })      = true       ← 错在这里
  → names_a_scope_this_file_has                          = false      (限定符是 `allocator_traits`,不是本文件的 scope)
    → declaration_parts 直接 return
      → 整个类定义被丢掉:没有绑定、没有 fact、`unplaced` 也是 0
```

`declaration_is_qualified` 的规则本身是对的 —— 它拦住 `int ns::Widget::count = 0;` 在文件作用域绑出一个
`count`。但 `mentions_a_qualified_name` 的走法只避开了 **declarator**,没有避开**类体**。而
`class basic_string` 是 94 KB 的成员,成员里到处都是限定名(`allocator_traits<_Alloc>::…`、
`pointer_traits<pointer>::…`),于是"这个类定义自己写了限定名"被判为真。

**任何类体里出现 `X::y` 的类都会丢**,这是整类形状,不是 MSVC 特有的。

修法是让那个走法**不进入属于别的声明的子树**(`ClassBody` / `Declaration` / `CompoundStat`):本声明的名字只
可能写在说明符序列或 declarator 里,两者都在所有 body **之外**。

读数(同一个真实工程,106 个文件,零回归):

```text
                修前        修后
cooked(xstring) 157 条  →  1 011 条   basic_string 的类体 129 939 字节
cooked(vector)  273 条  →  1 048 条   std::vector 的类体 76 664 字节
members_of("std::string")     NotDeclaredHere  →  204 个成员
members_of("std::vector")     0 个成员        →  176 个成员
type_at(s)                    No              →  Yes(std::string)
declarations_in("std")        1 157           →  1 259
```

回归测试 `a_class_whose_members_write_qualified_names_is_still_declared`(三种形状:成员类型限定、默认实参限定、
嵌套别名限定),并且**验证过它会失败**:去掉那个 guard 之后它报 `bindings: []`,正是这个 bug 本身。

这一轮的教训还要加一条,而且是前两轮那条的根源:**"未定位"往往是"还没问到那个判定"。** 我前两轮一直在读
`build_scopes` 的**结果**(绑定数、scope 树),而没有去读那个**决定要不要绑**的布尔。一旦把
`declaration_is_qualified` / `names_a_scope_this_file_has` 这两个值打出来,答案立刻就在那里。

登记 —— **这两项已被 [`plan-frontend.md`](plan-frontend.md) 接管**,那里给了路线图和里程碑:

| 活 | 去向 |
| --- | --- |
| `type_traits` 仍被隔离 | M0(给隔离文件补它没关的 scope 的闭合符)+ M6(语法);不再是"唯一剩下的未闭合作用域"这种孤立说法 |
| **失败模式本身** | M0 的"拆闸门":跨文件括号对**不再是拒绝整份读数的理由** |

七个形状都是**小语法缺口**,而它们的代价全部来自**恢复时留下未配对的作用域** —— 以及归属错开一个、和一个
布尔判错之后**没有任何检查会发现**。

**这一节最终留下的判断是:补语法是对的,但收益被下游两道"全有或全无"的闸门吃掉了。** 空工程
`#include <format>` 的实测(`crossings 0` 却仍有 5 个文件被隔离、`std::format` 与 `std::string` 都
`NotDeclaredHere`)说明:决定成败的不是错误数量。下一步不再从语法开始,而是按 `plan-frontend.md` §5.1
的 M0(拆闸门)→ M1(对齐真编译器做验收)→ M2(工具链发现与内建宏)走。

### 45.5 本轮验证

`cargo test --release --workspace`:**49 个二进制全绿**;`clippy` **0 警告**。
新增测试:`a_class_whose_members_write_qualified_names_is_still_declared`(去掉修复后会红)、
`a_declaration_starts_in_the_file_it_stands_in`、`taking_a_file_out_of_the_stream_keeps_the_offsets`、
`a_construct_the_grammar_refuses_does_not_take_the_rest_with_it`(七种形状,断言**两个 namespace 仍然平级**)、
`modern_constructs_produce_the_right_nodes` 里 requires+属性一条、以及 `session.rs` 里两条(单括号属性**不再**需要隔离;
真泄漏的文件仍然被隔离,用一份**文字配平但解析不配平**的 fixture)。