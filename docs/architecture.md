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

> **进度（已落地的那一半）**：`Session::index_the_rendering(path)` 已经能工作 —— 它用现有的
> `rendering_of` + `FileIndexer::index_rendering` + `ProjectIndex::insert`，把"lex 一次 → 渲染 → 解析渲染 → 范围映射回
> 文件"这条路走通，并用 `tests/rendering_real.rs::the_rendering_is_the_reading` 钉住：
>
> ```text
> stl_bvector.h：294 条事实（raw 读法是 439 条），24.8 ms（cook + parse + map + commit）
> emplace_back → [Some("reference")]      ← 没有任何兜底，因为 #else 分支不在 parser 读的文本里
> insert       → 5 × Some("iterator")     ← 死分支的 void 声明不存在
> split_by_a_directive 为真的事实 = 0      ← 渲染里没有指令可拆
> 所有 range 都在文件内
> ```
>
> **还差的那一半，接缝已经确认（查过代码）**：第二次读法（`SummaryStore::prepare_the_re_read`）的签名带
> `units: &[Arc<TranslationUnit>]`，也就是说它手上已经有 unit，可以在 store 里直接渲染 ——
> `unit.environment_of(path)`（`MacroView`）+ `unit.definitions()`（`UnitDefinitions`）+ `cook_with` →
> `RenderedCooked`，再走 `index_rendering`。第一次读法（`prepare_inner`）没有 unit，只能是临时读法，这正是第二次读法
> 存在的原因（按已知的宏环境重读）。
>
> 所以接线的顺序是：**先把第二次读法切成渲染读法**（事实从此来自渲染，四层补丁才能删），再决定第一次读法要不要也
> 变成"知道环境就渲染、否则临时"。
>
> 接线的验收：`lsp_status` 全绿、`loop_probe` 第二轮 0 次 cook、`edit_loop` 不劣化，并且真项目 settle 之后
> `index.summary(stl_bvector.h)` 里 `emplace_back` 的返回类型是 `reference`（现在是 `None`，因为索引里还是 raw
> 读法）。
>
> **接线试了三次，第三次把原因测出来了**（代码用 `git checkout` 回退，树保持绿）。第三次的做法：渲染 → `index_rendering`
> → 把第一次读法的 `macros`/`includes`/`guards` 合并进来。诊断打印证明：
>
> ```text
> [re-read] widget.h: Widget scope Some("std")       ← 渲染读法建出来的摘要是对的
> [test]    widget.h: 2 declaration(s), Widget scope None, macro_readings 0
>                                                     ← 但索引里最终留下的还是第一次读法
> ```
>
> 也就是说：**重建的摘要在 `commit_the_re_read` 被丢掉了**。那个函数有一条正确的守卫：
>
> ```rust
> let current = self.index.summary(&rebuilt.path).map(|summary| summary.key);
> if current != Some(rebuilt.key) { continue; }   // 文件在这期间被别的读法换过，就别覆盖它
> ```
>
> 而重建的 key 与索引里的不是同一个：第一次读法是用 `prepare_with_the_environment` 建的，key 里带 `env_hash`，而重读路径
> 里现算的 key 不带。于是 `current != Some(rebuilt.key)` 永远成立 —— 重建被跳过的次数比它成功的次数多得多，而且**没有
> 任何日志说它被跳过**。
>
> **守卫已经修好（这是迁移的使能项，已提交）**：`commit_the_re_read` 现在只比较 `content_hash`/`context_hash`
> （"计划是针对这次读法做的"），并在提交时沿用当前 key 的环境部分 —— 文本变了仍然拒绝。之前比整个 key，于是**凡是
> 第一次读法给过环境的文件，重读结果都被静默丢掉**。修好之后那条一直挡路的 `std::Widget` 测试在旧路径下通过。
>
> **两个分支的能力不一样，这是下一步的全部要点**：
>
> | 分支 | 有 unit？ | 能渲染？ |
> |---|---|---|
> | `through_a_timeline`（`prepare_the_re_read` 里带 `units` 的） | 有：`unit.environment_of` + `unit.definitions()` | **能**：`FileMacros::new` 要的正是这两个 |
> | 无 unit 的 eager 分支（`closure_environment`） | 没有，只有 `MacroEnvironment` | **不能**：`cook_with` 要 `MacroBindings`，而只有 `FileMacros` 实现它，`FileMacros` 又要 `MacroView`/`UnitDefinitions` |
>
> 而挡路的那条 fixture **走的正是 eager 分支**（`units` 为空），所以 timeline 分支的渲染接线既没被它验证、也没被它
> 否定 —— 接线要验的是"有 unit 的那条路"。两条具体结论：
>
> **更正（第三次接线实测）**：上面那句"fixture 走 eager 分支"是**错的** —— 第三次接线后同一条测试仍然红，说明它走
> 到了 timeline 分支，且渲染读法在这条路上**丢掉了 scope**。而 `tests/rendering_scope.rs` 已经证明同一个文件的渲染
> （`session.rendering_of`）和 `index_rendering` 都能给出 `scope: Some("std")`。差别在**选哪个 unit**：
>
> ```text
> session.rendering_of(path)  →  translation_unit_of(path)：这个文件**自己的** unit
> timeline 分支（旧代码也一样）→  units.iter().find(|u| u.environment_of(path).is_some())：**第一个覆盖它的** unit
> ```
>
> 一个有多个 unit 的工程里，第一个覆盖 widget.h 的 unit 可能没有走到 open.h，于是 `_STD_BEGIN` 没有体可展开 —— 渲染
> 出 `_STD_BEGIN struct Widget {};`，scope 就是 None。旧路径不怕这种"部分环境"，因为 `with_macro_bodies` 只需要
> **名字→体**的映射（部分视图也有），不需要完整展开；而渲染需要完整环境。**部分环境下的渲染比 raw 更差**，这是这次
> 试出来最重要的一条。
>
> 所以下一步不是"接渲染"而是"**先让 plan 带上每个文件自己的 unit**"（plan 里的 `through_a_timeline` 从 `(&PathBuf,
> &String)` 变成带上 unit 索引），否则选择逻辑决定了渲染的正确性。
>
> **可以机械执行的做法（API 都已确认存在）**：渲染只在"unit 走完了这个文件的整个闭包"时才可用 ——
>
> ```text
> TranslationUnit::paths()         单位走过的文件集合
> TranslationUnit::environment_of   该文件的视图（只在走过时 Some）
> ProjectIndex::visible_files      该文件的 include 闭包（已有、带记忆）
> 规则：environment_of 为 Some 且 闭包 ⊆ paths() 的那个 unit 才能用来渲染；否则保持 raw 路径
> ```
>
> 为什么这条规则是对的：渲染是"整个环境"的函数，而 raw + `with_macro_bodies` 只需要"名字→体"的映射 —— 后者在部分
> 环境里仍然完整，前者不完整就会把 `_STD_BEGIN` 原样留在文本里。所以**部分环境 ⇒ 不渲染**，而不是"渲了再补救"。
>
> 1. **timeline 分支**可以接（第二次渲染试过、代码写得出、编译过），要在**有 unit 的场景**里验：真项目探针
>    （`lsp_status` settle 之后 `index.summary(stl_bvector.h)` 的 `emplace_back` 应为 `reference`），或一个
>    session 测试让 drain 带 units。
> 2. **eager 分支**要么保持 raw（现状，可以用），要么接受一个新适配器：把 `MacroEnvironment` 实现成
>    `MacroBindings`（`definition_at(name, offset)`）。这是**唯一**能在无 unit 时渲染的路，且是新机制 —— 决策留给
>    下一步，不偷偷加。
>
> ```text
> 渲染文本 = 编译器看见的东西：宏已展开、非活跃分支已删除、#define 行本身不在里面
> 所以 summary.macros 若从渲染的解析里来，就是**空的** —— 而 open.h 的 _STD_BEGIN 体正是别的文件
> 靠它把 Widget 放进 std 的东西。raw + with_macro_bodies 那条路能通过，就是因为它保留了 #define 事实。
> ```
>
> 也就是说目标架构要写清一条新的分层规则（下一次接线的依据）：
>
> | 事实 | 来源 |
> |---|---|
> | 声明、类型、参数、基类、范围 | **渲染的解析**（宏已展开、分支已选） |
> | `#define` / `#include` 事实、会话需要的宏体 | **原始文本**（它们不在渲染文本里） |
> | 映射 | 渲染 token 的 `spans`（`index_rendering` 已经在做） |
>
> 于是"一个文件的摘要"= 渲染的声明事实 + 原始文本的指令事实，两者用同一个 `SummaryKey`（含 `env_hash`）落在同一份
> 摘要里。下一步就是按这张表实现：`index_rendering` 产声明，指令事实由一次轻量的指令扫描（`FileIndexer::includes_of`
> 已经是这类扫描）补齐，`macros` 从扫描里来而不是从渲染的解析里来。


