# 编辑延迟：一次按键的成本，和检查层的 328 倍

**结论先行。编辑路径上"重建 + 索引"已经是 10 ms 级；真正贵的不是索引，是诊断（`sema::check`）。**

```text
一次按键（LuaSource.cpp，6.5 KB）              改前        六轮之后
  did_change                                   0.1 ms      0.1 ms
  pump（重建 + 重新索引）                      10.2 ms      9.9 ms
  补全请求                                     15.3 ms     15.6 ms
  诊断（客户端每次编辑都会拉）             19 704 ms   140 ms     ← 141×

四条真实工程上的诊断成本（每文件一次）        改前        六轮之后
  LuaSyntaxTree.cpp    2 800 行             283 326 ms   679 ms（单次拉取）；重复拉取 619 ms
  LuaSyntaxNode.cpp                           1 314 ms   200 ms
  四个文件合计                              284 648 ms   868 ms     （328×，findings 18 → 18，逐条相同）

冷启动（EmmyLuaCodeStyle，1 273 文件）       改前                        修之后
  index_everything                          4.4 s，返回时还欠 1 205 个文件   31.9 s，返回时欠 0
  之后每次按键的 pump                        65 ms（持续约 300 键）          9.7–19 ms
```

第三件事（§五）是调度而不是算法：第二遍的重读债务原来不计入"还有没有工作"，
于是 `index_everything` 在欠着 1 205 个文件的时候报告"索引完成"，而每一次按键都在替它还债。
现在它算工作量了——代价是冷启动如实是 31.9 s，而不再是"4.4 s 加 27 s 隐形"。

六轮分别找到的东西不同，所以下面按轮写：第一轮（§二）是"同一个昂贵动作被重复了几百次"，
第二轮（§三 ①–④）是**三个真正的设计错误** —— 推导引擎没有 memo、基类遍历把模板参数当基类、作用域查找是线性扫描；
第三到五轮（§三 ⑤–⑧）把剩下的 4% 拆开：96% 的检查层是类型推导，并把它最该跨请求复用的部分缓存起来。
§五 是第三件事，一个**调度缺陷**：重读债务不算工作，于是它被藏在"索引完成"之后由按键偿还。

本文只写**测出来的东西**和**改了什么**。所有数字都来自下面的探针，命令见 §六。

---

## 一、一次按键到底走了哪些步骤

服务器处理一次 `didChange` + 一次补全 + 一次诊断拉取，等于：

```text
did_change          Session::buffer_changed    换文本、算指令签名、丢摘要、入队、标记要 cook
pump 一个 slice     prepare_a_wave             解析（或从磁盘摘要命中）
                    commit_a_wave              插入摘要、把 include 入队
                    materials_for / render_them  cook = lex + 宏展开 + 解析渲染 + 建事实
                    prepare_the_drain            第二遍（被包含的头已入索引后重读文件）
                    commit_the_drain / commit_them  两次插入
request             view + completions         补全要走可见集和成员表
diagnostics         checks_about               22 条规则，读同一份 view + 索引
```

`examples/edit_loop.rs` 逐步计时，在 `EmmyLuaCodeStyle` 的 `LuaParser/src/File/LuaSource.cpp` 上：

```text
  did_change                0.1 ms
  pump                      9.9 ms
    parse（wave prepare）     3.0
    wave commit               0.3
    cook materials            1.9
    cook render               3.6     ← lex + 渲染 + 解析渲染 + 191 条 fact
    second pass（drain）      1.1
    drain commit              0.0
    cook commit               0.1
  write hold                0.3 ms   ← 查询要等的那个锁，一直是这个量级
  request                  15.6 ms   （view 0.9 + 补全 14.7）
  diagnostics             191.4 ms
```

也就是说：**重建 + 索引这一半已经只花 10 ms**，并且写锁只被持有 0.3 ms。用户在打字时感到的"卡"，
如果还剩下，来自诊断（和补全），不来自索引。

## 二、第一轮：检查层 19.7 s → 0.34 s

`CPPLS_TIME_CHECKS=1` 是仓库自带的按规则计时（`sema/check/mod.rs::run_check`）。同一个文件，改前改后：

```text
规则                        改前      改后
assign-type-check         15 371 ms    135 ms
param-type-check           2 228 ms    121 ms
implicit-bool-conversion   1 115 ms     59 ms
access-invisible             375 ms     15 ms
argument-count               365 ms     10 ms
concept-not-satisfied          1 ms      1 ms
undefined-field                1 ms      1 ms
----------------------------------------------
diagnostics（整次拉取）   19 704 ms    340 ms      58×
```

