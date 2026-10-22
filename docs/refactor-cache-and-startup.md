# 重构方案：磁盘缓存与冷启动（性能优先）

**结论先行。**

```text
1. 磁盘缓存改成单一嵌入式数据库（redb；SQLite 为备选），四张逻辑表 + generation GC，
   取代 1 768 个摘要文件 + 85 个 .tu + sharing.bin 的目录树
2. 索引单位从"文件"改成"TU(root)"：一次解析、facts 按来源归档 —— 删掉第二遍（1 621 次走查 / 120 s）
3. 冷启动分三段预算：扫描 ≤150 ms → 打开文件的 TU ≤800 ms → 后台建库（不可见）
4. 缓存键 = (content_hash, config_hash, **relevant_env_hash**)：环境取的是"这个文件能观测到的宏"的投影，
   不是整个宏环境（实测 43–408 个名字 vs 9 401 个），见 §三之二
5. 体积目标 160.6 MB → ≤25 MB（宏体去重 + zstd）
```

---

## 一、现在的缓存是什么（实测）

在 `EmmyLuaCodeStyle`（1 761 个文件，冷缓存跑完一遍）上的 `.cppls` 目录：

```text
.cppls/                 160.6 MB, 1 854 个文件
  summaries/   1 768 个文件   69.0 MB    ≈39 KB/个
  tu/             85 个文件   85.6 MB    ≈1.0 MB/个（最大的 5.0 MB）
  sharing.bin                  6.0 MB
```

五个具体问题：

```text
① 1 854 个小文件本身就是冷启动成本：NTFS 上 1 768 次 stat+open+read 是几百毫秒
② prune 只在会话打开时做，删除语义靠"下次扫"，无法回答"谁引用了这份缓存"
③ tu/ 85 MB 与 summaries/ 69 MB 之间没有共享：149 160 条带 body 的宏 fact 在两边都存
④ 没有事务：写到一半崩溃会留下半份摘要（现在靠 header 校验兜底）
⑤ 键里没有宏环境，但摘要的内容依赖它 —— 这正是第二遍存在的根因（见 §三）
```

## 二、存储层：redb（SQLite 为备选）

**选 redb 的理由**：纯 Rust（Windows/MSVC 零 C 构建依赖）、单文件 MVCC、零拷贝读、键有序（范围扫描 +
按前缀删）、ACID 崩溃安全、无后台线程。**SQLite 作为备选**：能直接用 DB Browser 查、生态成熟；
代价是 bundled C 编译，且大 blob 需要自己分块。

### 逻辑表

```text
meta          key                 -> value
              schema_version / reading_fingerprint / format_version / created_at / generation

files         path                -> { content_hash, mtime, size, context_hash, generation }
              一次扫描得到 diff 的入口：内容没变的文件**不解析**，直接从 summaries 命中

summaries     (content_hash, context_hash, format, fingerprint) -> zstd(summary bytes)
              主键里的 context 是"这次编译的宏环境指纹"，不是路径

macros        body_hash           -> zstd(body)
              宏体只存一次；environments 里按名字引用

environments  env_hash            -> { 名字 -> body_hash 的排序数组, 配置指纹 }
              一个 TU 的宏环境；被 summaries.context_hash 引用

renderings    (content_hash, env_hash) -> zstd(rendered text)
              每个文件自己的渲染（实测 5 KB 级），TU = 这些渲染的有序拼接

roots         root_hash           -> { env_hash, files:[path...], boundaries }
              TU 的成员与文件边界（`with_file_boundaries` 要的那份数据）
```

### 三条硬规则

```text
1. 缓存不是权威：任何表损坏只导致重建，不丢用户数据；schema_version / reading_fingerprint 不匹配即清库
2. 事务粒度：写 = 一个文件（或一个 root）一个事务；读 = 一次读事务。redb 的 MVCC 让读不阻塞写
3. generation：每次启动 +1；GC 删除"没有被任何 files 行引用"的 summaries / macros / renderings
```

### 体积估算

```text
宏体去重：149 160 条 → 去重后估计 3–5 万条，省约 60 %
zstd：摘要与渲染约 3×
tu/ 整份不再存储（85.6 MB → 0）：渲染按文件存，环境按 env_hash 存
合计：160.6 MB → 15–25 MB
```