> **接线又试了一次（两半合并：声明来自渲染、`#define`/`#include` 事实从第一次读法继承），仍被同一条测试挡住**：
> `Widget` 在渲染里没有落在 `std` 里。测试本身说明了原因，而且它是架构层面的：
>
> ```text
> session::tests::a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session
>   open.h   #define _STD_BEGIN namespace std {
>   widget.h _STD_BEGIN struct Widget {}; _STD_END      ← 作用域来自**别的文件里**的宏体
>   断言：std::Widget 能找到、scope == "std"，并且 summary.macro_readings 非空（"作用域来自宏体，摘要要记下证据"）
> ```
>
> 也就是说第二次读法要跨过的不是"能不能渲染"，而是**渲染之后的两个后果**：
>
> 1. `Widget` 的 `scope` 必须由**展开后的** `namespace std {` 决定，并且在范围映射回文件之后仍然对；
> 2. `macro_readings`（"作用域是哪个宏体给的"这份证据）在渲染读法下**没有意义了** —— 那正是要删的东西，但决策
>    与它绑在一起：先证明 1 成立，再删 2。
>
> 这两条现在写进计划，而不是留成"下次再说"：下一步是**先把这条测试在渲染读法下修绿**（scope 与映射），再删
> `macro_readings`，然后四层补丁。
>
> **接缝（已用代码确认，替换上一版"不能在 store 里做"的说法）**：第一次读法 `prepare_inner` 只有 parser 层的
> `(&dyn MacroBodies, &dyn MacroFacts)`，确实渲染不了；但**第二次读法 `prepare_the_re_read` 带着
> `units: &[Arc<TranslationUnit>]`**，而 `unit.environment_of(path)` 给 `MacroView`、`unit.definitions()` 给
> `UnitDefinitions` —— `FileMacros::new` 要的正是这两个，所以渲染**能**在 store 里做。第一步的落点是：
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

