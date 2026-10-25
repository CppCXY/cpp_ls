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
```

**第一步的完成标准**：facts 全部来自渲染；删除四类补丁后全部套件仍绿（除已知红）；上表每一项不劣化。

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

**为什么四层补丁还在**：它们覆盖的是"第一次读法（raw）到 pump 精炼"之间的窗口。第一次读法也渲染之后那个窗口才关，
而那需要 §5 第二步的 key（`env_hash` 由调用方可算），也就是 `directive_summary` + unit walk 先于 parse 的顺序。
