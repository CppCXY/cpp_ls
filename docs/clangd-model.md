# 抛弃"按文件建索引"：clangd 的模型与我们该怎么改

**结论先行。现在这套设计的问题不在性能工程，在模型：我们把"索引"当成了一个必须先建完、且以文件为单位
建造的全局数据结构。clangd 的索引是一个**可缺失、按 TU 建造、按 TU 缓存**的旁路数据，答案来自 AST 加
已有的索引。它启动时不需要"把项目读完"。**

```text
1. 索引单位：从"每个文件一份摘要"改成"每个 TU 一份产物"（一次解析，facts 带来源文件）
2. 索引分两层：FileIndex（打开过的文件，随编辑刷新） + 后台全量索引（按 TU、按盘缓存）
3. 答案不等索引：诊断/补全来自打开文件的 AST（我们的 view + cook），索引只补充跨文件信息
4. 删掉：第二遍、provisional summary、`pending_work == 0` 这道全局门控、按文件的闭包走查
5. 我们已经有这两块拼图：`index_unit_rendering`（一次 TU 解析产出按文件的 facts）
   和 redb 里按 program hash 缓存的 unit / indexed_unit —— 它们是 `.idx` 的同构物
```

参考（外部资料）：

```text
clangd 的索引设计          https://clangd.llvm.org/design/indexing
clangd 的线程与请求模型     https://clangd.llvm.org/design/threads
clangd 代码                 https://github.com/llvm/llvm-project/tree/main/clang-tools-extra/clangd
```

---

## 一、clangd 实际怎么做的（按它的设计文档）

```text
索引接口     SymbolIndex，多个实现用 MergedIndex 叠起来，功能层只看见一个索引
Symbol       (SymbolID, 名字, 声明/定义位置, 文档, 补全属性)，同名同物 → 同一 SymbolID 合并
Ref / Relation  引用与继承等关系，按 SymbolID 查

FileIndex（动态层）
  只收"打开过的文件及其 include 的头文件"，随解析回调逐文件更新；
  两种存储/回调分开：昂贵的 preamble 重建 vs 频繁的主文件重建
  作用：头文件里的全局补全、后台索引没建完时的跨文件跳转、编辑中位置不陈旧

BackgroundIndex
  后台线程池，从编译数据库给每个**源文件**入队
  建索引前先看磁盘上有没有 `*.idx` 缓存（key 是 TU 的依赖内容），有就跳过；建完写回
  位置：compile_commands.json 旁边的 `.cache/clangd/index/`；没有 CDB 的头文件放在用户缓存目录

Static / Remote index
  离线构建或远端服务，用来避免后台索引的等待

线程模型
  主线程只做分发，不在 ClangdServer 上阻塞
  每个打开的文件一个 ASTWorker（队列 + 线程）：
    丢弃过期操作（被取消的读、连续两次写）→ 执行仍有效的第一个操作
    写：重建 AST（必要时重建 preamble）并发布诊断；读：把 AST 交给回调
  **完成一次 AST 构建的会话不是"索引建完"，而是"这个文件可以回答了"**
  防抖：写不立刻开始，等一次读或一小段时间
  补全：不复用 AST，另起一次带 completion token 的解析，不等待最新 preamble

preamble
  include 区域单独编译成 PCH；只有 include 区域变了才重建（可能几十秒）
  有了最新 preamble，主文件的 AST 重建是**亚秒级**；preamble 是不可变字节，可跨线程共享
```

三条对我们最要紧的性质：

```text
① 索引可以**不完整**：查不到就查不到，答案是"已知的符号 + 打开文件的 AST"，不是"等它完整"
② 索引的产物是**每个 TU 一份**，缓存在盘上，未变则启动时不用重建
③ 诊断来自**打开文件的 AST**，从不来自索引 —— 所以不需要"索引好了才敢报"的门控
```

## 二、我们的设计与之逐条对照