> **⑬ 接线落地（本次提交）—— 上表就是实现，而且它不是"再试一次"**：`FileIndexer::index_the_readings(path, source,
> rendered, key)` 是唯一入口，声明来自渲染的解析（范围经 `reported_span` 映射回文件），`#define`/`#include`/条件区
> 从**同一次 lex 的 token** 来（`directive_summary`，不解析），两者由 `FileSummary::take_the_directives_of` 合成一份
> 摘要。`prepare_the_re_read` 的 timeline 分支现在真的渲染：`unit.environment_of` + `unit.definitions()` →
> `FileMacros` → `cook_with` → `index_the_readings`。
>
> 这条路上真正挡住前四次的不是"能不能渲染"，而是**三个已经存在、但没人读的东西**：
>
> 1. **`FileIndexer::dead_macros` 从来没被读过。** `without_these_macros` 设置了它，`index_tree`/
>    `directive_summary` 都不看它 —— 也就是说 `vcruntime.h` 那条 `_STL_LANG = 0L` 的修复**从未生效**。现在两处都过滤。
> 2. **磁盘缓存把"依赖环境"的读法存在只描述文本的 key 下。** 实测：`_STD_BEGIN` 从 `namespace std {` 改成
>    `namespace studio {`，文件被正确 forget、重新入队、重读，然后 `ProjectIndex` **把上次存在磁盘上的 `std` 读法
>    又发了回来** —— 整条失效链都工作，输给了一个分不清两个世界的缓存项。现在重读的摘要**不写盘**（它没有能描述自己
>    的 key；那是 `env_hash` 与第二步的事），盘上留的仍是第一次读法的诚实读法。
> 3. **依赖失效从来没丢过摘要。** `invalidate_dependents` 只丢 cooked 读法、重排一次 cook。现在它丢**摘要本身**
>    （`store.forget` + 重新入队），并清掉该文件的渲染缓存。旧测试用 `macro_readings` 非空来代表这件事 —— 那个字段
>    **没有任何读者**，所以那项能力其实一直是空的（把断言换成行为断言后，旧代码同样红）。
>
> 还有两处是模板本身的后果，必须写下来：
>
> * **`Reused`（磁盘命中）的文件也要进第二遍。** 一条摘要现在是"文本 + 环境"的函数，而 key 只描述文本，所以盘上那份
>   永远是未精炼的读法 —— 用它当答案就是回到 raw。代价：热启动重读冷启动要重读的那些文件（实测真项目 settle
>    **23.6 s / 80 pulls**，基线 22–27 s，未劣化）。
> * **第三类重读触发条件：声明被条件编译拆开的文件。** `bodied`（宏体）看不见 `#if` 拆分的声明，而 `stl_bvector.h`
>    的 `emplace_back` 形如 `#if __cplusplus > 201402L reference #else void #endif`，完全没有宏体参与。触发条件就是
>    事实自己说的话：`split_by_a_directive` 或 guard 不是 `Unconditional`。重读之后这些事实全是 `Unconditional`，
>    所以**同一个文件不会被无谓地渲染第二次**。
>
> 验收（真项目 `E:\EmmyLuaCodeStyle`，重跑于最终代码）：
>
> ```text
> rendering_real（libstdc++ stl_bvector.h，pump 78 slices 后）
>   index.summary(header) 的 emplace_back = [(Some("reference"), split:false)]   ← 流水线自己落的读法，没人要求过
>   lsp_status：29 行全绿，settle 23.6 s / 80 pulls，hover 1–8 ms，diagnostic 31 ms，completion 2 ms
>   cpp_code_analysis lib 677 绿；cpp_parser / cpp_ls / cpp_formatter 全绿；仅剩已知的
>   translation_unit::the_timeline_answers_what_the_per_file_walk_answered 红（先于本次改动）
> ```
>
> **一个真实的产品后果，必须记在这里**：`Session::outline` 原来说"有摘要就用摘要，因为大纲是文件的读法"，这在摘要
> 还是 raw 时成立；摘要变成渲染之后，`cpp_ls` 的 `the_outline_shows_the_file_and_not_the_compiler` 立刻红 —— 大纲
> 少了 `Dead`（死分支里的类，用户正在编辑它）、多了 `HWND`（`DECLARE_HANDLE(HWND)` 宏声明的类型，文件里没有这个声明）。
> 结论：**大纲一律读文件自己的文本**（`view_of_the_file`），不看摘要 —— 归档视图和档案视图不能混；这一改顺带让大纲
> 不再依赖"渲染是否已经存在"。
>
> **还没做，且明确不做在本次**：eager 分支（无 unit）仍是 raw；四层补丁（`split_by_a_directive`、查找期
> `visibility_at`、对账探针、转换运算符拼写规则）**仍然必须留着** —— 它们覆盖的是"第一次读法到 pump 精炼之间"的
> 窗口，只有第一步全部落地（第一次读法也渲染）才能删。谁删谁要先把那个窗口关掉。
>
> **⑭ 第一次读法也能渲染了（同一提交）：窗口关掉了一半。** `Session::advance` 本来就会把"已经有 unit 的文件"从
> wave 里摘出来，交给 `index_one` 的 `None` 分支单独读 —— 那个分支原来做的是"raw 解析 + 宏体"，现在做的是
> **渲染 + `index_the_readings`**（`SummaryStore::prepare_with_the_timeline`）。所以键入一个字符、不移动任何指令时，
> 这个文件的下一次读法**就是**编译器读法，不用等 pump 的第二次读法：
>
> ```text
> session::tests::a_keystroke_in_a_body_reads_the_file_through_its_timeline
>   did_change(widget.h, 体内加一行) → advance → Step::with_the_environment == true
>   并且 summary 里 Widget 的 scope == Some("std")（来自别的文件的 _STD_BEGIN 体），立刻成立
> edit_loop（真项目 CodeFormat.cpp，10 次按键）
>   min 17 / median 18 / max 71 ms     ← 基线 16–18 / 17–60 / 61–118，未劣化
>   second-pass backlog 937 文件        ← 基线 1 205：被 timeline 读过的文件不再进第二遍
> lsp_status  settle 24.3 s（80 pulls），hover 9 ms，diagnostic 30 ms
> ```
>
> 同时删掉了因此变成死路的 API：`SummaryStore::prepare_with_the_environment`、
> `FileIndexer::with_macro_body_readers` / `with_a_macro_environment`（"raw + 宏体"作为**读法**没有了，
> `with_macro_bodies` 只剩 eager 回退一个用户）。
>
> **剩下的一半窗口**：某个文件手上有 unit 才走这条快路，而 unit 表是有界的（`HeldUnit` 的字节预算）—— 一次
> `index_everything` 之后，键击的那个文件的 unit 常常已经被挤掉，于是仍然走 wave 的 raw 解析（`edit_loop` 的
> `env 0` 就是这个意思）。下一步（§5 第二步）要么让查询按需拿到该文件的 unit，要么让 wave 也能拿到
> `directive_summary` 先建 environment 再读 —— 后者的顺序是文档里那句话：**先指令、再 unit、最后 parse**。
> （**这一半已经关了，见 ⑮。**）