四条根因，全部是"对每个成员/每个表达式重复做同一件昂贵的事"。每一条都先量、后改、再量。

### ① 每个成员重建一次 `DeclarationShapes` —— 927 ms 一个成员表

`fact_from_binding` 把一条绑定读成一条 fact，它的每个字段都要 `DeclarationShapes` 表
（`declared_type_of` / `declared_returns_of` / `declared_bases_of` / `parameter_list_at`），
而 `DeclarationShapes::of(root)` 是**一次整棵树遍历**。`direct_members` 对每个成员调用它一次：

```text
std::vector（libstdc++ <bits/stl_vector.h>，151 个成员）
  own 927 ms        ← 151 次建表
  bases 0 ms
```

`declarations.rs` 里 `_with` 形式的读取函数（`declared_type_of_with` 等）本来就是为这种调用准备的，
注释写着"每一个有不止一个问题要问的调用者都该用它"。改法：`direct_members` 建一次表，
`fact_from_binding_with(&shapes, …)` 走完整张表。

```text
std::vector 成员表   own 927 ms → 4 ms
```

### ② 为找一个 operator 建整张成员表 —— 340 ms 里 13 次相同的问题

`operator_returns_of`（给 `v[0]`、`a + b` 这类表达式求类型）原来的做法是：拿整个类的**成员表**，
再在表里找 `operator[]`。`LuaSource.cpp` 有 14 个下标表达式，每个都建一遍：

```text
members_of(std::vector)            24 ms
× 14 个下标表达式                 340 ms   ← assign-type-check 全部成本
```

一个"某个成员是否存在"的问题，索引里有**单成员**的窄查询（`member_fact`，按名字/scope 的倒排）。
改法：先问窄查询；`Known::Yes` 就直接用它（成员表按同样顺序遍历成员——本类优先、再逐层基类——
所以表里 `find` 到的必然就是这一条，答案相同）。只有窄查询答不出时才回落到成员表。

```text
operator_returns_of                  20 ms → 0 ms（窄查询命中时）
assign-type-check                   135 ms（其余是表达式类型推导本身）
```

顺带记一次**做过又撤回**的改动：`declared_returns_of_with` 只接受 `BindingKind::Function`，
所以 operator 的 fact 从来没有返回类型。放开它看起来是明显的修好，实测是**错误**：
`std::vector<Token>::operator[]` 的返回类型写作 `reference`，而 `reference` 是类内的 typedef，
未替换的名字让 `tokens[i]` 的类型变成"一个叫 `reference` 的类"，
`undefined-field` 因此报出**四个文件 60 条误报**。这条路径要保持"答不出就不答"，
记录在 `declared_returns_of_with` 的注释里，免得下一个人再试。

### ③ 模板参数被当成基类 —— 355 ms 的成员表里 96% 是它

成员表的基类遍历从一个**写法**里取"可以据此查找类的名字"：
`conditional_t<A, B>` 的真实基类确实是括号里的某个名字，所以 `lookup_names` 会取出里面的名字。
但 `_Vector_base<_Tp, _Alloc>` 也照此取出 `_Tp` 和 `_Alloc`：

```text
members_of("std::vector")（当前文件的 scopes，走索引）
  own                    1 ms      282 个成员
  base std::_Bvector_base 1 ms
  base _Alloc             13 ms → 0 个成员
  base std::_Vector_base  1 ms
  base _Tp               251 ms → 0 个成员     ← "谁声明了 _Tp"：整个闭包里上百条声明
  base _Tp 的 bases_of    88 ms
  level 1                355 ms
```

规则本身不是启发式：C++ 里模板参数会遮蔽同名，**一个类自己的模板参数不可能是它的基类**。
`members_of` 现在先取本类的模板参数（`class_template_parameters`，已有函数），基类遍历跳过它们。

```text
members_of("std::vector")   level 1  355 ms → 3 ms
access-invisible            375 ms → 15 ms
argument-count              365 ms → 10 ms
```

## 三、第二轮：三个设计错误（6 575 ms → 1 895 ms）

