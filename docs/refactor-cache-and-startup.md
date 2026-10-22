# 重构方案：磁盘缓存与冷启动（性能优先）

**结论先行。**

```text
1. 磁盘缓存改成单一嵌入式数据库（redb；SQLite 为备选），四张逻辑表 + generation GC，
   取代 1 768 个摘要文件 + 85 个 .tu + sharing.bin 的目录树
2. 索引单位从"文件"改成"TU(root)"：一次解析、facts 按来源归档 —— 删掉第二遍（1 621 次走查 / 120 s）
3. 冷启动分三段预算：扫描 ≤150 ms → 打开文件的 TU ≤800 ms → 后台建库（不可见）
4. 缓存键保持"解析前可算"（content_hash, context_hash），但 context 里必须含**宏环境指纹**
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

同一个头文件在不同 root 下可能有**不同**的 summary（不同宏环境），所以 `summaries` 的主键是
`(content, context)`，`files` 只记录"路径 → 当前 content/context"。**多行是正确，不是冗余**；
GC 按引用保留，不能按路径去重。

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