## 三、冷启动：从"两遍"到"一遍"

### 现状（实测）

```text
第一遍（按文件索引）        12.4 s
第二遍（1 205 个欠账重读）  120 s   ← 1 621 次闭包走查 × ~75 ms
第一次诊断                  等第二遍结束（门控是全局 pending_work == 0）
```

根因：第一遍解析时闭包还没索引完，拿不到宏表 → summary 被标 provisional → 第二遍带环境重读一遍。
**事实是 `f(文本, 宏环境)`，而宏环境是 TU 的属性，不是文件的属性。**

### 新模型：preamble-first

```text
阶段 0  扫描：mtime + size，与 files 表 diff                      ≤150 ms
阶段 1  打开文件的 TU：用 renderings 拼出闭包 → 一次解析 → facts 按来源归档
        → 这个文件的诊断、补全、语义着色可用                      ≤800 ms（首屏）
阶段 2  后台：按 root 逐个建库；每个 root 一次 TU 解析
        已缓存的 header 摘要按 (content, env) 直接命中，
        只有内容/环境变了的文件才重新渲染、重新索引                ≤3–4 min，不可见
阶段 3  空闲：按 generation 与体积预算 GC                          与启动无关
```

三个关键变化：

```text
① 不存在 provisional summary。DB 里有的摘要一定是"这个 (content, context) 的"，
   于是门控从"全局 pending_work == 0"变成"这个文件的摘要存在" → 首屏诊断 800 ms 级
② 一次解析产出多个文件的 facts：`FileIndexer::index_unit_rendering` + `file_what_was_found`
   已经实现了"按来源归档"，目前只有 examples 和 read_the_unit 在用
③ context_hash 里含宏环境指纹。旧注释说"键必须解析前可算"——新模型里 root 在解析前已知，
   它的闭包的 #define 集合来自 files + renderings，所以环境指纹**仍然可以在解析前算**
```

### 一个必须写进实现说明的细节

同一个头文件在不同 root 下可能有**不同**的 summary（不同宏环境），所以 `summaries` 的主键要能表达"环境"。
但**不能是整个环境**——见 §三之二。

## 三之二、键里的"环境"必须是**投影**，不是整个宏环境

用整个宏环境进键是纯粹膨胀：同一个标准库头文件因为 include 顺序不同就有一份新摘要，缓存永远不命中。
实测这个担心有多大：

```text
工具链里的宏名（MSVC 14.51 include/，全部 #define）        9 401
某个头的**自身文本**里出现、且确实是宏名的标识符数量：
  xstring        1 082 个标识符 →  101 个是宏名（9.3 %）
  vector         1 135 个标识符 →  101 个（8.9 %）
  string           221 个标识符 →   43 个（19.5 %）
  vcruntime.h      350 个标识符 →   84 个（24.0 %）
  yvals_core.h   2 451 个标识符 →  408 个（16.6 %，一个专讲宏的头）
```

一个头文件能观测到的宏是**它自己文本里出现的名字**（对象宏、函数宏、`#if` 条件里的名字），不是环境里的全部。
所以键里的环境部分应该是：

```text
relevant_env(file) = { name → body_hash | name 出现在该文件自身 token 流里 } 的传递闭包
                     （闭包 = 这些宏的 body 里出现的宏名，再取它们的 body，直到不动）
relevant_env_hash  = hash(按 name 排序的 (name, body_hash) 序列)
summary_key        = (content_hash, config_hash, relevant_env_hash)
```

为什么这解决了膨胀：

```text
· 投影是 43–408 个名字，环境是 9 401 个 —— 少 20–200 倍
· 这些名字的定义来自工具链自身（_STD_BEGIN、_NODISCARD、_HAS_CXX20 …），
  不同 include 顺序**不改变它们的定义**，所以同一个头在 10 个 root 下通常只有 1 份摘要
· 真正会变的是 /std: 级别与用户在自己代码里 #define 的覆盖值 —— 变体数是"几种编译配置"，不是"几种顺序"
· 投影为空的文件（不提及任何宏、也没有 #if）→ 键退化为 (content, config)，命中率最高
```

三个附加好处：