> **⑮ 剩下的一半窗口关掉了：unit 不在手上时，就按「先指令、再 unit、最后 parse」现场走一个**（本次提交）
>
> ⑭ 之后，一个文件的第一次读法是不是编译器读法，取决于这个会话**手上有 unit**。而 unit 表是有界的，所以真实序列是：
> 项目索引完 → unit 被挤掉 → 用户在那个文件里键入 → 这一步回到 raw。⑮ 把"手上有"换成"**要得到**"，并且用文档自己
> 写下的那个顺序：
>
> ```text
> 先指令     summary 被编辑丢掉了，但文件的 #include 还在文本里 —— directive_summary 不需要 parse
> 再 unit    从这个 root 走一条时间线；closure 里有一个文件还没被读过，就**拒绝**（部分环境比 raw 更差，见 §5）
> 最后 parse 解析的是渲染，事实的范围经 reported_span 映回文件
> ```
>
> 落点（三处，都在 `Session`）：
>
> | 位置 | 作用 |
> |---|---|
> | `unit_and_state_of` | index 没有 summary 时，若**读者正在看这个文件**（`documents.is_open`），用 `directive_summary` 当 walk 的 root；闭包没读完就 `None` |
> | `the_timeline_to_read_through` | `index_one` 的入口：手上有（非 provisional）就用，否则 `a_timeline_can_be_walked_for` 说可以就走 |
> | `will_read_it_as_the_program` | `advance`/`prepare_a_wave` 决定**要不要把这个文件留给 wave** —— 留给 wave 就会先写下一份 raw 读法 |
>
> **两个便宜一半的修正，都是量出来才加的**（同一台机器，真项目 `clice`：397 文件 / 60 k 事实 / settle ~10.5 s）：
>
> * **`directive_summary` 被记住了。** 一次索引步会问它**三次**（wave 的决定、walk 的 root、渲染的指令半边），而它是
>   lex + 指令扫描 + 每个条件求值 + 每个 `#include` 一次文件系统搜索 —— 实测 55 KB 源文件 **16 ms**。按
>   `(路径, 文本哈希)` 记住之后，3 次变 1 次：directive 按键 **165 → 125 ms**。
> * **渲染的指令半边不再重算。** `FileIndexer::with_directives` 让 `index_the_readings` 用调用方已经读过的那一份 ——
>   能走 timeline 的调用方，正是先读过指令的那一个：**125 → 113 ms**。
>
> 实测（`crates/cpp_code_analysis/tests/keystroke_probe.rs`，新增；`--ignored`，打印而不断言）：
>
> ```text
>                       基线（HEAD）                     ⑮
> didOpen + 重读        50 ms，environment false          74 ms，environment **true**
> 体内按键 10 次        52 / 54 / 55 ms，10/10 是程序      54 / 56 / 57 ms，10/10
> 指令按键 10 次        93 / 98 / 109 ms，**0/10**        109 / 113 / 123 ms，**10/10**
> unit walk 次数        168（索引时）+ 10（每次指令按键）  168 + 10 —— 相同，但**基线那 10 次是白走的**
> ```
>
> 最后一行是这笔账的关键：基线里第二遍**走了**这个文件的 timeline（41 ms），然后因为"没有宏体提到"把文件跳过，walk 白费；
> ⑮ 走同一次，但它的结果就是这份摘要。所以多出来的 ~15 ms 不是新工作，是"把它用在正确的地方"加上一次 16 ms 的指令扫描。
>
> **明确不做**：wave 里的**其他**文件仍然先读 raw、由第二遍精修。让整个项目第一次就读渲染 = 每个 TU root 走一次
> timeline，代价是 §4 那张表；正确的答案是 §5 第二步的 per-TU 索引（一个 TU 的闭包读一次、每个文件的事实
> 从 `index_unit_rendering` 分出来），不是给每个文件各走一次。
>
> **本次顺带修掉的三个真实缺陷（都不是 ⑮ 引入的，是 ⑮ 的探针和测试撞出来的）**：
>
> 1. **parser 的 marker 泄漏**（`crates/cpp_parser`）：`Marker::undo` 把节点改名成 `None` 却**没有从 open 栈上摘掉**，
>    `parse_expression_list` 的两处 `expect_token(…)?` 也不 unwind。结果：一个没有闭合的括号 —— 也就是读者**正在输入**
>    的那一行 —— 会在文件剩下的部分里留一个 open marker，之后每一次 recovery 都收敛到一个仍然握着它的层级。
>    `tests/invariants.rs` 的删除 fuzz 一直在红（`single_character_deletions_are_parseable`）；现在它是绿的，并新增了
>    `a_group_that_is_missing_its_closer_leaves_no_marker_open` 钉住这两种形状。
> 2. **宏展开里跨两个坐标系做减法**（`preprocess::expand`）：`expand_body` 用 `tokens.first()..tokens.last()` 算
>    region，而一次 rescan 的 token 可以同时来自**替换列表**（`#define A F`）和**调用点**（`A(1)`）—— 两个不同文本的
>    offset 相减于是 `attempt to subtract with overflow`（debug 直接 panic，release 得到一个荒唐的 `SourceRange`，
>    `origin_of` 拿它和 token 比大小）。现在 region 是 invocation 的 **call site**，它按构造只在一个文本里。
>    `tests/expand.rs` 里那两条一直红的用例（`a_function_like_name_ending_a_body_takes_the_arguments_after_the_call`、
>    `a_chain_of_object_like_macros_ends_in_a_call`）现在绿。
> 3. **`textDocument/foldingRange` 永远回答 `null`**（`crates/cpp_ls`）：handler 要的是**渲染视图**，而渲染只有在 cook
>    之后才有，cook 又会被"闭包还没读完"拒绝 —— 所以在索引还没收敛的项目上，折叠请求三十秒里每一次都拿到 `null`。
>    折叠本来就是"只读这个文件"的查询（它自己的模块文档这么写，`Region` 折法也来自 `#if` 指令，而渲染里没有指令），
>    改成 `view_of_the_file` 之后立刻就有答案。`the_folds_of_a_file_are_the_regions_a_reader_hides` 从红变绿。



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

