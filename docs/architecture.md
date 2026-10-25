# 架构：原始文本只 tokenize，只有渲染文本进 parser

> 这份文档取代 `docs/` 下除配置类之外的全部旧文档（旧文档已删除，见 commit 里的 `git rm`）。
> 里面每一处"现在的做法"都指着一个可以用探针复现的事实；每一处"目标"都写明了验收方式。

---

## 1. 原始设计，和它被偏到哪去了

原始设计（这是唯一正确的读法）：

```text
文件文本 ──lex──► token 流（位置=文件坐标）
                    │
                    ├──► 预处理（宏展开、条件分支求值）──► 渲染后的 token 流
                    │                                       每 token 带：
                    │                                         cooked   在渲染文本里的位置
                    │                                         written  在本文件里的原始位置（宏体来自别处则 None）
                    │                                         reported 报告给用户的位置（宏体 → 最外层调用点）
                    │                                         origin   宏展开的来历
                    │
                    └────────────────────────────────► parser **只**解析渲染文本
                                                          │
                                                          语法树，叶子 token 仍带上面的映射
                                                          │
                              ┌───────────────────────────┴───────────────────────────┐
                              ▼                                                       ▼
                     语义分析、索引事实（范围映射回文件坐标）             LSP 回答（位置全在文件坐标）
```

关键性质只有一句：**parser 永远看不到宏**。它看到的是预处理器的输出，宏已经展开、条件分支已经选定；文件坐标通过
每个 token 的映射回到用户那里。这样"宏被当成类型""死分支的声明也是答案""`#if` 拆开的返回类型要猜"这些问题在
**架构上不存在**，而不是靠一层层补丁。

**实际做成了什么（必须承认）**：索引和大多数查询走的是"直接解析文件自身文本"的主路径，渲染只是一条**按需**的
旁路（`Session::rendering_of` / `view_of_the_rendering`）。于是 parser 看到的是未展开的文本 —— 这正是
`_GLIBCXX_NODISCARD` 被当成一条变量声明、`_GLIBCXX20_CONSTEXPR` 被当成返回类型、`#if` 两边的声明都是答案的
根源。为把这些症状按回正确行为，后来加了：

| 补丁 | 它存在的唯一原因 |
|---|---|
| `DeclFact::split_by_a_directive` + 渲染兜底 | 写读法不知道 `#if` 哪边是活的 |
| 查找期 `visibility_at` 过滤（+ `guard_answers` 记忆） | 写读法读到了死分支的声明 |
| `facts_vs_rendering` 里"写读法 vs 渲染读法"的对账 | 两种读法必须对账，说明不该有两种 |
| 转换运算符按拼写取返回类型 | 写读法把宏名当成了返回类型 |

这些补丁**各自是对的**（它们让当下的行为不再撒谎），但它们加在一起说明了一件事：**主路径偏了**。目标架构里它们
全部删掉。

---

## 2. 现状盘点：哪些部件已经在正确的位置上

这不是从零开始。映射基础设施和渲染器都已经存在，缺的是"把 parser 挪到渲染这一侧"。

| 部件 | 现状 | 与目标的关系 |
|---|---|---|
| lexer（`cpp_parser::lex`） | 只吃文本，产出带文件坐标的 token | ✅ 完全符合 |
| 预处理器（`preprocess::cooked::cook_with`） | 产出 `RenderedCooked { text, spans }` | ✅ 完全符合 |
| 映射（`RenderedSpan`） | `cooked` / `written` / `reported` / `origin` 四个字段 | ✅ 正是设计要的那张表 |
| 坐标换算（`FileView::{file_offset_of, reading_offset_of}`） | 双向，按 token 走 | ✅ 已实现，按需加更快的索引 |
| 渲染读法的索引（`IndexedRendering`） | 存在：`summary` 的范围**已映射回文件**，还有 `MapReport` 与 `CookedDiagnostic` | ✅ 目标路径已经有一条，只是不是主路径 |
| 文件自身文本的索引（`summarize`） | **主路径**：所有事实都来自它 | ❌ 这就是要拆掉的东西 |
| 按需渲染（`rendering_of`，8 条 LRU） | 旁路，命中率低，催生过"每次 hover 一次 cook" | ⚠️ 目标里它变成"主路径的缓存"，容量与键都要重设计 |
| 摘要缓存键（`SummaryKey::env_hash`） | 已经带"这次读法在哪个宏环境下" | ✅ 目标的 per-TU 索引直接用它 |
| 双层补丁（见上表） | 因为两条读法并存 | ❌ 目标里删除 |