```text
① 失效是**精确**的：改一个没有文件提及的 #define，什么也不用失效；改 _STD_BEGIN，
   失效的正好是提到它的那些文件（不是整个 generation）
② 泛型化：同一个投影哈希可以算出"还要渲染吗"——渲染键同样用 (content, relevant_env_hash)
③ 可以在解析前算：投影的**名字集合**只依赖文本（一次词法扫描，100 KB 的头约 1 ms），
   body 从环境里查（每个 root 一次），所以"键必须解析前可算"这条约束仍然成立
```

表结构相应调整（替换 §二 里的 summaries / environments）：

```text
projections   (content_hash) -> zstd(该文件提到的宏名集合)      ← 只依赖文本，跨 root 复用
summaries     (content_hash, config_hash, relevant_env_hash) -> zstd(bytes)
renderings    (content_hash, relevant_env_hash) -> zstd(渲染文本)
environments  (root_hash) -> { 名字 -> body_hash, 配置指纹 }     ← 按 root 一份，供投影求值
```

**实现注意**：`config_hash`（编译器、dialect、include 目录）与 `relevant_env_hash` 要分开存，
前者决定"能不能用"，后者决定"内容对不对"。合并成一个哈希会让诊断无法回答"为什么没命中"。

### 投影过大时的退路（按文件选择键的种类）

传递闭包并不总有界：一个头若把宏名收敛到上千个（`yvals_core.h` 已经 408 个，且它是专讲宏的头），
继续用投影换来的收益就抵不上算它的成本。所以**按文件选**，用一个明确的阈值：

```text
投影名字数 ≤ 512   → 键 = (content, config, relevant_env_hash)      ← 默认，跨 root 复用最好
投影名字数 >  512   → 键 = (content, config, root_env_hash)          ← 该文件按"整个环境"存，命中少但可预期
```

判定发生在投影求值时（一次词法扫描 + 闭包），结果连同 `projections` 行一起记下，
所以"这个文件用的是哪种键"是可查的事实，不是隐式规则。阈值取 512 的理由是可量：
它大于实测最大的真实投影（408），又远小于环境（9 401），所以退路的触发是**例外**而不是常态；
实现完成后用 `yvals_core.h` 与 STL 的 `_MSVC_STL_*` 系列头各测一次，确认触发的文件数占比。


## 四、性能目标与验收

```text
指标                                   现在        目标
冷启动 → 首屏诊断                       135.6 s     ≤1.5 s（打开文件的 TU）
冷启动 → 全项目建库                     135.6 s     ≤40 s（后台、不可见）
热启动 → 首屏诊断                       ~4.2 s      ≤0.8 s
.cppls 体积                             160.6 MB    ≤25 MB
每次按键的编辑延迟（LuaSource.cpp）      ~10 ms      ≤10 ms（不退化）
闭包走查次数                             1 621       0（TU 化后每个 root 一次）
```

## 五、实施阶段（每阶段可独立合入，带自己的指标）

```text
M1  存储层：redb + 五张表 + generation GC；旧 .cppls 一次性删除重建
    验收：功能测试全绿；体积 ≤25 MB；热启动不慢于现在；探针改读 DB 指标

M2  TU 索引：按 root 解析 + facts 按来源归档；删除 parsed_since_the_last_pass 与第二遍
    验收：cpp_code_analysis 全绿；冷启动全建库 ≤40 s；不存在 provisional summary

M3  preamble-first 启动：阶段 0/1/2；门控按文件
    验收：首屏诊断 ≤1.5 s；服务端日志不再出现长的写锁持有

M4  复用：per-file 渲染缓存（content + env 为键）先行；绿树前缀复用作为独立一步
    验收：二次 TU 解析 ≤10 ms/root（渲染命中）；前缀复用见 docs/prefix-reuse.md

M5  清理：删除 summaries/ tu/ sharing.bin 的旧代码路径与 prune；文档与探针收尾
```

M1 与 M2 互不依赖，可以并行；M3 依赖 M2；M4 只在 M2 之后有意义。

## 六、风险与对策