> **⑬ 之后的状态**：第 1 项已完成（唯一入口是 `FileIndexer::index_the_readings`）；第 2 项部分（重读路径的 key 有了
> 文本与上下文，`env_hash` 还没有算，因为算它需要先有 unit —— 见 §5 第二步）；第 3–5 项**还没做**，理由是上面那条
> "窗口"：第一次读法还是 raw，删掉补丁会让窗口内的答案退化。清单本身保留，作为删补丁那一步的依据。

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
            （**⑮ 之后全绿**：那条红是断言本身写错了，见 §7.2）
```

**第一步的完成标准**：facts 全部来自渲染；删除四类补丁后全部套件仍绿（除已知红）；上表每一项不劣化。

> **上表是另一台机器上的数字**（`E:\EmmyLuaCodeStyle`，本机没有这个 checkout）。本机能跑的是 `C:\Users\zc\Desktop\github\clice`
> （397 个文件，60 k 事实，无 `compile_commands.json`），所以 ⑮ 的数字来自它，并且**只用于和同一台机器上的 HEAD 比**，
> 不与上表相比。`tests/keystroke_probe.rs` 是那台的 `edit_loop` 的替代品：同样的三个问题（重开一个文件、体内按键、
> 指令按键），打印而不是断言。

### 7.1 ⑬ 之后的重测（同一台机器，最终代码）

```text
渲染读法的落点   index.summary(stl_bvector.h).emplace_back = [(Some("reference"), split:false)]
                  —— 流水线自己落的，没有调用 index_the_rendering；pump 78 slices 收敛
LSP              lsp_status 表 29 行全绿，hover 1–8 ms，diagnostic 31 ms，completion 2 ms，
                 settle 23.6 s（80 次 pull）      ← 基线 22–27 s，未劣化
测试             cpp_code_analysis lib 677；cpp_parser / cpp_ls / cpp_formatter 全绿；
                 唯一红仍是 translation_unit::the_timeline_answers_what_the_per_file_walk_answered
重读触发         宏体（`bodied`）+ 死宏（`dead_macros`）+ **声明被条件编译拆开**（新增）
失效             丢摘要 + 丢渲染缓存 + 重新入队；重读的摘要不写盘（没有能描述环境的 key）
```

### 7.2 ⑮ 之后的重测（本机，`clice`，与 HEAD 同机对比）

```text
按键探针          tests/keystroke_probe.rs（新增，--ignored）：didOpen / 体内 10 次 / 指令 10 次
                  didOpen        50 → 74 ms，environment false → true
                  体内按键        52/54/55 → 54/56/57 ms，10/10 → 10/10 是程序
                  指令按键        93/98/109 → 109/113/123 ms，0/10 → 10/10 是程序
                  代价来自：一次 55 KB 文件的指令扫描（16 ms，已被记住后只算一次）+ 渲染解析比 raw 解析贵
测试              cpp_code_analysis lib 680 绿（新增 2 条 session 测试）；cpp_parser 全绿（含之前一直红的
                  删除 fuzz）；cpp_ls 全绿（含之前一直红的 folding；`latency` 必须 `--release` 跑，debug 下它
                  自己文档就写了会因算术而红）；cpp_formatter 全绿
                  另外：`translation_unit::the_timeline_answers_what_the_per_file_walk_answered` 这条从 ⑭ 之前
                  就一直红的测试**也绿了**，但绿的方式是把它改成它本来该断言的东西 —— 见下
一直红的那条 它要的是"per-file walk 与 timeline 给出**完全相同**的答案"。实测的差异只有一处，而且是结构性的：
                SA（config.h 里 #ifdef __cplusplus 之内）
                  timeline   offset 21 之后：kind/body/parameters 都有   ← 它求值过那个条件
                  fallback   只有 body_text_in_force，位置通道答 None    ← 没有 unit 就无法给条件里的 #define 定位置
                REPEATABLE / _CONFIG_H_（文件自己的 include guard 之内）
                  位置通道两者**完全一致**；只有"in force"通道 timeline 多一份（表示差异，不影响判定）
              所以测试现在断言的是那条真正成立的关系：**fallback 能答的位置，timeline 必须答得一样；timeline 多答
              的地方，那个名字必须是 fallback 只能放进 in-force 通道的**（并且 fixture 必须真的用上这条许可）。
              这正是 §5 第二步要解决的差别，而不是一个可以长期留红的噪声。