**结论**：目标架构不是重写，是 **把主路径从 `summarize(raw)` 切到 `IndexedRendering`**，然后把因此不再需要的补丁
删掉。风险集中在"per-TU"和"增量"两件事上（§4、§5）。

---

## 3. 目标架构

### 3.1 数据单元

```text
TranslationUnit  (一个 .cpp + 它的 include 闭包 + 该 TU 的宏环境)
  ├─ 每个文件一份 LexedFile { tokens }            ← 只依赖文件文本，可跨 TU 共享
  ├─ 每个文件一份 RenderedFile { text, spans }     ← 依赖 (文件内容, 宏环境)
  └─ 每个文件一份 FileSummary { declarations, macros, includes, guards }
                                                    ← 从 RenderedFile 解析得到，范围已映射回文件
```

三条缓存键，全部**先于解析就能算出来**（这是可查找性要求）：

```text
LexedFile      content_hash
RenderedFile   (content_hash, env_hash)              env_hash = 该文件在那个 TU 里生效的宏集合
FileSummary    (content_hash, env_hash, context_hash) 加上编译器配置、目录等既有部分
```

`env_hash` 已经在 `SummaryKey` 里 —— 现在它只用于"第二次读法"，目标里它是**唯一**读法的键。

### 3.2 不变量

1. **parser 只吃渲染 token 流。** 代码里不存在"解析原始文本"的入口；`summarize(raw)` 删除。
2. **所有对外范围都在文件坐标。** 事实、诊断、跳转、重命名、语义高亮的范围一律经映射产出；`RenderedSpan::reported`
   是"报告给用户的位置"的唯一来源。
3. **一个文件的事实按环境分区。** 同一个头在两个 TU 里可以有不同的 `FileSummary`（例如 `_GLIBCXX_USE_CXX11_ABI`
   不同），查找必须选**询问方所在 TU** 的那一份。
4. **不活跃区域不产生事实。** 死分支在渲染时就被删掉了，所以查找里不再需要 `visibility_at` 过滤。
5. **宏文本不出现在任何语义字段里。** 事实的 `type_of`/`returns`/`parameters` 来自渲染 token，宏已经展开。

### 3.3 查询怎么走

```text
请求（文件 F，文件坐标 P）
  → 找到 F 所在的 TU（或 F 自己就是一个 TU）
  → 把 P 映射到渲染坐标（reading_offset_of）
  → 在该 TU 的渲染树上做语义分析
  → 结果范围映射回文件坐标（file_offset_of / reported）
```

这条链现在就在 `view_of_the_rendering` + `FileView::parse_rendering` 里，问题只是主路径绕过了它。

---

## 4. 代价：为什么当初会绕过它，以及现在为什么不该绕

当初绕过的理由是**索引延迟**：渲染一个头要单位遍历 + lex + cook，实测 `bits/stl_bvector.h` **17–24 ms**，而
"1 273 个文件、2.24 s 首次索引"是直接解析文本的数字。要么付 per-TU 的渲染代价，要么接受"parser 看见宏"。

正确的算法不是二选一，而是**把渲染做成共享的、可缓存的、增量的一层**：

* 渲染的键是 `(内容, 环境)`，**不是 TU**。一个工程里的绝大多数文件共享同一个环境（同一套 `-D`/`-std`），所以一个
  头只渲染一次，第二个 TU 直接命中；