第一轮之后，一个 2 800 行的文件仍然是 6.2 s。`CPPLS_TRACE_TIME=1`（这一轮新增的探针）打印每一层推导：
382 个顶层问题，引擎合计 **5 923 ms**，其中 `_nodeOrTokens.size() - 1` 两条各 **1 572 ms**。

### ① 推导引擎在顶层以下没有 memo

`SemanticModel::type_of` 按节点 range 缓存，但 `type_of_expression` **直接递归调用自己**，不经过模型：
所以一个表达式内部问过的子表达式，问第二次还是重新推导。

```text
一个 e.U.Start.Kind 的展开（depth 0 → 5）
  e                      8 ms × 11       ← 每次都在重新推导 e 的声明（`auto e = events[i]`）
  events[i]              7 ms × 11
  _nodeOrTokens.size()   783 ms
```

同一个文件的 382 个顶层问题，实际发生的是 **上千次子问题**。

改法：把 memo 变成**引擎自己的参数**（`type_of_expression_memoed(…, &TypeMemo, …)`），
所有递归调用（11 处 + 8 个辅助函数）都走它；`SemanticModel` 把自己的 map 交给引擎，
所以一次请求内（诊断的所有规则 + 所有表达式）共享一张表。

```text
LuaSyntaxTree.cpp：引擎 5 923 ms → 2 900 ms，整文件 6 161 → 3 912 ms
```

**这个改动第一次漏了两处，而漏掉的正是最热的那处。** 那次替换是按单行调用点做的，
而成员访问那个 arm（`x.y`、`x.y()` 都走它）的调用是**多行**的，所以它仍在调**入口**
（每次新建一张 map），而且先问一次 `Yes`、再问一次拿 `Unknown` 的理由——同一个子表达式推两遍。
改成一次、走共享 memo 之后：

```text
LuaSyntaxTree.cpp 整文件 1 671 → 715 ms（这条 arm 覆盖的规则各降 2–4 倍）
  access-invisible         740 → 246 ms
  implicit-bool-conversion 420 → 115 ms
  param-type-check         293 → 156 ms
```

顺带修掉一个潜伏的 `unreachable!`：那条 arm 对 `Known::No` 会 panic（第二次 match 只处理 `Unknown`），
现在答 `UnknownType` 而不是崩。

### ② 基类遍历又一次把模板参数当基类（这次在单成员查询里）

第一轮修了 `members_of`（成员表），但 `member_fact`（单成员查询）的基类遍历没修。探针：

```text
[member_fact]   389 ms  std::vector::size_t
[member_fact]   384 ms  std::vector::size_t
… 8 次，合计 2 280 ms
```

`std::vector::size_t` 这个成员根本不存在，查它是 0 个候选；380 ms 全部花在
`bases_of("std::vector")` 返回的 `_Tp` / `_Alloc` 上 —— 又问了一次"谁声明了 `_Tp`"。
同一个过滤规则（类自己的模板参数不是基类）搬到 `member_fact` 的基类遍历上。

```text
LuaSyntaxTree.cpp 整文件 3 912 → 1 671 ms
param-type-check           1 759 →  293 ms
assign-type-check            825 →  102 ms
```

### ③ `scope_with_qualified_name` 是"每个查询扫一遍所有 scope，并各自拼一个 String"

它是 `direct_member` / `direct_members` / `bases_of` 每次都要问的第一句。原文的实现是

```rust
(0..self.scopes.len()).find(|id| self.qualified_name_of(*id).as_deref() == Some(name))
```

而 `qualified_name_of` 会沿父链把段拼成一个**新的 `String`**。所以一次"这个类在不在本文件里"的查询，
在 2 800 行文件上是几千次 String 分配。实测：推导一个普通局部名 `currentPos` 花了 **780 ms**。

改法：`ScopeTree` 在建 scope 的时候顺手把 `qualified_name -> ScopeId` 存进一张 map（同一个拼接只做一次），
查询变成一次哈希。A/B（单独回退这一项）：**2 064 ms → 1 895 ms**。

### ④ 顺带：`declarator_declaring` 每次问都走一遍整棵树

它原来是 `root.descendants()` 加一层"子树里有没有 Declarator"的嵌套扫描。改成按 range 下降
（名字一定在某一条祖先链上，最深的就是最内层），语义不变。A/B：**1 958 → 1 895 ms**（3%），
所以它**不是**那 5 923 ms 的原因 —— 记录在函数自己的注释里，免得下一个人把它当成大修的入口。

### ⑤ 检查层剩下的 96% 是类型推导（第三轮量的）