| 维度 | clangd | 我们（现状） | 后果（实测） |
|---|---|---|---|
| 索引单位 | 每个 TU 一份产物 | **每个文件一份摘要** | 1 761 份摘要，每份都要单独解析 |
| 事实来源 | 一次 TU 解析，符号带文件位置 | 先按文件解析（无宏环境），再**第二遍**带环境重解析 | 冷启动 131 s：第一遍 12.8 s + 重读 66 s + 走查 36 s |
| 索引完整性 | 可以缺，按需补 | **必须先读完**（门控 `pending_work == 0`）才有检查项 | 状态栏 54 s（已修成 17 s，但底层仍要读完） |
| 缓存单位 | 每个 TU 的 `.idx`，依赖内容哈希 | 每份摘要的 `(content, config, env)` 行 | 热启动 3.2 s，但**冷启动无法分摊** |
| 增量 | preamble（PCH）复用，主文件亚秒重建 | 每次重新渲染 + 重新解析（渲染已缓存） | 每次编辑 10 ms（这块我们其实不差） |
| 诊断来源 | 打开文件的 AST | 同样来自文件自己的树，但**检查项**读索引并被门控 | 第一屏诊断被索引阻塞 |
| 并发模型 | 每文件一个 ASTWorker，丢弃过期写 | 一个全局泵按项目队列切片 | 编辑与索引互相排队，需要写锁预算兜底 |

**根本分歧就一句**：我们把"索引"当成了**答案的前提**；clangd 把索引当成**答案的补充**。
这一句决定了后面所有的复杂度：provisional summary、第二遍、全局门控、写锁预算、切片大小调参。

## 三、目标架构（映射到本仓库）

```text
A. 索引产物 = TU 产物（已有能力，改默认路径）
   `FileIndexer::index_unit_rendering(root, stream, key)` 已经是"一次解析、facts 按来源文件归档"，
   产出 `IndexedUnit { files: Vec<(PathBuf, CookedFile)>, ... }`。
   现在它只在 examples 与 `read_the_unit` 里用；要把它变成**唯一的索引路径**。
   删除：`advance` 里按文件的 `prepare` + `index_one` 作为索引来源的地位（保留它给"打开文件的即时答复"）。

B. TU 产物的盘上缓存（已有，差最后一层）
   redb 里已有两张表：`units`（渲染文本，键 = closure 内容哈希）与 `unit_indices`（facts，键 = 程序哈希）。
   这就是 clangd 的 `*.idx`：启动时按内容哈希校验，未变就只解码。
   实测：`read_the_unit` 冷 6.5 s → 暖 91 ms。

C. 两层索引
   FileIndex 层：打开的文件 + 它的 include 闭包，随编辑刷新（我们有 `insert_cooked` 与 cook，正好是这个）。
   Background 层：按 TU 逐个建，键与盘缓存同上；**不阻塞任何请求**。
   查询：先 FileIndex，未命中的符号去 Background 层；都没有就是没有。

D. 答案不再等索引
   诊断：来自 `FileView`（已有）+ 检查项 —— 检查项读索引是它的本性，但**门控改成"这个文件的读数已终态"**
   （已有一半：`its_closure_is_owed`）。索引不完整时，缺的成员列表就是缺，报出来的规则要按"不确定"处理，
   而不是把整份诊断压住。

E. 并发
   把"全局泵 + 项目队列"换成"每打开文件一个任务 + 一个后台索引任务"：
   打开文件的任务只做它自己的 cook/解析（毫秒级）；后台任务按 TU 推进，可被取消、可让位于请求。

F. 删除清单
   · `parsed_since_the_last_pass` 与第二遍（`prepare_the_re_read` / `commit_the_re_read`）
   · "provisional summary" 这个概念本身（TU 产物要么对应当前内容，要么不存在）
   · `pending_work()` 里的 backlog、以及诊断对它的门控
   · 按文件的闭包走查（`units_for_the_pass` 的每文件一跳）
   · `RE_READ_SLICE` / `COOK_SLICE` / 写锁预算这一族调参旋钮（模型变了，它们失去意义）
```

## 四、迁移步骤（每步可独立合入，带验收）

```text
M-a  把 TU 产物作为**默认索引路径**，先只对"项目自身的源文件"生效（198 个 root）：
     它们各自的 TU 读一次，facts 写进已有的 index；header 的摘要**不再单独生成**。
     验收：冷启动不再有第二遍；`std::string` 之类跨文件查询与今天等价或更好；
           本项目 tests 全绿（`checks`、`scopes`、`modules`、`semantic`）。

M-b  盘上缓存生效：已建过的 TU 只解码（今天 91 ms/个）；启动时后台只补变化的 TU。
     验收：冷启动首屏 ≤2 s（打开文件的 TU 已缓存）；全项目索引在后台完成，日志里不再有阻塞。

M-c  查询层两层化：FileIndex（打开文件）+ TU 索引（其余），缺失即缺失。
     验收：删掉诊断的全局门控后，第一屏诊断 ≤1 s 且不出现"另一台机器上才有的"假报。

M-d  删除第二遍、provisional、全局门控与相应旋钮；把 `docs/edit-latency.md` 的测量重做一遍。
     验收：代码量净减少；每一项删除都有对应测试改写而不是删除覆盖。
```