* 渲染产物可落盘（`renderings` 表已经存在：`(content, env) -> zstd(rendered text)`）；
* 环境变化时只失效受影响的那一层：改一个 `.cpp` 的 `#define` 只影响它自己的 TU；改一个**头里的** `#define` 影响
  所有包含它的 TU —— 这条级联必须有明确的上界策略（§5.4）。

验收方式（必须在实现前后各测一次，写进本文件）：

```text
首次索引（冷缓存）      1 273 个文件 / 闭包           目标：不劣于现在的 2.24 s 的 1.5 倍，且事实全部来自渲染
稳态每按键              当前 17–60 ms（文件而异）        目标：不劣化
hover / 定义 / 补全      当前 1–12 ms                    目标：不劣化，且不再有"每次 hover 一次 cook"
```

---

## 5. 实施计划（每一步都可独立提交、独立回退）

### 第一步：把"渲染读法"变成主路径（不改任何 LSP 行为）

> **已经查清楚的接缝（写在这里，避免下一步再猜）**：索引目前从 `SummaryStore::prepare_inner` 出发，它拿到的
> `environment` 是 **parser 层的两个 trait**（`&dyn MacroBodies`, `&dyn MacroFacts`），而渲染器要的是 **session 层的
> 类型**：`FileMacros::new(MacroView<'unit>, &UnitDefinitions, Option<&MacroTable>, bool)`。也就是说"渲染 + 解析渲染"
> **不能在 store 里做**，只能在 `Session` 里做 —— `Session::rendering_of` 已经证明这条路可行（它就在 session 层拼出
> `FileMacros`）。于是第一步的落点是：
>
> ```text
> Session::index_the_rendering(path)
>     unit   = translation_unit_of(path)                      // 已有
>     text   = files.read(path)                               // 已有
>     tokens = lex(text)                                      // 已有
>     macros = FileMacros::new(unit.view(path), &unit.definitions(), Some(seed), true)
>     rendered = cook_with(&text, &tokens, &macros).render()
>     indexed  = FileIndexer::index_rendering(path, &rendered, key.with_environment(env_hash))
>     commit indexed.summary                                   // 范围已经映射回文件
> ```
>
> 也就是说：**准备（prepare）与提交（commit）之间的那一步换掉，而不是重写 store**。pump 的切片、`prepare_closure`
> 的并行读、摘要缓存都保留；换来的是"事实来自渲染"。

* `FileIndexer`/`summarize` 的**唯一**入口改成"给文本 + 环境 → 渲染 → 解析 → 映射 → `FileSummary`"；
  保留一个仅供测试的 `summarize_the_written_text`（名字里带"测试"），生产代码不再调用。
* 删除 `DeclFact::split_by_a_directive`（渲染里不存在"拆分"）：事实要么有一个类型，要么确实没有。
* 删除查找期的 `visibility_at` 过滤与 `guard_answers`（死分支根本不在渲染里）。
* 删除 `facts_vs_rendering` 这个"两种读法对账"探针，改成"渲染读法的事实清单"探针（保留它作为审计工具）。
* 验收：全部测试套件绿；`facts_vs_rendering` 的位置由新探针取代；`lsp_status` 表全绿且延迟不劣化。

### 第二步：per-TU 的索引与查找

* `ProjectIndex` 的摘要查找按 `(path, env_hash of the asking file's TU)`；询问方没有 TU 时用工程默认环境。
* `definitions`/`members_of` 等查询的入口加一个"环境"参数（多数调用点从 `visible_from` 推出）。
* 验收：一个 fixture（两个 `.cpp` 用不同 `-D` 包含同一个头）里，同一个名字在两个 TU 得到各自环境下的答案；
  真项目 `lsp_status` 不劣化。

### 第三步：增量与缓存