前四条修完，一个 2 800 行的文件还要 695 ms。为了知道该往哪走，加了一个 A/B 开关
`CPPLS_NO_INFERENCE=1`（`probes::no_inference`，所有 `type_of` 直接答 `Unknown`）：

```text
LuaSyntaxTree.cpp 整次诊断      正常 695 ms      关掉推导 25 ms
每个规则（正常 / 关掉推导）
  concept-not-satisfied           20 / 20        ← 不依赖推导
  missing-return                   1 /  1
  argument-count                  77 /  3
  param-type-check               150 /  1
  access-invisible               234 /  0（类型未知，规则自己就静默了）
  implicit-bool-conversion       113 /  0
  assign-type-check               95 /  0
```

也就是说：**规则自己的树遍历几乎不要钱，钱全在 `context.type_of` 里**——一次拉取 572 个问题
（`SemanticModel::inference_cost` 计数）。同一个 A/B 也解释了为什么"关掉某一类规则"这种想法永远不会有效：
省下的是 0.1 ms 级。

再把这些问题拆开（每处 >0 µs 都记，按标签汇总）：

```text
[phase] text             572 次    0 ms      ← expression.text().to_string()，不是瓶颈
[phase] declare          307 次    1 ms      ← definition_at（scope 查找）
[phase] named_type       252 次    1 ms      ← 从声明读类型
[phase] mca               18 次   15 ms      ← 成员访问的类/实参解析
[phase] member_fact       18 次   15 ms      ← 单成员查询
[phase] declared_type    252 次  277 ms      ← 其中 auto 推导（initializer_type / range_for_element_type）
```

`declared_type` 的 277 ms 里绝大部分是 **`auto` 推导链**：`auto x = f(...)` 要推导初始化的类型，
而初始化里还可能有 `auto` 声明的变量，于是递归（嵌套的时间在这里被重复计入，所以 277 > 实际）。
这不是开销，是这一层真的在做的工作。结论：剩下的 686 ms 是"572 个问题 × 约 1 ms，其中四成是 `auto` 推导"，
**micro-optimization 到此为止**；再往下需要跨请求缓存或增量推导（§4.3），不是再找一个线性扫描。

顺带把同一个模式里最后一处线性扫描也换了：`ScopeTree::scope_at` 原本是
`scopes.iter().filter(contains).min_by_key(length)`——每个名字查一次，而文件有几千个 scope。
改成"二分最后一个 start <= offset 的 scope，再沿 parent 链向上找第一个包含的"（同一个 O(log n) 做法）。
实测只省 ~1 ms（695 → 686），因为一次过滤只要 2 µs；**保留**是因为它把 O(scope) 换成 O(log n)，
在一个一万行的文件上是十倍的钱，而代码只多一个 `Vec`。

### ⑥ `access-invisible` 里藏着的"每个成员访问走一遍整棵树"

这条规则里有一个 `context.own_syntax_nodes().find(|node| node.text_range().start() == range.start)`
——**每个成员访问都走一遍本文件的整棵树**去找那条成员声明。它没被上面那个 `CPPLS_NO_INFERENCE` 的 A/B 抓到，
因为它藏在类型门后面：类型未知时规则在到达那行之前就 `continue` 了，所以"关掉推导 25 ms"里不含它。

改成"取该 offset 的 token，再沿祖先链找起点相同的最外层节点"（O(depth)），语义不变
（找到之后仍用 `view.was_written_here` 保持"只看本文件写的声明"这条边界）：

```text
access-invisible  234 → 222 ms（2 800 行文件）
```

这条是"量对了地方"的反例记录：**A/B 关掉一层，会把那一层后面的工作一起藏起来**。

### ⑦ 跨请求的成员表缓存：一次按键不该重新认识 `std::vector`

§三.⑤ 的结论是"钱在 `type_of` 里"；这一轮把其中**最容易跨请求复用的一部分**拿出来了。一个类的成员表
不因为读者在别处敲了一个字符而改变，而每次诊断拉取都要问同一批类（`LuaSource.cpp` 上 17 次 / 82 ms）。
`ProjectIndex` 现在带一张 `(class, 用到的那棵树) → 成员表` 的缓存，**按"这张表由哪些文件的事实组成"失效**：