## 五、成本模型（用已有实测推）

```text
一次 TU 读（LuaSource.cpp 的 110 文件闭包）  渲染 55 ms（已缓存） + 解析 399 ms + sweep 3.8 s
198 个 TU × 4.4 s ≈ 15 min —— 这是"全项目索引"的**一次性**成本，clangd 同样付（它对每个 TU 跑一遍 clang）
但它发生在**后台**，且缓存后第二次启动只解码：
  98 % 的 TU 未变 → 198 × 91 ms ≈ 18 s（后台，且可被取消）
  首屏：打开文件的 TU（缓存命中 91 ms / 未命中 4.4 s）+ 它的 FileIndex 层
对比今天的 131 s **阻塞式**启动，差别不在总 CPU，在"谁在等"。
```

## 六、不能照搬的地方（诚实清单）

```text
· 我们没有 PCH。clangd 的 preamble 是编译器给的；我们的等价物是"渲染 + 缓存"，粒度是文件而不是字节码，
  所以"主文件亚秒重建"这条我们靠 cook（已实测 10 ms/次编辑）而不是 preamble 实现。
· 我们没有 CDB 的项目（MSVC/VS 生成器）没有"每个源文件的编译命令"。clangd 的 BackgroundIndex 从 CDB 入队，
  我们不能假设它存在 —— 已有的"配置阶梯"（docs/no-build-database.md）是这一层的替代。
· 手写 parser 的 sweep 成本远高于 clang 的 AST 遍历：3.8 s/TU 是关键约束，所以 M-b 的缓存必须先到位，
  否则 M-a 会让冷启动更慢（198 × 4.4 s）。
· 符号合并：clangd 靠 SymbolID（USR）合并同名同物；我们靠 (qualified name, kind, 位置) 的近似，
  跨 TU 合并会有精度损失 —— 这是既有问题，不因这次重构而消失。
```

## 七、与既有文档的关系

```text
docs/refactor-cache-and-startup.md   缓存与启动的分阶段计划（M1–M5），本文件是它的模型替代方案
docs/prefix-reuse.md                 解析复用（绿树共享）；在 TU 模型下它的价值下降（TU 只解析一次）
docs/edit-latency.md                 编辑延迟的实测与第二遍的由来；M-d 之后要按新模型重测
docs/no-build-database.md            没有 CDB 时配置从哪来 —— M-a 的前提
```

## 八、一段必须先算清楚的算术（2026-10-22 晚，写进这里以免照着上面的顺序盲做）

上面 §五 的成本模型按"后台、可缓存"论收益。实测两个数字之后，**在冷启动上 TU 模型比现在的按文件设计更慢**，
必须先把这条写清楚：

```text
read_the_unit 冷（LuaSource.cpp 的 110 文件闭包）   6.5 s
read_the_unit 暖（渲染与 facts 都在库里）            91 ms

项目自身的源文件（root）数                          198
  → 冷：198 × 6.5 s ≈ 21 分钟   ← 比今天的 131 s 冷启动慢 10 倍
  → 暖：198 × 91 ms ≈ 18 s      ← 比今天的 7 s 热启动慢，但只是后台的事
```

原因：TU 解析要**重新解析整个程序**（399 ms 解析 + 3.8 s sweep），而我们的第二遍是
"按文件重解析 + 复用一个已缓存的时间线视图"（每个文件 ~170 ms）。**冷启动时按文件更便宜**，
这是反直觉但已经量到的。

所以 M-a（TU 成为默认索引路径）**不能替换冷启动路径**，它只能在"已经建过一次"之后有价值。
clangd 敢这么做，是因为它的冷建在后台、按文件粒度增量（每个 TU 一个 `.idx`），而且它的
编译前端比我们的 sweep 快一个数量级。

**修正后的顺序**：

```text
1. 保留按文件的第一遍（冷 12.8 s）——它是我们最好的冷建路径
2. 冷启动的剩余成本（第二遍 66 s + 走查 36 s）只能靠"少做"而不是"换个模型做"：
   要么接受它（后台 100 s，用户已经在第一屏拿到答案），要么按 span 区段做增量 sweep
3. TU 产物留给"M-a 之后的新会话"：已有缓存的 TU 在后台以 91 ms 的速度补齐全项目索引，
   让跨文件查询的质量逐步达到 TU 水平 —— 这是收益，不是启动速度
```