* 渲染缓存升格为一级缓存：键 `(content, env)`，容量按字节、按 LRU 淘汰；磁盘表复用现有 `renderings`。
* 编辑一个文件：只重新 lex 它、重新渲染它自己（若它是头，还要使其渲染缓存条目失效并**标记**依赖它的 TU）。
* 验收：`edit_loop` 在 `LuaSyntaxTree.cpp` / `CodeFormat.cpp` 上的每按键数字不劣于当前；`loop_probe` 第二轮 0 次 cook。

### 第四步：删掉"两种读法"这层概念

* `view_of_the_file`（原始读法）只保留给**必须看原始文本**的功能：格式化、`documentSymbol` 的原文范围、
  语义高亮里不活跃区域的处理、以及"文件里有语法错误时仍要能回答"的降级路径。
* 其余全部走渲染视图。命名上把 `rendering`/`cooked` 之类词汇统一成"the reading"（只有一种）。
* 验收：`grep -r "written text" crates` 不再命中任何语义分析路径；文档与代码命名一致。

### 5.4 已知风险与上界

1. **级联失效**：头文件里的宏变化会影响所有包含它的 TU。策略：不追踪到具体 TU，把 `env_hash` 变化当作**该文件
   渲染缓存失效**，下一次查询按需重渲染（惰性），并用"一次 pull 内重渲染预算"限制单次请求的最坏代价。
2. **内存**：per-TU 渲染文本。策略：渲染只保留到 `FileSummary` 产出即释放（除非它被 LSP 正在使用），磁盘表兜底。
3. **配置缺失**：没有 `compile_commands.json` 时，环境是工程默认（`.cppls.toml`），与现在一致；这条不因为架构改变
   而变差。
4. **错误容忍**：渲染文本解析失败时的降级路径（原文解析）保留，但只作为"诊断/降级"，并把使用次数计数暴露出来 ——
   它一旦经常触发，说明渲染器有真 bug，而不是架构需要两条主路径。

---

## 6. 从现在到第一步的具体改动清单

1. `cpp_code_analysis::summarize(path, text, key)` → 私有 + `#[cfg(test)]`；新增
   `summarize_the_rendering(path, rendered, key) -> IndexedRendering`（已有等价实现，改成公开入口）。
2. `ProjectIndex` 的所有 `SummaryKey::new(0, 0)` 处改成真实的环境键（现在有几个"没有环境"的占位，正是它们让
   `_STD` 之类的宏在事实里是名字而不是 `::std`）。
3. `DeclFact::split_by_a_directive`、`a_guard_is_taken`/`guard_answers`、`certain_declarations` 里的可见性过滤：删除。
4. `hover` 里的 `fact_from_the_rendering`：删除（类型直接从事实读，因为事实来自渲染）。
5. 探针：`facts_vs_rendering` → `facts_audit`（"渲染读法的事实清单 + 映射报告"），保留 `rendering_probe`、
   `parsed_summary` 审计。

---

## 7. 验收基线（当前数字，实现过程中每次都重测）

```text
真项目      E:\EmmyLuaCodeStyle，1 273 文件，150 834 事实，0 诊断；首次索引 ~2.24 s
闭包        libstdc++ 579 头文件，55 549 事实
渲染        单头 stl_bvector.h：raw 439 声明 / 渲染 294 声明，两侧 0 错误节点；渲染 ~17–24 ms
事实对账    facts_vs_rendering：267 条可比事实，返回类型/type_of 矛盾 0；483 条无对应（462 条件分支内、21 孤儿）
LSP         lsp_status 表 29 行全绿，hover 1–12 ms，diagnostic 28–29 ms，settle 22–24 s（80 次 pull）
按键        edit_loop 每按键 min 16–18 / median 17–60 / max 61–118 ms（文件而异）
测试        cpp_parser 30、cpp_code_analysis 35、cpp_ls 13、cpp_formatter 12 个套件；唯一已知红：
            translation_unit::the_timeline_answers_what_the_per_file_walk_answered
```

**第一步的完成标准**：facts 全部来自渲染；删除四类补丁后全部套件仍绿（除已知红）；上表每一项不劣化。