```text
一张表由它每个成员的 `file` 组成；`forget` / `insert_at` / `insert_cooked` 只丢掉
"贡献者里包含这个文件"的条目 —— 编辑 A.cpp 不会丢掉 `std::vector` 的表（它在 <bits/stl_vector.h>），
但会丢掉 A.cpp 里那些类的表（它们的成员就写在 A.cpp）。
```

实测（`edit_loop`，每次按键拉一次诊断）：

```text
LuaSyntaxTree.cpp（2 800 行）   第一键 734 ms → 之后 619–635 ms（−16%）
LuaSource.cpp（6.5 KB）         第一键 154 ms → 之后 139–142 ms（−11%）
四文件审计（每文件只拉一次）    875 ms（没有跨请求可复用，符合预期）
```

**中途量到一个把缓存废掉的细节**：`insert_cooked` 原来无条件失效。而泵每个 slice 都会重新 cook 同一个文件
（被编辑的文件，和 `want_the_closure_cooked` 排进来的闭包头），于是"内容没变的重煮"每键都把缓存清空——
三键内 15 次命中、59 次重算。改成"只有 cooked 的 declarations 真的不同才失效"之后是 **37/37**。
一个缓存的有效性有时不取决于它的键，而取决于**谁在无谓地宣布失效**。

内存：96 条 × 一张表（最大的 `std::vector` 307 个成员），量级 ~10 MB，超了整张清空
（同一批类总是一起被问）。

### ⑧ 两条规则的前置过滤：能不问就不问

`access-invisible` 只在**本文件声明的成员**上可能报告（模块文档的第一条），而它原来对每个成员访问
都先求对象类型。先按"本文件的 fact 里有没有这个名字"过滤：

```text
access-invisible  231 → <1 ms（2 800 行文件）
```

`implicit-bool-conversion` 只报告"算术且不是 bool"，而 `if (a == b)`、`if (p && q)`、`if (!x)` 的类型
由语法就定死了（内置运算符是 bool，重载的运算符是类，两者都不是算术）。先判语法再求类型：

```text
implicit-bool-conversion  117 → 28 ms
```

**但这一次总时间几乎没动**（884 → ~870 ms，重复跑有 ±3% 的抖动），因为 `undefined-field` 本来也要问同一批成员访问——
模型的推导 memo 是两个规则共享的，所以**优化一个规则的问题，只有当没有别的规则问同一个问题时才省时间**。
这是这一层最后的形状：一次拉取的工作量是"文件里每个成员访问 × 一个类型"，被几条规则分摊，
谁先问谁付钱。要再往下只能让"这个类型"本身变便宜——即 §4.3 的跨请求缓存或增量推导。

## 四、等价性：findings 一条没变

性能改动最容易悄悄改变答案，所以每次都用同一份输入对比**全部 finding**（`examples/diagnostics_audit.rs`，
输出 `path:line:col code message`，排序后 diff）：

```text
四个 LuaParser 文件
  改前（HEAD）  284 648 ms  18 findings
  第一轮之后      6 575 ms  18 findings
  第二轮之后      1 895 ms  18 findings
  第三至六轮之后     868 ms  18 findings
  diff                                            ← 空（每次都是）
```

测试：`cargo test --release --workspace`。唯一失败的
`an_unresolved_include_does_not_erase_the_files_own_facts` 在 HEAD 上也失败（用 `git stash` 验证过），
不是这次改动引入的；`handshake` / `latency` 两条在整套并发跑时会红、单独跑会绿，
是 `architecture-review.md` P1-1 / P3-8 记的墙钟与顺序依赖。

第六轮结束时 `a_completion_sees_a_change_to_an_included_header` 在整套并发跑里红、单独跑绿（0.22 s），
与前几轮一致；`an_unresolved_include_does_not_erase_the_files_own_facts` 则是 HEAD 上就有的失败。

## 五、冷启动第二遍：欠账现在算工作量（已修），27 s 不再是浪费

### 4.1 已修：第二遍的欠账不再记在打字上

`index_everything()` 在索引队列空时返回，但**第二遍**（被包含的头入索引后重读文件）只由一个 slice 的尾部
搬运、一次 4 个文件，而且 `pending_work()` 里**不算它**。冷摘要缓存下一次实测：

```text
                      修之前                    修之后
index_everything      4.4 s，返回时欠 1 205 个文件   31.9 s（欠账一起做完），返回时欠 0
之后打字的 pump        65 ms/键（最长 152 ms），约 300 键   9.7–19 ms/键
```