**第一屏实测（与上面的争论无关，单独说清）**：

```text
冷缓存  open 273 ms → did_open 3 ms → 一次切片 377 ms → diagnostics 58 µs（0 个检查项，队列还剩 1253）
热缓存  open 546 ms → did_open 6 ms → 一次切片 237 ms → diagnostics 71 µs
```

也就是说**第一屏早就不慢了**：诊断接口 0.4 s 内就能回答，只是检查项被"闭包还没读完"门控着。
用户的观感来自状态栏（第一遍 12.8 s）+ 后台 118 s 的 CPU。

## 九、根因排序：为什么"简单的项目也要等这么久"

不是调参问题，是四处结构性成本叠加。用 EmmyLuaCodeStyle（1 761 个被索引文件，其中绝大多数是 STL 闭包，
项目自身的源文件只有 198 个）的实测：

```text
冷建 131 s
├── 第一遍：逐文件解析            12.8 s   （7 ms/文件）
├── 第二遍的单元走查              36 s     （951 次闭包走查 × 38 ms）
└── 第二遍：带环境重解析          66 s     （388 个文件 × 170 ms）
```

### 根因 0：索引单位是"文件"，而文件数由 STL 闭包决定

20 个源文件的项目，工作项不是 20 个而是 1 761 个。clangd 的工作项是**源文件的 TU**（这里是 198 个），
头文件的符号由包含它的 TU 顺带产出。**只这一条就把工作量放大了约 9 倍。**

### 根因 1：一次 TU 读 4.4 s，于是不敢按 TU 索引

```text
read_the_unit 6.5 s ≈ 渲染 899 ms + 解析 399 ms + sweep 3.8 s + 其余
```

因为付不起 198 × 6.5 s，才有了"按文件一遍 + 第二遍补偿"的设计。**第二遍和第二遍的走查，
是这个 4.4 s 的下游产物，不是独立的问题。**

### 根因 2：预处理与解析是两遍（渲染 → 再解析）

`cook` 先把文件渲染成宏展开后的文本（899 ms/TU，17 ms/文件），`CppParser::parse` 再把这份文本解析一遍。
**两遍都是 O(tokens)**，而真实编译器是一遍：解析器直接读指令流、按需展开宏。

### 根因 3：facts 抽取是"整个程序 × 每次 TU"

`index_unit_rendering` 对每个 TU 都把整个程序里 306 000 条声明重新抽取一遍，每条带若干 `String`
（名字/作用域/类型/返回类型……）。没有 interning、没有 arena、没有按文件增量。
**3.8 s/TU 里绝大部分是分配，不是解析。**

### 根因 4：宏环境在解析之后才知道

第一遍解析时闭包还没索引完 → 拿不到宏表 → summary 是 provisional → 必须第二遍。
**如果宏表能在解析前算出（它本来就只依赖闭包的 `#define`），第二遍根本不需要存在。**

## 十、要达到"简单项目 1 秒内打开"，必须做的三件事

按投入产出排序，全部是**结构性**的：

```text
① 一遍完成预处理与解析（删掉渲染这一步）
   解析器直接消费指令、按需展开宏；`preprocess::cooked` 整个"渲染文本 + span 映射"子系统随之消失。
   收益：每次 TU 读 −899 ms；每次编辑的 cook（17 ms）也不需要了。
   代价：parser 要长出指令处理（`#if` 求值、`#define` 作用域、`#include` 递归），这是最大的单项工作。

② facts 按文件增量 + 字符串不再逐条分配
   一个 TU 解析完，只为**内容变了的文件**重建 facts（文件在渲染流里的区段是已知的）；
   名字/作用域/类型进 arena 或 `Arc<str>`，跨 TU 共享。
   收益：sweep 3.8 s → 目标是 ~200 ms；内存也从每 TU 数十 MB 降下来。

③ 索引单位改成 TU，并只索引项目自身的 TU
   198 个 TU（简单项目 20 个），每个产物按 program hash 缓存（已有）。
   收益：第一次打开项目的后台建库从 131 s 降到 ~20–40 s；之后每次启动只解码（已有 91 ms/TU）。
   首屏：打开文件的 TU = 一个 ①+② 之后的 TU 读 ≈ 0.6–0.7 s。