```

**四层补丁为什么还在**：它们覆盖的是"**别人**的文件第一次被读成 raw 到第二遍精修之间"的窗口 —— ⑮ 关掉的是
"**读者正在看的那个文件**"的窗口。wave 里的其他文件仍然先读 raw，所以 `split_by_a_directive` 之类的兜底仍然有用户。
删它们的前置条件是 §5 第二步：一个 TU 的闭包只走一条 timeline，闭包里每个文件的事实都从**这一条**渲染里分出来
（`FileIndexer::index_unit_rendering` 已经在，只是没接线 —— 见 `finish_a_slice` 里"NOT WIRED, ON PURPOSE"那段）。

### 7.3 第二步的「before」：一次 unit read 到底给索引加了什么（本机，`clice`）

`tests/unit_read_probe.rs`（新增，`--ignored`）先索引一个真项目，再对**一个** TU 调用 `Session::read_the_unit`，
打印前后的数字。这是接线的判据，也是"那三个数字变差"这条旧结论的复核：

```text
项目                       397 文件；settle 13.0 s；索引时走了 168 条 timeline
闭包（Selection.cpp）      177 个文件，42 526 条 raw 事实，**0** 条 cooked 声明
read_the_unit              5.09 s；159 个文件 placed；737 525 token；**0 error / 0 unplaced / 0 unbalanced**
之后                       42 526 条 raw 事实（不变，一条没丢）+ **39 080** 条 cooked 声明（142 个文件有了）
第二次读（同一程序）       85 ms（rendering 与 facts 都按程序字节记忆）
```

三条值得记下来的：

1. **一条没丢。** 旧结论（`declarations_in("std")` 2008 → 1959）在这份代码上不复现：一次 737 k token 的整程序
   解析**零错误**，142 个文件的 cooked 声明是净增，raw 事实数一位没变。所以当年那三个数字变差的原因确实是现在
   `index_unit_rendering` 的 crossing fence 隔离掉的那一类（一个文件的未闭合作用域吞掉后面所有文件），而不是
   整程序解析本身不成立。
2. **cooked 可以比 raw 多。** `logger.h`：raw 18 条 / cooked 28 条 —— 多出来的是宏写出来的声明，正是"只有渲染才
   看得见"的那一类（`DECLARE_HANDLE(HWND)` 的形状）。
3. **代价是 5 s，而且是一次性的、按程序算的。** 同一个程序的第二次是 85 ms。但这 5 s 现在是**一整块**，落在哪一半
   锁上就是哪一半的停顿 —— 所以接线的方式只能是 `prepare_the_drain`（读锁一侧，与查询并行）算出 `IndexedUnit`，
   `commit_the_drain` 只做 `insert_cooked`。`read_the_unit` 现在把这两半写在了一个 `&mut self` 里，接线前要拆开；
   两个记忆字段（`rendered_unit` / `unit_index`）也要从 `&mut` 改成锁。

### 7.4 ⑯ 接线：整程序读法进了流水线，但带上了一个由数字决定的预算（本次提交）

§7.3 说的三件事都做了：

```text
拆两半   read_the_unit = prepare_a_unit_reading(&self) + commit_a_unit_reading(&mut self)
         prepare  走锁之外（读锁一侧）：walk、cook、整程序 parse、sweep —— 5.09 s 的 5.09 s
         commit   只剩每文件一次 insert_cooked + queue.mark_worked
记忆字段 rendered_unit / unit_index 从 Option<…> 改成 Mutex<Option<…>>（和 units / definitions 同一形状）
接线     finish_a_slice 在一份"没有别的活"的 drain 上排一个整程序读：
           is_idle() && drain.files.is_empty() && drain.environments.is_empty() && 时钟 && 预算
         根是读者打开的文件（`the_next_unit_to_read`），`units_read` 保证一个程序只读一次
唤醒     `Session::the_wait_before_a_program_may_be_read` + pump 的一次 sleep：
         闸门是**时钟**，而时钟不会唤醒任何东西 —— 读者停下打字之后没有任何通知，所以"安全的那一刻"正是 pump 会睡过去
         的那一秒。pump 睡完剩余时间再回来，闸门因此是延迟而不是死锁
```

**预算为什么存在，以及它为什么让这个特性在真 C++ 项目上基本不触发。** 读法对程序是线性的：**7.07 MB 源码 →
5.09 s，即 0.72 s/MB**。prepare 那一半握着**读锁**，而 `didChange` 要写锁，所以那次读多贵，读者在这一刻敲下的
那个字符就要等多久。服务端自己的写入预算是 1 s（`crates/cpp_ls/tests/latency.rs` 的 `UPDATE_BUDGET`），于是有
`THE_LARGEST_PROGRAM_READ_AT_ONCE = 1 MiB`（≈0.72 s）。

这条线把所有包含标准库头的 TU 都排除在外了 —— clice 那个闭包是 **7.07 MB**，libstdc++ 的闭包也在同一量级。所以诚实的
结论是：**这套机制现在是活的、有测试的，但在真项目上几乎不会触发**，因为真正的解法不是调大预算，而是**让这次读不需要
会话锁**。5.09 s 的实测切分（`unit_read_probe`，同一程序，第二次跑在 render 已做完的基础上）：

```text
walk + 每个文件的文本   41 ms + 3 ms     ← 唯一真正需要 &Session 的部分（units / definitions 两张表、queue）
render（cook + 拼接）   1.25 s           ← 3 524 298 字节的渲染文本
parse + sweep           3.66 s           ← 737 525 token、39 080 条声明
第二次读（记忆/DB）     63 ms
```

也就是说 5.05 s 是 unit + 文本 + 配置的纯函数，只有 44 ms 需要那把锁。下一步是把 `prepare_a_unit_reading` 拆成
"锁内取材料"与"锁外算"：前者产出 `(Arc<TranslationUnit>, sources, config, files 句柄)`，后者依次是 `cook_the_unit`
与 `index_unit_rendering`；服务端那边需要一个三段式的 `run_blocking`（读锁 gather → 无锁 compute → 写锁 apply），
`prepare_the_drain`/`commit_the_drain` 已经是同一个形状。预算那时可以抬到一个只防"病态闭包"的值，而不是现在这个
"防人手速"的值。

测试（`crates/cpp_code_analysis/src/session/tests.rs`，两条都断言机器而不是断言意图）：

```text
a_program_is_read_when_the_reader_stops_editing   打完字一秒钟后，下一个 drain 把程序读了，
                                                  闭包 4 层下的 deep.h 有了 cooked 读法，HWND__ 落在 api.h