改法两条，都很小：

```text
· pending_work() 把 parsed_since_the_last_pass 也算进来 —— 欠着重读的会话不是空闲会话
· index_everything 的退出条件加上"第二遍欠账为 0"（它原来在 read 0 / cooked 0 就返回）
```

服务器的泵循环就是 `while pending_work() > 0`，所以它现在会在冷启动后**继续跑**直到第二遍做完；
这段时间的回答（读锁共享）不受影响，只有写要等每一片的读锁（和以前一样）。

**代价，如实记下。** 第一，`index_everything` 从 4.4 s 变成 31.9 s：这 27 s 本来就是欠着的，现在它出现在
"索引完成"这句话里，而不是藏在"下一次按键"里。第二，`checks_may_be_trusted` 的门是
`pending_work() == 0`，所以冷启动后约 30 s 内不发布 check findings——这和 cooking 被计入同一扇门的理由一样：
第二遍正要修正的摘要，不能当成事实来断言。

### 4.2 还没修：那 27 s 里没有重建，但它不是白做的

**这一节的结论在第三轮被量翻了，原文写的是"白做"。** 第二遍的每一片 trace 都是 `nothing bodied` ——
没有摘要被重建 —— 于是看起来像纯粹的浪费。加了 `Session::dead_macro_decisions()` 之后：

```text
第二遍走完 1 192 次单元走查，记录 1 089 个文件 / 15 417 个"编译器不会读的 #define"位置
```

这些位置（`SummaryStore::dead_macros`）是第二遍的**隐形产品**：它决定一条在 `#if` 分支里的 `#define`
要不要进摘要，而这恰恰是之后每一次 `prepare` / 编辑重读摘要时用的（`_STL_LANG = 0L` 那一类）。
所以跳过走查不是零成本：它会丢掉这些判定。

成本分解（`CPPLS_TRACE_TIME=1` 的 `[unit]` 行，1 194 次走查）：

```text
read 2 367 ms   （closure 的文本；VFS 已持有，所以只是拷贝）
walk 9 348 ms   （时间线：平均每次走 240 个文件 / 20 MB 的指令流）
put  3 674 ms   （把单元写进磁盘缓存，给下一次运行）
+ pass 本身 ≈ 7 s
```

也就是说每次走查都在走**整个程序**（windows.h 375 文件 / 9 MB；LanguageServer.cpp 1 093 文件 / 19.8 MB），
因为"这个文件单独编译时的环境"就是它的闭包。三个方向，按风险从低到高：

```text
· dead_macros 是"最后写赢"的 map（prepare_the_re_read 里是 insert，不是 merge）：
  1 089 个文件被走了 1 192 次，只有最后那次的判定留在表里。所以"走足够的 root 覆盖所有文件一次"
  与"每个文件走自己的 root"给出的表大小相同 —— 差别只在判定取自哪个时间线，这一点必须先量等价性
· 先判后走：pass 的 candidate 扫描只读文本和 plain shape；只有 plain shape 是 Other 且 body 里
  提到某个已知宏名的，才需要环境 —— 需要把 units 改成惰性
· 单元复用（已被否掉）：跨 root 借一个单元来回答 environment_of 在语义上是错的 ——
  单元是"这个文件单独编译"的时间线，换一个 root 就是另一个环境
```

下一轮的第一件事：量第一项。做法是把"覆盖所有文件所需的最少 root 集合"（先工程文件，再未被覆盖的头）
走一遍，得到的 `dead_macros` 与今天逐文件走的结果逐条对比 —— 相同则实现，不同则只有工程文件这一半可用。

### 4.3 大文件仍然是秒级：LuaSyntaxTree.cpp 0.68 s

```text
第四轮之后（LuaSyntaxTree.cpp，2 800 行，整次诊断 ~680 ms；重复跑 ±3%）
undefined-field          293 ms      ← 第三轮它在 4 ms：access-invisible 不问了，它来问同一批类型
param-type-check         134 ms
assign-type-check         95 ms
argument-count            78 ms
implicit-bool-conversion  28 ms      ← 语法前置过滤，117 → 28
concept-not-satisfied     20 ms
access-invisible         <1 ms      ← 前置过滤 + O(depth) 查找，237 → <1
```

