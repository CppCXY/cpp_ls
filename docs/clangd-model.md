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