```

做完 ①②③ 之后，今天这套"逐文件 + 第二遍 + 闭包走查 + provisional + 全局门控"可以**整块删除**，
而不是继续在上面打补丁。

**在那之前**，任何"把 131 s 调成 110 s"的改动都不值得做 —— 这一轮我做的四个实验（切片大小、第二遍延后、
includer 取根、并发度）全部没有收益，就是这句话的实证。

## 十一、把预处理并进 parser：怎么做，以及为什么它不该是第一步

### 11.1 现在的四遍

```text
lex 原文                     24 ms / 594k tokens
render：宏展开 + 生成新文本    913 ms / TU   ← `preprocess::cooked`，产出 (text, 594k spans)
lex 渲染文本（parse 内部）     24 ms
parse 渲染文本                363 ms
sweep：抽 facts + 按文件归属   约 3 200 ms / TU
```

### 11.2 parser 一遍化要长出来的东西

```text
① 词法层之上加一个 `TokenStream`，`current_token()` / `bump()` 的语义不变（几千个调用点不动）：
   它在内部负责：跳过指令、求值条件、展开宏、跨文件入栈。
② `#if/#ifdef/#elif/#else/#endif`：用一个条件求值器（`preprocess::condition` 已有）在当前宏表上求值，
   取一个分支，其余 token 直接跳过 —— 不产生节点，也不进流。
③ `#define/#undef`：写进**活的**宏表（`MacroTable` 已有），位置就是当前 token 的 offset。
④ `#include`：把被包含文件的 (text, tokens, cursor) **压栈**，返回时弹栈；
   文件边界、`#pragma once`、include guard 都变成栈上的状态，`with_file_boundaries` 可以删掉。
⑤ 宏展开：解析器取下一个 token 时，如果它是宏就按需展开到一个小队列里（带递归 hideset，
   处理 `##`/`#`、`__LINE__`）；解析器随后像读源码一样读这个队列。
⑥ **坐标**：展开出来的 token 直接带"写在哪个文件、哪个 offset"，所以 `RenderedCooked.spans`
   和 `ReadingOffset ↔ FileOffset` 的映射整套消失 —— 这是最大的简化，也是现在很多 bug 的来源。
```

风险与工作量：`peek_token_kind_at`、`joined_operator_at_the_cursor`、`split_closing_angle`、
`is_a_known_type_name` 这些按**下标**前看的地方都要改成按流前看（展开会改变"第 n 个 token"的含义）。
这是 1–2 周、且几乎每个语法规则都要回归的工作。

### 11.3 但测量说：它只值 20 %

```text
一次 TU 读 4 551 ms = render 913 + parse 387 + sweep 约 3 200 + 其余
parser 一遍化能省下：render 的文本生成与 span（约 500–700 ms）+ 一次 lex（24 ms）
省不掉的：解析本身 363 ms、sweep 3 200 ms
```

**所以第一步不是 parser，而是 sweep。** 3 200 ms 里做的是：
`index_tree` 把整棵树走一遍、为**每条声明**造一个带多个 `String` 的 `DeclFact`；
然后 `file_what_was_found` 再走一遍，按 span 把它们归属到文件。

### 11.4 sweep 的四刀（按收益）

```text
A. 归属合并进同一次遍历：现在两遍（造 facts、按文件归属）本可以一遍，省约一半
B. 只为"内容变了的文件"重建 facts：树是按渲染流建的，而每个文件在流里的区段已知
   （`RenderedUnit.files` + `file_lengths`），所以可以按区段遍历（rowan 的 range 查询），
   整个文件跳过而不是重新归属 —— 这是**编辑后**也能快的关键
C. 字符串 interning/arena：306k facts 里 `std`、`size_t` 这类名字重复几万次，
   改成 `Arc<str>` 或 per-TU arena，分配量能降一个数量级
D. 不值得的：`skip_bodies` 早测过（neutral），不要再试

目标：sweep 3 200 ms → 300 ms 量级，TU 读 4.5 s → 约 1.6 s；
而**编辑后**（B 生效）一个 TU 的增量 sweep ≈ 0。
到那时再谈 parser 一遍化（把 1.6 s 里的 1.3 s 变成 0.6 s），顺序不能反。

## 十二、正确的方向：索引是一份"浏览数据库"，不是一份"精确语义"

### 12.1 微软自己的文档说明了 IntelliSense 为什么快