第一轮之后这里是 6 248 ms（四条规则 1 055 / 531 / 2 745 / 1 808）；第二轮降到 1 671；
第三轮把成员访问那条 arm 的重复推导补上后到 ~700；第四轮的两条前置过滤到 660
（`LuaSource.cpp` 的整次诊断 174 → 157 ms）。

**归因已经清楚（§三.⑤、⑧）**：这 660 ms 的 96% 是 `context.type_of`——572 个问题，
其中四成是 `auto` 推导链，而且几条规则共享同一张 memo，所以"某条规则不问"只是把同一笔钱转给另一条规则。
下一个数量级不在这层代码的微优化里，而在这两个方向：

```text
· 跨请求缓存：`(path, 渲染哈希)` 不变时，同一批类型问题的答案是同一批。今天每次拉取都从头推导，
  而大多数按键只改一个函数体；缓存键必须是**渲染**的哈希（文件的读数变了，答案就失效）
· 增量推导：只重算被编辑区域所在的声明链。引擎的 memo 是"节点 range → 答案"，可以随树一起复用，
  但需要先让两次拉取共享一棵树（今天每次 `view` 都重新 parse 渲染）
```

### 4.4 每按键仍然重建一次 cook

`cook render` 4.8 ms = lex（6.5 KB）+ 宏展开 + 解析渲染 + 191 条 fact。这一项与按键文本长度成正比，
在 6.5 KB 的文件上是 5 ms；`docs/prefix-reuse.md` 记的前缀复用（共享 green tree）如果做出来，
能省的也是这一段的解析部分，但起点已经不是 17 ms 而是 5 ms。

**大文件上这一项已经是每键最大的一笔。** `LuaSyntaxTree.cpp` 的按键 pump 是 60–63 ms，拆开（第五轮量的）：

```text
[cook] 133 KB 的渲染       lex 0 ms      cook（宏展开 + 渲染）9 ms      parse + index 53 ms
[cook] 90 KB 的其余文件                                                 parse + index 22 ms
```

即 `FileIndexer::index_rendering` 里的 parse + facts + scopes 就是那 53 ms。它与"文件的渲染有多大"成正比，
与按键改了几个字符无关——这是 `docs/prefix-reuse.md` 那条线（共享 green tree / 增量解析）唯一还值得做的地方，
而且它现在比诊断那一层（§三）更值得做：诊断 140 ms，pump 在大文件上是 60 ms。

## 六、怎么复现

```text
# 一次按键的逐阶段成本（会自动清掉第二遍欠账，先给你稳态数字）
cargo run --release -p cpp_code_analysis --example edit_loop -- \
    E:\EmmyLuaCodeStyle E:\EmmyLuaCodeStyle\LuaParser\src\File\LuaSource.cpp 8

# 按规则计时
$env:CPPLS_TIME_CHECKS="1"; …同上…

# 全项目 findings 基线 + 每文件耗时（--match 选一个子树，--time 打最慢的文件）
cargo run --release -p cpp_code_analysis --example diagnostics_audit -- \
    E:\EmmyLuaCodeStyle --match luaparser/src --files 4 --time

# 冷启动：项目自己的 .cppls.toml + 一个只用一次的 cache_dir，不动用户在用的缓存
Copy-Item E:\EmmyLuaCodeStyle\.cppls.toml target\cold.toml
Add-Content target\cold.toml "`n[index]`ncache_dir = `".cppls-cold`""
$env:CPPLS_PROJECT_CONFIG="E:\cpp_ls\target\cold.toml"; …edit_loop…; Remove-Item -Recurse E:\EmmyLuaCodeStyle\.cppls-cold
```

`Session::re_read_backlog()` 是新增的仪器：第二遍的欠账数。它现在**也在** `pending_work()` 里，
所以"没有工作"的会话和"还没有第二遍"的会话不再混为一谈。

`CPPLS_TRACE_TIME=1`（`probes::timing`）打印每一层类型推导（>5 ms 的才打，带 depth）和每次
"为了回答一个问题去读另一个文件"（`[resolver]`）。§三 的三个设计错误都是它指出来的：
没有它，5 923 ms 只能看出"这个文件慢"。

**一个容易踩的坑**：摘要缓存的名字里含 `READING_FINGERPRINT`——读者源码的哈希。
所以**任何一次改动 `cpp_code_analysis` 的读取代码，整份缓存都不可达**，下一次启动必然是冷启动
（`cache::prune` 会把它们扫掉）。上面所有"冷"数字都是这种状态；要量"热"，同一个二进制连跑两次。