```text
单文件数据库损坏       缓存非权威：校验失败就清库重建；不做恢复
多实例 / 文件锁        写者单例：第一个实例持写锁，其他实例只读打开；Windows 用独占 open 判定
事务粒度               写：一个文件一次；读：一次读事务；大渲染分块 256 KB
读被写阻塞             redb MVCC，读不取写锁；请求路径只读
schema 演进            meta.schema_version + reading_fingerprint 双闸，不匹配即重建
内存                   只 mmap 索引；blob 读到即解压即丢，不常驻
TU 解析太慢（M2 前提） 渲染 899 ms + 解析 399 ms + sweep 3.8 s（单个 2 MB TU）：
                       先靠 renderings 命中与 sweep 增量；解析复用等 M4
```

## 七、为什么不是"继续优化现有目录树"

```text
· 1 854 个小文件的 stat+open 就是几百毫秒，且无法批量读
· prune 只能在打开时跑，删除语义靠"下次扫"，没有引用关系
· tu/ 与 summaries/ 各自存宏体（149 160 条），没有共享
· 没有事务与版本闸，半份写入只能靠 header 校验兜底
· 每次改读者代码（READING_FINGERPRINT 变化）都留下一整代不可达文件，只能靠扫删
```

## 八、与已有文档的关系

```text
docs/edit-latency.md     §七 是这次重构的直接动因（第二遍 120 s）
docs/no-build-database.md §② facts 按来源归档 —— M2 的理论依据
docs/prefix-reuse.md     M4 的解析复用；事件流复用已实测为负收益，必须走绿树共享
docs/architecture-review.md  层与写锁预算
```

## 九、M1 进度：存储层骨架与实测（2026-10-22）

`crates/cpp_code_analysis/src/cache_db.rs` 已落地：redb 单文件数据库，七张表（`meta` / `files` / `projections` /
`summaries` / `renderings` / `environments` / `macros`），`CacheKey = (content, config, env)` 三段分开存，
`FileRow` 记录每个文件的 content/config/env/mtime/size/generation，`collect()` 按"有没有 files 行引用"删除，
`compact()` 归还文件空间；版本或 `reading_fingerprint` 不匹配时**删除重建**（缓存非权威）。

**这一层还没接进索引**（M1 的后半段），但已经能把现有文件缓存整个导入并量出差别。用项目自己的
1 768 份摘要（69.0 MB 的 `.bin` 文件）实测：

```text
文件缓存            1 768 个文件，69.0 MB
数据库              16.6 MB，1 768 行                    ← 4.2× 更小
写入（逐行事务）     8.2 s（4.6 ms/行）
写入（批量事务）     0.97 s                              ← 批量是必须的，不是优化
读回 1 768 份        170 ms（10 384 份/秒，解压后 72.4 MB）
collect（全删）      21 ms
compact              123 ms，24.6 MB → 3.5 MB            ← 删除后必须 compact，否则文件变大
```

两条写进实现的结论：

```text
① 写入必须批量：一次事务写多行，泵在一次切片结束时提交一批
② 删除之后必须 compact：redb 不会自动回收文件页；collect 与 compact 一起放在空闲步骤
```

**还没验证的**：多实例（写者单例）与 Windows 文件锁、`renderings`/`projections` 的实际压缩比、
以及 `CacheDb` 与现有 `SummaryStore` 的接线（M1 后半段）。

### M1 闭环：默认就是数据库（2026-10-22 更晚）

开关全部删除（`CPPLS_CACHE_DB` 与 `without_the_cache_database()` 都不在了），
文件缓存只是 miss 时的回落；`index_everything` 结束时跑一次 `collect`（空闲步骤）。
在 `EmmyLuaCodeStyle`（1 761 文件）上，同一个探针：

```text
                     文件缓存        数据库（现在）
冷启动                15.4–15.8 s     11.7 s
热启动                4.2 s           3.1 s / 2.0 s（连跑两次）
缓存体积              69 MB / 1 768 文件   32 MB / 1 个文件
```

**两个在收尾时修掉的缺陷**（都是"两个写者"或"批次不可见"这一类）：

```text
① shared() 原来用 canonicalize 做键：第一个调用者目录还不存在（canonicalize 失败），
   第二个成功 → 两个句柄、两个缓冲 → 20 个测试失败。改成词法键。
② 批次只在写路径可见：泵刚建好的摘要被下一个查询重新解析。
   SummaryCache::summary 现在先查批次再查数据库。
```