a_program_too_large_to_read_at_once_is_left_alone 同一个 fixture 加一个超预算的注释头 → 不读，per-file 读法照旧
```

（**⑯ 的第二半已在 ⑰ 完成**，见 §7.5：预算已经抬到 8 MiB，那两条测试按常量取尺寸而不是写死 1 MB。）

### 7.5 ⑰ 那 5 s 挪到锁外了，于是整程序读法在真项目上真的会跑

§7.4 的下一步就是这一步，做法和当时写的完全一样 —— **三段式**，中间那段不带任何会话：

```text
Session::gather_a_unit_read(&self)          读锁   44 ms    walk、闭包文本、两次缓存查询
UnitReadMaterials::read_the_program(self)   无锁   4.9 s    cook → facts 查询 → parse+sweep → 两行写库
Session::commit_a_unit_reading(&mut self)   写锁   几 ms    每文件 insert_cooked + mark_worked
```

`UnitReadMaterials` 带的是 `Arc<TranslationUnit>`、每个文件的文本、`Marked`、`CompilerConfig`、files 句柄、以及
store 的数据库 `Arc` —— 也就是说"读一个程序"这件事需要的每一样东西都可克隆、都不借会话。服务端那边
`AnalysisState::read_a_program` 就是这个三段式：`run_blocking`（读锁）取材料 → `tokio::task::spawn_blocking`
（**不带锁**）算 → `update_session`（写锁）落库；pump 在队列排空、且距上次编辑一秒时调它，时钟的剩余时间仍由
`the_wait_before_a_program_may_be_read` 让 pump 自己睡出来。

**预算因此换了含义**：`THE_LARGEST_PROGRAM_READ_AT_ONCE` 从 1 MiB（"防人手速"）抬到 **8 MiB**（"防病态闭包"）——
中间那 4.9 s 不握任何锁，所以一个读者在它跑的时候敲键只会等 gather 的 44 ms。8 MiB ≈ 5.8 s 单核，并且**高于**
被测量的那个程序（7.07 MB，MSVC 的 STL），也就是说真项目的真 TU 落在线内。超过的（生成出来的、被到处 include 的
那种）保持 per-file 读法：不阻塞、也不会失控。

实测（本机 `clice`，release）：

```text
keystroke_probe 新加的最后一相（读者停手 1.2 s 之后调用 read_a_program_now）
                  5.24 s，180 个文件，806 008 token，**0 error / 0 unplaced / 0 unbalanced**
                  **+43 522** 条 cooked 声明变成可查询的（142 → 180 个文件有这个闭包的读法）
交互不变          didOpen 71 ms（environment true）；体内按键 54/58/60 ms；指令按键 104/108/113 ms；10/10 是程序
cpp_ls release    13 个目标全绿：handshake 34（含 folding）、latency 2（5.87 s）、idle、diagnostics_stable …
cpp_code_analysis 682 lib + 全部集成绿
```

**端到端（真正的服务端，`loop_probe` 打开 clice 的 `Selection.cpp`）**：索引收敛的那一行之后紧跟着

```text
[INFO cpp_ls::handlers::initialized] the workspace is indexed — the longest an update held the analysis was 91 ms
[INFO cpp_ls::context::analysis_state] read a program as one piece: 180 file(s), 806008 token(s), 0 error(s)
                                       — their declarations are the compiler's reading now
```

也就是说这把 5.2 s 真的是**在写锁之外**发生的：同一段日志里最长的写锁是 **91 ms**（那是索引自己的输入），而那十次
diagnostics pull 期间 `0 cook(s)`、没有一行重复（`loop_probe` 的判据）。

**这条链现在闭合到哪**：一个读者打开一个文件、停手一秒，这条 TU 的**整个闭包**就有了编译器读法（不只是他打开的那个
文件），而且这些事实是从**一条** timeline、**一次**整程序解析里分出来的 —— 正是 §5 第二步的形状。还差的是
§5 第二步的另一半：`ProjectIndex` 的查找按询问方所在 TU 的环境分区（`env_hash`），以及把四层补丁删掉。删补丁的
窗口现在是"**非读者文件**第一次被读成 raw"——wave 仍按文本读它们，然后第二遍精修；把 wave 也变成"一个 TU 一条
timeline"就是下一步。

**一个读到一半才发现的代价，已经修掉**：`units_read` 原来在**每次编辑**都清掉（`buffer_changed`），于是"打字 → 停
一秒 → 再打字"会让这条程序每停一次就重读一遍 —— 5.24 s 的后台 CPU 换来一份**没有变化**的读法。现在它和 unit 表用
同一条规则：只有**指令移动**（`moved.layout`）才让这条程序重新变成"欠着"的，因为只有那时整程序渲染的每个偏移才都变了；
body 编辑不动程序形状，而那个文件自己的事实本来就由 per-file cook（⑮）重建。
`session::tests::a_body_edit_does_not_cost_the_program_again` 钉住这件事，用的
是 `the_wait_before_a_program_may_be_read`（pump 问的那个问题）而不是睡眠。

### 7.6 下一步：冷启动按 TU 读，而不是按文件读（计划，未开始）

删四层补丁的前置条件写在 §5.4 和第 7 章里很多次了，⑰ 之后它第一次成了一个**可算**的问题而不是一个愿望：

```text
现在的冷启动   397 个文件各解析一次（wave）→ 每个 open 文件的 TU 再读一次程序（⑰）→ 第二遍再精修被条件拆开的
目标           每个 TU root 一次：directive_summary 把闭包扫一遍（不解析）→ walk → render → parse → 每个文件
               的事实与摘要都从这一次读法里分出来