Visual Studio 的索引用的是 **C++ Browsing Database Parser**，微软的文档写得很直白
（[Hint Files](https://learn.microsoft.com/en-us/cpp/build/reference/hint-files)）：

```text
"The C++ Browsing Database Parser is a fuzzy parser that can parse large amounts of code in a short amount
 of time. One reason it's fast is because it skips the content of blocks. For instance, it only records the
 location and parameters of a function, and ignores its contents."
```

它**按源文件逐个解析**（不按 include 闭包），**不展开宏**，**跳过函数体**，并且在解析不了的地方
"uses error recovery techniques to ignore indecipherable source code"。
`_STD_BEGIN`、`BEGIN`、`NOEXCEPT` 这类宏带来的问题，用一个**固定的 hint 文件**解决 ——
里面只有 `#define X` 这样的替换，一次写好、随工具链走，不是每个文件算一遍环境。

CLion 的 stub index 与此同构：每个文件一份 stub（只有声明、没有函数体），可增量重建；
cpptools 的 tag parser 也是每个文件一次模糊扫描，加上一个 browse.path。

**三家的共同点：索引是"名字 + 位置 + 类别"的堆，允许缺失，从不为一个文件做编译器级的预处理。**

### 12.2 我们违反了其中每一条

```text
· 按 include 闭包展开宏（每个文件一次 render，17 ms）
· 要求 facts 的 scope 精确（`std::` 必须正确）→ 拿不到就 provisional → 第二遍
· 不跳过函数体（我们连 body 的语法树都建）
· 索引建完才敢出检查项
```

### 12.3 实测：按微软的方式做，这个项目 0.77 秒

临时探针：1 761 个文件、79.5 MB、13 242 088 个 token，只做"词法 + 一个花括号计数器 +
`标识符` 后跟 `(`/`;`/`{` 记一条声明"的模糊扫描：

```text
token scan: 772 ms, 312 082 条"声明样"
对比今天的索引路径：131 s
```

**170 倍。** 而且这 772 ms 里绝大部分是词法（13.2M token × 0.04 µs）。真实的浏览索引器要加
作用域启发式、类别、基类子句，按 2–4 倍估：**全项目 2–3 秒**，简单项目几十毫秒。
每编辑一个文件重扫一个文件 ≈ **0.5 ms**。

### 12.4 因此：正确的架构是"两个索引 + 一个精确视图"

```text
浏览索引（新）    每文件一次 token 扫描，无树、无宏、跳过 body；产出 (名字, 类别, 写出来的作用域, 位置)
                  宏的问题由 hint 表解决：工具链一份内置 hint（_STD_BEGIN → namespace std { 之类），
                  用户可加自己的；错的地方就是错，允许缺失
                  盘上按 (文件内容, hint 版本) 缓存，编辑只重扫一个文件

精确视图（已有）  打开的文件：cook + parse（17 ms/次编辑）+ FileView —— 补全、跳转、语义着色、诊断
                  它对索引的依赖只是"名字 → 位置"，索引缺了就是"未解析"，不是"等它建完"

删除（不再需要）  第二遍、闭包走查、provisional summary、env 键、全局 pending 门控、
                  `index_tree` 与 `FileIndexer` 在索引路径上的地位（它们留给精确视图用）
```

### 12.5 为什么这条路能解释三家的性能

| | VS browse parser | CLion stub index | cpptools tag parser | 我们（现在） | 我们（12.4 之后） |
|---|---|---|---|---|---|
| 单位 | 每个源文件 | 每个文件 | 每个文件 | 每个文件 + 闭包 | **每个文件** |
| 建树 | 否（模糊） | 否（stub） | 否 | **是（CST）** | 否 |
| 宏 | hint 文件 | 有限展开 + 启发 | 不展开 | **完整展开 + 闭包环境** | hint 表 |
| 函数体 | 跳过 | 跳过 | 跳过 | **建树 + 抽 facts** | 跳过 |
| 冷建 1761 文件 | 秒级 | 秒级 | 秒级 | **131 s** | **预估 2–3 s** |
| 每编辑 | 一个文件 | 一个文件 | 一个文件 | 一个文件 + **闭包 + 第二遍** | 一个文件 ~0.5 ms |

这就是"为什么 VS/cpptools/CLion 快"的答案：**它们的索引根本没有做我们正在做的事**。
我们不是慢在实现上，是慢在要求上——要求索引具备编译器级的精确度，于是每一行索引都要付预处理和解析的代价。