**还没做的一件事：compaction。** `collect` 只删不可达行，文件不会因此变小——
实测一次收集后文件从 22 MB 长到 32 MB（redb 不回收页）。compaction 需要 `&mut Database`，
而句柄是进程级共享的 `Arc`，所以它必须是一个**独占时刻**的任务（例如一个独立的
`cpp_ls --compact-cache` 子命令，或下次启动、打开数据库之前）。在那之前：
数据库会随世代缓慢增长，这是已知且可量的一点欠账。

### M2 的结果：第二遍不再"每次重做"，而是"缓存下来的"（2026-10-22 下午）

M2 的原计划是"按 root 解析 + facts 按来源归档，删掉第二遍"。这一轮做到的是它的**前提**：
`SummaryKey` 带上 `env_hash` 之后，第二遍的产物**能被下一轮命中**，于是它从"每次启动都重做一遍"
变成"一次建库、之后都是查表"。

实测（`EmmyLuaCodeStyle`，1 761 文件）：

```text
第一遍索引（冷）        17.5 s，结束时欠 1 753 个重读
第二遍（冷）            124.6 s —— 1 753 个文件各建一次环境（约 50 ms/文件），共走了 947 个 unit
第二遍（暖，同一二进制） 1.8 s —— 每个切片 4 个文件，31–119 ms，`nothing bodied`
```

关键在最后一行：暖启动时第二遍几乎不做事，因为 `(content, config, env)` 这个键已经存在，
`prepare_inner` 直接命中，不再解析、不再建环境。**这就是 M2 想要的性质**：第二遍可以留着，
它的成本从"每次 O(项目)"降到"每次 O(变化)"。

同一轮里还做了一个不改变语义的优化：第二遍切片先问**已持有的 unit** 是否覆盖这个文件
（`MAX_UNITS` 里那些完整的），覆盖就不走自己的闭包。冷启动的走查数从 1 621 降到 947。

**还没做的是 M2 的正体**：按 root 一次 TU 解析、facts 按来源归档，从而在**冷启动**里也不必有第二遍。
它的成本门槛写在这里，是下一步要看的东西：

```text
一次 TU 读取（LuaSource.cpp 的闭包）：渲染 899 ms + 解析 399 ms + 索引 3.8 s ≈ 5.1 s
198 个 root × 5.1 s 不可接受 —— 除非渲染、解析、sweep 三项都变成增量的（见 §六 的 M4）
```

### M4 的三项：1 做了，3 和 4 没做（以及为什么）

**1（done）**：TU 渲染进数据库 —— `render_the_unit` 899 ms → **55 ms**（跨会话），同会话内 17 ms（2 MB 克隆）。
见上文的实测与 `unit_codec`。

**3（未做）**：sweep 增量。实测 `read_the_unit` 仍是 4.4–5.5 s，其中解析 399 ms、
`index_unit_rendering` 的 sweep ≈3.9 s。sweep 是**对整棵树按 span 归属**的遍历，所以"跳过没变的文件"不是
一个 if：流里任一文件改动都会移动后面所有 span，缓存的 per-file facts 不能直接复用。

可行的做法（下一次的第一步，成本可估）：

```text
把 IndexedUnit 本身按 program hash 存进数据库（与 unit 渲染同一个套路）：
  read_the_unit = 解码 103 个文件的 facts + tokens 统计
  重复读同一个 closure：4.4 s → 估计 50–100 ms（解码 103 份 facts）
它不解决"闭包里有一个文件改了"的情况——那仍要整段 sweep——但重复读是常见情形（每次拉诊断都会读打开的文件的 TU）
```

**4（未做）**：TU 索引。它依赖 3：按 root 一次 TU 解析的代价是 parse + sweep，sweep 不变成增量，
198 个 root 就是 198 × 4.4 s ≈ 15 分钟。顺序不能颠倒。

**这一步没有做的事，以及为什么不做**：把 3 做成"缓存 IndexedUnit"要动 `IndexedUnit` 与 `CookedFile` 的
序列化（它们不是 `FileSummary`，是一棵按文件的 facts 集合），那是又一轮 400 行的编码工作；
半途而废会把 `read_the_unit` 留在"有时命中、有时不命中"的中间态。所以停在这里，把成本写清楚。