```

为什么现在可算：⑰ 证明了一次整程序读法是 **5.2 s / 180 个文件 / 806 k token**，而它取代的是"这 180 个文件各解析
一次 + 第二遍重读其中一部分"。指令那一半是每文件一次 lex + 指令扫描（55 KB 的文件 16 ms，头文件多为 10–30 KB），
而且**已经在 `SummaryStore::directive_summary` 里按内容和路径记住了**。

真正的工作量在三处，都不是小改：

1. **`FileSummary` 的来源要换。** 目标定义是"指令（文本里）+ 声明（渲染里）"，两者都用同一个 `SummaryKey`；
   现在 `summarize`/`index`（raw）是声明那半的唯一来源。
2. **walk 要先于 parse**：闭包里的每个文件先只有 directive summary，`TranslationUnit::walk` 已经能吃
   `&FileSummary`（它只读 `macros`/`includes`/`guards`），所以这条顺序是通的 —— ⑮ 已经在单个文件上用了它。
3. **缓存键**：这样建出来的摘要**不能写进只描述文本的那一行**（⑬ 的第 2 条教训），要有 `env_hash`
   （`SummaryKey::with_environment` 已有），而 lookup 侧必须能用同一个哈希问 —— 这就是 §5 第二步。

### 7.7 ⑰ 到底买到了什么，以及一个**被测量推翻**的下一步

`tests/unit_read_probe.rs` 现在把两件事分开数，而它们在此之前被同一个"39 080 条 cooked 声明"混在一起：

```text
只有渲染才有的声明           **21** 条（177 个文件里，全部在 clice::function_traits 与 logger.h 的特化里）
名字答数（前后）             36 → 36   —— 索引本来就能从 raw 摘要回答普通声明；程序读没有增加"名字"
raw 摘要里有、渲染里没有     **1 095** 条（`stdlib.h` 的 `#ifdef` 分支等）
```

**第一个数字修正了 ⑰ 的叙事**：整程序读法不是"补上索引缺的声明"，而是"**修正索引已有的答案**"——条件分支、宏展开的
作用域、宏写出来的声明。想用"名字答数"证明它的价值会得到 0。

**第二个数字推翻了一个看起来显然的下一步。** 我按它做了半截"把 summary 的声明半边换成程序读法"，然后在测试里发现前提
是错的：`index_everything` 之后，一个文件的 summary **本来就是渲染**（第二遍重读了每一个"被指令拆开或有宏体"的文件）。
所以那 1 095 条差异不是 raw vs 渲染，而是

```text
文件单独读（standalone）      第二遍：把文件当成它自己的 TU，`_CRT_…`/`_GLIBCXX_…` 按"独立编译"回答
文件在程序里读（⑰）          整程序：同一个头在 `Selection.cpp` 的宏环境下回答
```

**同一个头在两个 TU 里本来就该给出不同答案**，所以把一份换成另一份是拿错误的 TU 回答 —— 这正是 §5 第二步要的
`env_hash`，而不是一次改写能解决的。那半截改动已经撤回，理由写在 `commit_a_unit_reading` 的注释里（下一个人会想
做同样的事）。真正要做的因此是：**摘要按（文本 + 它被读的那个 TU 的环境）分区**，查找按询问方所在 TU 选一份 ——
`SummaryStore::prepare_with_the_timeline` 已经在用这样的 key 写渲染摘要了，缺的是查找侧。

### 7.8 ⑱ 一个文件只有一份读法：渲染赢，并集删掉

上面那条"1 095 条差异"指出的最直接问题不是"该用哪一份"，而是**查找当时把两份一起给了**。
`visible_declarations_upto` 的规则是"先放摘要的事实，再放渲染的事实里摘要没有的那些"，`hits_in`（补全走的那条
bucketed 查找）另有一条"raw 影子盖住 cooked"的规则 —— 两处都在**替一个文件调解两份读法**。现在两处都只有一条规则：

```text
一个文件有渲染读法  →  它的读法就是渲染（facts = cooked.declarations）
没有渲染读法        →  它的读法就是摘要（facts = summary.declarations）
```

`Posting::is_cooked` 说明一条 posting 属于哪一份列表，所以选边是一次比较而不是一次扫描
（`ProjectIndex::the_reading_is_cooked`）。随之删掉的：按 qualified name 的 raw-vs-cooked 去重（十行注释与逻辑）、
`hits_in` 里的影子规则、以及"一个文件的两份读法该怎样合并"这一整类推理。

实测（本机 `clice`，`unit_read_probe`）：

```text
名字在两份列表里都有、却仍从同一个文件答出 >1 个候选     **0**（改动前最多 1 095）
```

一条新测试钉住这条规则的**语义**（`a_files_rendering_is_its_reading_and_not_one_answer_beside_another`）：
`api.h` 在 `FROM_THE_SOURCE` 定义与不定义时各声明一个 `Live`，所以"单独读"和"在 `main.cpp` 里读"是两个答案；
程序读之后，查找给出**一个**，而且是被选中的那个分支（改动前给的是单独读的那份，并集时是两份）。

**还没解决的、也是 §5 第二步剩下的**：一个文件被**两个**程序读过时，写进 `cooked[path]` 的是后一个 —— 现在是
"最后读的那个 TU 的读法"，不是"询问方所在 TU 的读法"。要做到后者，`cooked` 的键得像
`SummaryStore::prepare_with_the_timeline` 那样带上 `env_hash`，查找再用询问方（`visible_from`）的 TU 环境去选。
⑱ 让"选一份"这件事成为**唯一**的规则，接下来只是"选哪一份"。

**这对四层补丁意味着什么**：patch 2（查找期 `visibility_at` 过滤）存在的理由是"死分支的声明还在摘要里"。⑱ 之后，
它只对**没有渲染读法**的文件还有用户 —— 而那些文件正是 §7.6 要消灭的。


