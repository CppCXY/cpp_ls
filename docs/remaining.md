# 还要修的东西

这份文档是给**后面接着做的人**看的：每一条都是实测定位过的、写在真实项目（`E:\EmmyLuaCodeStyle`）上的问题。
不是愿望清单 —— 已经修掉的写明"修在哪、怎么钉住的"，还没修完的写明现象、根因方向和验收方式。

先读 `docs/architecture.md` §9：那里记了九个真缺陷和"读整个项目"为什么被删掉。

---

## 本轮修掉的三条（2026-10-27）

三条都是"用起来觉得不对"报上来的，不是读代码读出来的。每条都有**事实层**的夹具断言
（`crates/cpp_code_analysis/tests/reported_defects.rs`）和**端到端**的卡片断言
（`crates/cpp_ls/tests/handshake.rs::a_hover_names_the_instantiation_and_never_invents_a_void`），
外加真实项目上的验收（`crates/cpp_ls/tests/real_project.rs::the_two_reported_hovers_answer_on_the_project_they_came_from`）。

### ✅ A1. 类模板的实例化拼写没有显示出来

**现象**：光标在 `std::map<std::string, LuaTokenKind, std::less<>> LuaLexer::LuaReserved = { … };` 的 `map` 上，
卡片是 `class map` + "a type template over `_Kty`, `_Ty`, `_Pr`, `_Alloc`" —— 读者写的是实例化，卡片只有头文件里的形参名。

**根因**：`declaration_markdown` 用**事实**重建声明，而事实记的是*声明*那一面（`pattern` / `parameters`），
不是*使用*处写出来的拼写。

**修在哪**：`crates/cpp_ls/src/handlers/hover/mod.rs`

* `written_template_id`（新）：用 `sema::resolve::name_node_at` 取光标处最内层的名字节点；解析器把整个限定名连实参表
  折进**一个节点**（见 `read_named` 的注释），所以它的文本就是拼写。`None` 当名字没有实参表（`Widget`）。
* `declaration_markdown` 多一个 `written: Option<&str>`：当事实是 `DeclKind::Type` 且 `parameters` 非空时，
  **签名行与句子主语都用这个拼写**，形参名留在后面那句里。

**真实项目验收**（`LuaParser/src/Lexer/LuaLexer.cpp:10`）：

```text
std::map<std::string, LuaTokenKind, std::less<>

`std::map<std::string, LuaTokenKind, std::less<>>` — a type template over `_Key`, `_Tp`, `_Compare`, `_Alloc`, in `std`
```

### ✅ A2. 匿名 struct 的变量被显示成 `void`

**现象**：`static const struct { unsigned char left; unsigned char right; } priority[] = { … };`
hover `priority` → `void[] priority`。

**根因**：`read_specifiers_named` 里 `ClassDef | StructDef | UnionDef | EnumDef | EnumClassDef` 那几个分支是空的
（"类体本身不是它的名字"），而**未命名的**类体是说明符序列里唯一的东西 —— 于是函数结尾回落到 `void`。

**修在哪**：`crates/cpp_code_analysis/src/sema/types.rs`

* 未命名的类体让说明符序列返回**空拼写**（`Type::builtin("")`），不是 `void`：`void` 是另一个类型。
* `type_of_declaration_named` 见到空拼写就原样返回 —— `priority[]` 不是 `[]`，它是"一个说不出来的类型的数组"。
* `declared_type_of` 本来就把空拼写读成"没有类型"，而 hover 的 `declaration_text` 于是走原文那条路；
  `crates/cpp_ls/.../hover/mod.rs` 里加了 `the_declarations_start`：变量的 `range` 是**声明符**
  （`priority[] = { … }`），原文要从**声明的起点**切（`static const struct`），并且这种情况下不按第一个 `{` 截断
  （那个 `{` 是类型体的开始，不是函数体）。

**真实项目验收**（`LuaParser/include/LuaParser/Lexer/LuaDefine.h:17`）：卡片是文件自己的原文，
含 `unsigned char left;` / `unsigned char right;`，不再是 `void[]`。

### ✅ B1. 有些类模板事实的 `parameters` 是关键字

**现象**：一份 `std::vector` 声明的 `parameters` 是 `["class", "class"]`（关键字，不是名字）。本机可复现的形状是
libstdc++ 的**无名形参**：`template <typename, typename> class vector;` → `["typename", "typename"]`。

**根因**：名字取"`=` 之前最后一个**词**"，而无名形参的最后一个词就是引入它的关键字。

**修在哪**：`crates/cpp_code_analysis/src/sema/types.rs` 的 `template_parameters_in`

* 名字改从 **token** 取（最后一个 `Identifier`，遇到 `=` 就停），关键字不可能成为名字；
* **无名形参留一个空槽**：实参是按位置和名字配对的，丢掉一个会让后面所有名字左移一位
  （`template <typename, class T>` 的 `T` 会拿到第一个实参）；
* `is_a_parameter_name` 从"挡住错误事实"变成"替换需要的形状检查"，并新增
  `lists_usable_parameter_names`：**有名字、且槽位之外每个条目都是名字**的列表才可用来替换 ——
  于是 `["", "T"]` 现在能替换了，`["", ""]`（`vector` 那种前置声明）仍然不能；
* 顺带修了 `substituted_spelling` 里一个越界读取：名字比实参多时（用了默认实参的典型情形）它 `arguments[at]` 会 panic，
  现在缺实参就保留声明里的拼写。

**回归**：`reported_defects.rs` 的三条 + `an_arrow_through_a_base_named_by_a_member_alias_in_another_file`、
`a_class_templates_members_…` 全绿；`cpp_code_analysis` 全套、`cpp_ls` 全套、`cpp_parser`/`cpp_formatter` 全绿。

---

### ✅ C1（本轮也修了）. `members_real.rs` / `signature_real.rs` 只对一台机器上的那个项目成立

它们把 `E:\EmmyLuaCodeStyle` 写在默认值里，在别的机器上是红的。现在改成 `tests/inference_probe.rs` 与
`real_project.rs` 的同一种写法：项目从 `CPPLS_REAL_PROJECT` 读，**目录/文件不在就打印 `skipped: …` 并返回**，
不是失败。验证过两种机器状态：

```text
$env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"   → 2 passed / 1 passed（真项目上仍然在跑）
$env:CPPLS_REAL_PROJECT = "E:\does-not-exist"     → skipped × 3，全绿
```

以后加真实项目探针，都照这个形状。

## 本轮第二批（2026-10-27，同一个提交 `25e5733`）

四条来自同一个编辑器会话的四张截图，每条都在项目上量过。

### ✅ B2. `switch (0) case 0: default: …` 不解析 —— gtest 整个文件报错

**现象**：`Test/src/FilePattern_unitest.cpp` 里每条 `TEST(...)` / `EXPECT_TRUE(...)` 都报满屏红：`62 个 SyntaxError`
（`expected {, but get case`）。看起来像"预处理器没做好"。

**实测**：**渲染是正确的** —— `CPPLS_RENDERING=1` 打出来的是 gtest 真正的展开（一个类定义 + `TestBody()` +
`switch (0) case 0: default: if (...) ; else AssertHelper(...) = Message();`）。所以错的不是预处理器。

**根因**：`parse_switch_statement` 在条件后面**要求 `{`**。语言允许任何语句作为 body，而 gtest 的 `GTEST_SWITCH_`
正是"`case`/`default` 标签 + 一条语句、没有花括号"。

**修在哪**：`crates/cpp_parser/src/grammar/cpp/stats.rs` —— 抽出 `read_switch_labels`（块形式与非块形式共用），
非块形式读"标签串 + 一条语句"。

**验收**：该文件诊断 **62 → 0**；`tests/gaps.rs` 增加 `switch (0) case 0: default: …`、`switch (a) f();`、
`switch (a) case 1 ... 5: break;` 三条到"读得懂"清单。

### ✅ A3. `auto` 变量的悬停显示成 `void`

**现象**：`auto lineStartOffset = _lineOffsetVec[line];`，hover 打印 `void lineStartOffset;`。

**实测**（`CPPLS_TRACE_INFERENCE=1`）：

```text
[inference] operator_returns_of(std::vector, operator[]): parameters ["_Tp","_Alloc"], arguments ["std::size_t"]
[inference] subscript on "std::vector<std::size_t>": the member wrote "reference", the type is "void"
```

**根因（两层，都修了）**：

1. `typedef typename _Base::reference reference;` 的**类型**被当成"声明符自己的名字"丢掉了（那条规则是给类外定义
   用的：`std::map<…> LuaLexer::LuaReserved`）。别名永远不需要那条规则 —— 于是 `reference` 的类型成了 `void`。
   修在 `sema/declarations.rs::declared_type_of_with`：`BindingKind::Typedef` 不再传 declared name。
2. `read_named` 在**第一个 `<`** 处切断拼写，`Base<T>::reference` 于是成了 `Base<T>` —— 名字里少了 `::reference`，
   `Foo<int>::Bar<int>` 还丢后半截。改成"只有**结尾**的实参表才是 template-id"
   （`trailing_template_id`，带括号深度栈）。

**验收**：真项目上 `lineStartOffset` 从 `void` 变成 `_Alloc_traits::reference`（不再是假答案，见下面"还没修"）；
`reported_defects.rs::an_alias_of_a_qualified_type_is_not_a_void` 钉住类型是 `typename Base<T>::reference`。

### ✅ C4. 进度条卡在 `re-reading 9 file(s)` —— 工具补上了，结论是"没能复现"

**实测**：用一个**真正读取管道的客户端**跑真服务器 90 秒，pump 在 **9 秒**内抽干并打印
`the workspace is indexed`；之后不再有 slice 日志（说明它睡了）。**卡住的是我这台测试里的客户端**：它只在等自己
的响应时读 stdout，于是服务器写 `$/progress`（7 秒里 418 条）填满 64 KB 管道，**阻塞在写**，CPU 为 0、进度条不动 ——
和截图一模一样，但不是产品的 bug。`real_project.rs` 的 `Server` 现在从第一字节起就在独立线程里排空管道。

为了让"真的再出现时"能定位，补了两件工具：

* `--log-level trace`（此前**没有**，而代码里已有 `log::trace!`）；
* **pump 心跳**：每轮 5 条 trace（round / wave prepared / drain starts / commit starts），因为"找不到事做的 slice"
  在 `debug` 下什么都不打，卡在哪个 `await` 从外面看不出来。

**还没排除的**：截图里的 9 和文档里那条老 livelock（"每秒要同样 9 个 cook，提交 0"）是同一个数字，那条已经被
`abandon_the_cooking_queue` 兜住；如果它再出现，`trace` 日志会直接指出是 pump 卡在 workspace 锁、analysis 锁还是 drain。

---

## 还没修
### ✅ A4. 依赖形式的限定别名（`LuaReserved.find(text)->second`、`_lineOffsetVec[line]`）—— 2026-10-27 收尾

**本轮做完的那一半**（`through_an_operator_arrow` 的第二次改动）：`operator->` 的返回拼写先跑一次
`a_spelling_through_the_aliases`，再取 pointee。

```text
修前  find(text)->second  → Unknown(NotDeclaredHere("std::_Rb_tree_iterator::second"))   （把 `pointer` 当成类）
修后  find(text)->second  → std::conditional<…>::type::second                            （`pointer` 跟到了值类型）
夹具  an_arrow_through_an_alias_reaches_the_pointee：`map<int,float>::find(1)->second` → `pair::second` ✓
```

同时**删掉了第一版用的 thread-local 深度守卫**：仓库里有 `tests/no_global_state.rs` 的 `no_thread_local_state`，它当场
把那条实现判红。替代方案是 `a_member_the_class_declares` —— 别名走查改用 `members_of`（走基类、从不问箭头），于是
"箭头 → 别名 → 成员 → 箭头" 这个环从结构上不存在，不需要运行时守卫。**以后写这个文件里的递归，先想这条**。

**现在的残差（实测）**：`find()->second` 停在 `std::conditional<B, T1, T2>::type`。原因是**trait 别名**：

```cpp
template <bool B, class T, class F> struct conditional          { using type = T; };   // 主模板
template <class T, class F>         struct conditional<false,T,F>{ using type = F; };   // 偏特化
template <bool B, class T, class F> using conditional_t = typename conditional<B,T,F>::type;
```

`conditional_t<__is_same(...), X, Y>` 要能答 `X`/`Y`，需要"**按实参选偏特化，再读它的 `type`**"。同一形状的还有
`remove_extent_t`（`shared_ptr` 的 `element_type`，见 E2）和 `enable_if_t`（C2 的
`a_trait_a_builtin_decides_selects_the_right_specialization`，实测现在答 `"type"` 而不是 `"double"`，**差一步**）。

**下一步（按顺序）**：

1. 让 `conditional<B, T1, T2>` 的解析落到**匹配的声明**上：`member_fact`/`direct_member` 已有 `a_pattern_matches` 与
   builtin 折叠（`__is_same(...)` 折成 `true` 的那套），缺的是"**选声明**"这一步在 trait 别名里也被调用；
   建议入口：`a_spelling_through_the_aliases` 的限定符分支（`typename conditional<…>::type` 走的就是它）。
2. `types.rs` 的那条已知红之所以还没被这条链带上，是因为它的答案来自**声明自身**的 `type_of`
   （`NamedDeclaration::type_of` 的 `Indexed` 分支，`crates/cpp_code_analysis/src/index/project.rs:4360` 一带），而
   alias/`finish_a_nested_name` 的走查只挂在**成员访问**那条分支上（同文件 3886–3923）。把同一段挂到
   `Indexed` 分支需要一个能拿到 `index` 的入口（现在的签名没有），这也是那条 ignore 注释里说的"两条不同路"。

**怎么验收**：`inference_probe` 在 `LuaSource.cpp` 上 `type_at` 给出 `std::size_t&`；`map<…>::find` 的 `->second`
答到 `pair<const K, V>::second`；把 `types.rs:924` 的 `#[ignore]` 去掉后绿。

**本轮（2026-10-27 第二批）修掉的那一段**：trait 别名在**嵌套名/成员**那条路上已经通了 ——
`enable_if_t<__is_same(int,int), double>` 答 `double`（`types.rs` 那条已知红去掉了 `#[ignore]`）。两处根因：

1. `finish_a_nested_name` 的 `asked` 列表在**问之前**就塞进了要问的名字（`chain.push(member)`），于是第一跳被
   "already asked" 挡掉，答案原样返回成成员名（`Some("type")`）。链里应当只放**已经解析过**的名字。
2. 代换用的是**对象类型**的 bindings（`Widget` 的），而 `enable_if<…>::type` 里的 `_Ty` 属于 `enable_if`；
   要按**被查类**的形参和实参建 bindings，且实参必须用 `split_arguments`（**认括号**）拆分 ——
   `Type::arguments` 会把 `__is_same(int, int)` 拆成两个，实测答 `Some("int)")`。

**残留（真实项目）**：`find()->second` 仍停在 `std::conditional<_Test, _Ty1, _Ty2>::type::second` —— 拼写里是

**2026-10-27 第三批：又走了两跳，卡在一个更具体的地方。** 这一批加了三样东西（都留着）：

```text
① a_spelling_through_the_aliases 的实参改用 split_arguments 拆（认括号）
   此前 Type::arguments 把 enable_if<__is_same(int, int), double> 拆成三段，代换答出 Some("int)")
② 别名走查会跟随【声明在别处的别名模板】：conditional_t<false, void, T> → conditional<false, void, T>::type
③ 限定符分支不再因为"递归回来没变"就停：限定符本身就是类时继续问它的成员
```

夹具 `a_trait_alias_selects_the_specialization_its_arguments_name`（`session/tests.rs`）复现到这一步：

```text
h->   →  class "conditional_t"      连不上，记录为 known red
追踪：follow 到 conditional<false, void, T>::type，特化声明找到了，然后卡在【形参配对】
     特化 conditional<false, T, F> 只有 2 个形参，使用处写了 3 个实参；
     位置配对把 F 配给了第 2 个实参，答出 void（一个*错的*类型，比"不知道"更糟）。
```

**下一跳的根因（实测）**：那份成员事实的 `pattern` 是 `None`（`CPPLS_TRACE_INFERENCE=1` 打印），所以"按 pattern
配对形参与实参"没有东西可读。两条路，任选其一：

1. 在**事实层**把类写出来的 `pattern` 记到它的成员上（`pattern_of` 只在 ClassDef/StructDef/UnionDef 的 *shape*
   上算，实测成员拿不到）；
2. 在**查找层**按**实参个数**选声明：`conditional<…>` 的主模板有 3 个形参、特化有 2 个，使用处写了 3 个 —— 先按这个
   选，再谈 pattern。

**目前不要做的事**：把 ②③ 单独合进去而没有配对规则 —— 那样 `h->` 会答 `void`（一个错的类型），而项目自己的规则是

**2026-10-27 第四批：事实层补上了，夹具通了。** 上一批的两条路里走了 ①，而且它确实是根因：

```text
事实层：fact_from_binding_with（缓冲区里声明的类，成员事实走这条路）
        以前 parameters: Vec::new()、pattern: None、"substitution 是调用方的事"
        现在与索引里的事实一致：parameters / parameter_defaults / pattern 都填上
理由：事实必须"不论来自缓冲区还是索引，说的都是同一件事"；实测同一条 using type = F; 
     索引里的 pattern 是 Some("conditional<false, T, F>")，缓冲区里是 None，两个答案因此不同
配套：declared_template_parameters_with / declared_parameter_defaults_with / pattern_of 改为 pub(crate)
```

再加上上一批撤掉、这一批重新落地的三处（现在有配对规则撑着了）：

```text
② 别名走查会跟随声明在别处的别名模板（conditional_t<…> → conditional<…>::type）
③ 限定符本身就是类时不停止（继续问它的 type）
④ 按【声明的 pattern】配对形参与实参：conditional<false, T, F> 有两个形参、三个槽位，F 取第三个实参
```

**夹具已转正**（`session/tests.rs::a_trait_alias_selects_the_specialization_its_arguments_name`，不再 `#[ignore]`）：

```text
template <typename T> struct handle { using value_type = conditional_t<false, void, T>; value_type *operator->(); };
handle<Item> h; h->     →  class "Item"   ✓
```

**真实项目仍停在一步之外**，而且这一批的追踪说明原因不在别名走查里：

**2026-10-27 第五批：又前进一跳，真实项目卡点换了地方。** 这一批两件事：

```text
① 夹具 a_calls_iterator_answer_reaches_the_element_through_a_trait_alias
   `map_like::find(1)->second`（调用 → 迭代器 → pointer 别名 → trait 别名）在本仓库内**直接通过**
   —— 说明这条链本身没有问题，真实项目的差异在别处
② finish_a_nested_name 增加"限定名先过别名走查"：`_Mybase::iterator` 这类【自己类的成员别名】
   以前读不动（那个循环一次只看一个裸名），现在会按类的作用域解析限定符再取成员
   夹具 a_calls_return_type_is_a_qualified_alias_of_its_own_class：`tree<int>::find(1)` → `int*` ✓
```

**真实项目的新卡点（实测）**：

```text
LuaReserved.find(text)  →  type_at: "_Mybase::iterator"     ← 修复后仍然是它
   · 这个返回值是 `_Tree::find` 的声明拼写；`_Mybase` 是 `_Tree` 自己的成员别名
   · finish_a_nested_name 的限定名分支【没有让它变化】，说明 `a_spelling_through_the_aliases` 在
     `std::_Tree` 上没解析出 `_Mybase`（下一步要在这个函数里加 CPPLS_TRACE_INFERENCE 打印确认）
   · 另一条候选：type_of_a_call 里 `iterator_element_type`（begin/find/… 的特殊处理）
     对 `std::map<…, std::less<>>` 返回 None，于是走了普通路径
```

下一步（明确）：在这两处之一加打印，确认 `_Mybase` 是在"解析限定符"还是"选声明"处丢的；夹具
`a_calls_return_type_is_a_qualified_alias_of_its_own_class` 已经把"本仓库内应当怎样"钉住了。



**2026-10-27 第六批：卡点分成了两半，都有实测证据。**

```text
[inference] qualified: "_Mybase::iterator" -> qualifier "_Mybase" resolved
            "_Tree<_Tmap_traits<_Kty, _Ty, less<_Kty>, allocator<pair<const _Kty, _Ty>>, false>>",
            member "iterator", fact None
[inference] alias step: "_Mybase::iterator" in Named { name: "std::map", arguments: [] }
```

两件事：

```text
① 这次调用拿到的 TypeBindings 是【空的】：class 拼写因此没有实参（`std::map`，不是 `std::map<string, LuaTokenKind, …>`），
   限定符 `_Mybase` 的目标里 `_Kty`/`_Ty` 就没人代换 —— 残差里的参数名就是这么来的。
   位置：type_of_a_call → declaration_of_a_callee 造出的 NamedDeclaration 里 bindings 没带上"对象类型的实参"。
   本轮已经为这条准备好工具：`TypeBindings::arguments()`（新）＋ what_a_call_has 现在把 class 连同实参一起交给
   finish_a_nested_name —— bindings 一旦非空就直接生效。
② `_Tree<…>::iterator` 查回 None：`_Tree` 自己没有 `iterator`（它在 `_Tree_val`/`_Tree_base` 基类里），
   而 a_member_the_class_declares 的 members_of 走基类那一步没拿到它。下一步：给这个查找加打印，确认
   members_of("std::_Tree") 的基类表与 unlisted。
```

本轮还留了一处**可用的追踪**：`a_spelling_through_the_aliases` 的限定符分支在 `CPPLS_TRACE_INFERENCE=1` 下会打印
（written、qualifier、resolved inner、member、命中的 fact），这就是上面这段输出的来源。
```text
LuaLexer.cpp  LuaReserved.find(text)->second
  → class "conditional_t": 0 member(s)     且 CPPLS_TRACE_INFERENCE=1 下【没有任何 alias step 日志】
  说明：这一次查询根本没走到箭头/别名走查 —— 对象类型（`find(...)` 的返回类型，`_Tree_iterator<…>`）
        在这一步之前就已经被当成了 `conditional_t`。下一步要查的是【调用返回类型】那条路
        （type_of_a_call → what_a_call_has → member_bindings），而不是 a_spelling_through_the_aliases。
"一个错的类型比没有更糟"。所以这一批只留了 ①（严格更好），②③ 的代码在夹具里被 `#[ignore]` 钉住。
**形参名**（没被实参代换），说明这一跳走的是**箭头**那条路（`a_spelling_through_the_aliases` 的限定符分支），
它内部的代换还没像上面那样接上"被查类的 bindings"。下一步就是把同一段搬进那条分支。


### 其余

### C2. 已知红（`#[ignore]`，语义缺口，不是回归）

```text
tests/checks.rs:1685   a_named_parameter_is_checked_too
                       `int` 实参对 `int width` 形参，两侧都解析不出来（见 docs/generics.md）
tests/completion.rs:1423  a_constructors_parameters_are_in_scope
                       一条测量，不是断言
tests/nested_conditional.rs  生效条件的判断（文件头注释解释了为什么现在会失败）
tests/macros.rs:37    宏查询的一个已知缺口（文件里有 "# Why this is #[ignore]d"）
```

**已经不再是红的**：`tests/types.rs` 的 `a_defaulted_template_parameter_of_a_variable_template_substitutes`
（默认实参进了事实）与 `a_trait_a_builtin_decides_selects_the_right_specialization`（见 §A4，本轮去掉了
`#[ignore]`，41 条 `types` 全绿）。
```

### C3. 一次未复现的抖动

`cpp_ls` 的 `handshake` 曾经出现过 **33 passed / 1 failed**，随后同一条命令连跑两次全绿。没有抓到失败的名字。
**以后再现时**：先记下失败的测试名和完整输出，再判断是"索引还没排空就断言"（多数是这个）还是真的行为不一致。

### D. 不确定性（`docs/architecture.md` §7.12）

同一个项目跑两遍，`xfilesystem_abi.h` / `xmemory` 的**摘要事实数**会差 120–1 000 条，而**答案**是稳定的
（36/36 个名字一致）。**根因方向**：第二遍（每个文件按自己的闭包重读）对某些文件的覆盖取决于时序。

### E. 可选的性能工作（不是缺陷）

1. **打开文件的 TU 仍然每次启动重读一次**（闭包 + 渲染 + 解析，热态约 0.4 s、冷态约 2.8 s）。clangd 的对应物是
   **preamble 缓存**：把渲染结果按（文件集合 + 宏环境）持久化。位置：
   `crates/cpp_code_analysis/src/index/store.rs` 的 `a_timeline_of_its_own`、`prepare_the_re_read`。
2. **trait 形态的 `element_type`**：`remove_extent_t<_Ty>` 现在靠"退回到类模板自己的实参"绕过去
   （`the_class_an_arrow_points_at` 的最后一段）。真正的做法是跟随 `remove_extent_t<T>` → `T`。

---

## F. 记分板：整个项目的成员问题，以及它点出的三个普通 BUG

```text
CPPLS_REAL_PROJECT=E:\EmmyLuaCodeStyle \
  cargo test --release -p cpp_code_analysis --test resolution_rate -- --ignored --nocapture
```

**为什么是这个问题**：`x.member` / `x->member` 是编辑器必须回答的问题，答案只有三种 —— 找到声明、带理由拒绝、
没有答案。第一个数是产品，第二个里的理由就是工作清单；而 `UnknownType` 那一类正是模板推理的墙。用**文件自己的
文本**（不是渲染）算，是因为渲染要烹整个标准库闭包（分钟级），而记分板必须能在每次改动后跑（本次 23 s）。

**基线（`2716fa7` 之前，103 个文件）**：

```text
6013 个成员问题   4571 解析（76.0%）   1442 拒绝   0 弃权
按接收者形状：  名字 4695 问 82.0% ｜ 成员链 873 问 34.0% ｜ 调用 350 问 94.9% ｜ 下标 89 问 97.8%
拒绝里：91 个有接收者类型，1351 个没有
```

`873 问 / 34%` 的"成员链"是 `a.b.c` 里取 `b` 的那一步：`a.b` 的类型没算出来，`c` 就没得问。

**探针点出来的三个候选（第一个已证明是"对的回答"，后两个还要一次测量）**：

| 现象 | 例子 | 探针说 |
|---|---|---|
| 类找到了，成员表是空的 | `_owner->SendRequest`，`LanguageServer*` | 从 `CommandService.cpp` 问：`members_of` → 0 个成员；而索引里 **32 条**事实的 `scope` 就是 `LanguageServer` |
| 类本身找不到 | `options.non_standard_symbol`，`FormattingOptions` | 从作废文件问 `NotDeclaredHere`（**这条已证明是对的**：`UriCommon.c` 确实 include 不到 `FormattingOptions`，32 条事实 0 条可见） |
| 已知 std 类型，成员缺 | `inputPath.parent_path`，`std::filesystem::path` | `no list: ConditionalCompilation` —— **文件视图没有宏环境**，`<bits/fs_path.h>` 的类在 `#if` 里；这是记分板已知的悲观项，不是 bug |

**第二行是可用的工具而不是 bug**：`CPPLS_CLASS=<类名>` 会打印"这个 scope 下有多少条事实 / 从某个文件能看见多少条"，
两个数一对比就知道是**没归档**还是**可见性过滤掉了**。

### ✅ 找到了：`@响应文件` 没有展开 —— 这是"索引器"最大的一个洞

```text
CommandService.cpp 的编译命令：
  g++.exe -DURI_STATIC_BUILD -D_WIN32_WINNT=0x0601 @CMakeFiles/CodeFormatServer.dir/includes_CXX.rsp -O3 ...
                                                  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
                                                  所有的 -I 都在这个文件里
```

`-I` / `-isystem` 是**从编译命令里读的**（`config.rs:539`/`548` 的 `-isystem`/`-I` 分支，`includes.rs` 的
"user paths before system paths" 也依赖它），而 CMake 生成的命令把全部 `-I` 放进了 `@…/includes_CXX.rsp` ——
**一个参数文件**。解析器不认识 `@`，于是这个 TU 的搜索路径里**一条 `-I` 都没有**。

后果是一条链，不是一句话：

```text
---

## ✅ A5. `auto x = std::format(…)` 推不出类型 / 推出来但没有补全 —— 2026-10-27 修好

**现象（编辑器截图，2026-10-27）**：`auto enenn = std::format(FULL_NAME_FORMAT, myName.getFullName(0));`
的 hover 只有"`enenn` — a variable, declared in a function body"，**没有类型**，因此 inlay hint 也没有。
同一张图里对 `std::format` 的 hover 一次列出十来个重载（这条已修：hover 现在最多打印 6 条 + 计数）。

**已经验证过的部分**（本仓库夹具，`std::format` 一个/两个重载、变参、返回 `string`）：

```text
auto s = std::format("%d", n);      →  s 的类型是 string      ✓
```

所以推不出来是**那个项目自己的声明**里的东西，最可能是这几种之一，按可能性排序：

1. `<format>` 没被索引到（没有 include，或 include 路径不对）——`std::format` 查不到，`type_at` 直接 Unknown；
2. 重载集里的**第一个**声明返回的不是 `string`（MSVC 的 `format` 有一族 `format_string<…>` 重载，
   以及 locale 版本），`definition_at` 取第一个的 `returns`；
3. 第一个实参是宏（`FULL_NAME_FORMAT`），展开后不是字符串字面量，`format_string<_Types…>` 的推导整条失败。

**2026-10-27 现场实测（一个两文件的 C++ 工程，`CPPLS_FILE=<其中的 main.cpp>`）—— 原因找到了，但它不是"漏读"：

```text
type_at("enenn")  →  Unknown(Ambiguous("std::format"))        ← 修前是 Unknown(UnknownType("std::format"))
std::format 在这个项目里能看见【4 个】真重载：
  format(const format_string<_Types...>, _Types&&...)      → string
  format(const wformat_string<_Types...>, _Types&&...)     → wstring
  format(const locale&, const format_string<…>, _Types&&…) → string
  format(const locale&, const wformat_string<…>, _Types&&…)→ wstring
另有 8 个同名的【类成员】被 files_declaring 带进来（std::formatter、std::_Formatter_base 等）
```

结论：**拒绝是正确的**——C++ 要靠"格式串是窄还是宽"选重载，而 hover 用的那个选择器
`Session::the_declaration_a_call_means` 在这个调用上答的也是 `None`（实测）。所以这不是索引缺东西，
而是**引擎还没有按实参选重载的能力**。

本轮做的（让它不再"什么都不说"）：

```text
① type_at 现在保留 Ambiguous 这个理由（此前被压平成 UnknownType，读起来像"名字不存在"）
   测试：两个头文件各声明 int f() / double f()，`auto x = f();` → Unknown(Ambiguous("f"))
② hover 的变量卡片在 auto 推不出类型时打印这句话（用分析自己的 describe()，不是第二份意见）：
   "The type is deduced from the initializer, and that could not be typed: `std::format` is declared more than once
    in what this file can see, and nothing here chooses between the declarations."
```

**要真正给出类型，下一步是"按实参选重载"**：现有材料已经够（实参类型、每个候选的 parameter_list、
template_parameters），缺的是把 `Session::the_declaration_a_call_means` 的那套排序搬进引擎的
`type_of_a_call`；对 `format` 这一族，判据是**第一个实参的字符宽度**（`char` vs `wchar_t`）对上
`format_string` / `wformat_string`，而这条判据需要"这个类能不能由这个实参构造"——也就是
`CppMember` 的构造函数参数表（索引里已经有了：`std::formatter` 那 8 条就是从这里读出来的）。

**2026-10-27 修好：验收标准达到。**

```text
auto enenn = std::format(FULL_NAME_FORMAT, myName.getFullName(0));
    →  type_of: basic_string<char, char_traits<char>, allocator<char>>      ✓（std::string）
auto c = std::format("Full name: {}", myName.getFullName(1));  →  同上         ✓
auto vec = std::vector<int>();  →  std::vector<int>   ✓
auto size = vec.size();         →  size_t             ✓
```

沿路修掉的**两个真 bug**（都不是"这个文件的问题"）：

```text
① parameters_in 把【任何含 "..." 的形参】当成参数包丢掉
   (const format_string<_Types...> _Fmt, _Types&&... _Args)  →  ([], true)      ← 整个列表空了
   (const locale&, const format_string<...>, _Types&&...)     →  只剩第一个
   后果：std::format 的候选一个形参都没有，排序无从比较，auto 拿不到类型
   修法：先跳过 <...> 再看 "..."
   测试：overload.rs::a_template_argument_pack_inside_a_parameter_is_not_the_parameter_pack

② 宽字符串字面量的类型是 const char*
   L"x {}" 也是 const char*  →  窄宽规则读错半边
   修法：按编码前缀给 const wchar_t* / char16_t* / char32_t*（测试覆盖 auto b = L"x" 等）
```

**引擎接上重载选择**（这是"缺的能力"）：

```text
・同文件多个声明：declaration_of_a_callee 先问 definitions_at，>1 条就走排序（原来取第一条）
・跨文件同名：index.definitions 的多条走同一个排序（原来直接 Ambiguous）
・排序用 sema::overload::choose（与签名弹窗、hover 的 → 标记同一套）
・平局时用【实参的字符宽度】选窄/宽族：仅当恰好一个候选的首个形参写了该宽度才作答，否则保持 Ambiguous
```

**下一步可能要抓的（如果还要再验别的现场）**：

```text
① 那个 main.cpp 的绝对路径（哪个 workspace / compile_commands.json）
② 文件里有没有 #include <format>，以及 .cpp_ls 的 include 路径配置从哪来
③ 在那边跑一次：
   CPPLS_PROJECT_ROOT=<workspace> CPPLS_FILE=<main.cpp> CPPLS_NEEDLE='enenn' CPPLS_TYPE=1 CPPLS_TRACE_INFERENCE=1
   cargo test --release -p cpp_code_analysis --test inference_probe -- --ignored --nocapture
```

`CPPLS_AT=last` 可把光标放到匹配文本的最后一个字符上（问"这个表达式"而不是"这个名字"）。

# 没有 -I
#include "LanguageServer.h"        ← 不在 CommandService.cpp 自己的目录里（它在 src/Service/，头在 src/）
  → include 边不存在            ← graph.rs 里没有这条边
  → LanguageServer 的 32 条成员事实"不可见"   ← 测出来的就是 0 visible
  → _owner->SendRequest 拒绝     ← 记分板里 20+ 条，全部是这个原因
```

证据（同一 scope，三个问题问出来的）：32 条事实在索引里；从 `LanguageServer.cpp` 看见 32 条，从 `main.cpp` 看见
21 条，从 `CommandService.cpp` 看见 **0** 条 —— 而它明明 `#include "LanguageServer.h"`。

**修在哪**：`crates/cpp_code_analysis/src/config.rs` 的参数解析（``@file` 要按 `directory` 展开成参数，再走现有的
`-I`/`-isystem` 分支），夹具用一份带 `@rsp` 的 compile_commands 条目断言搜索路径条数。
**验收**：`CommandService.cpp` 看见 32 条；记分板 76.0% → 预计 85%+（1242 个 `->` 问题里一大半是这类）。

**为什么这一条比模板推理重要**：它解释的是"为什么 `LuaReserved` 找不到引用/类型看着对却查不到成员"这一整类现象 ——
索引的 include 图缺边，后面所有层都只能摇头。先修它，再谈 §A4 的别名链。

### ✅ 已经修好（提交 `8928591`），并且量到了

两个原因叠在一起：

1. **摘要里的 `#include` 用的是项目级配置**，而旁边的 cooked 图已经用 `config.for_file(path)` —— 一个问题两个答案。
   改成三个摘要构建点都用本文件的编译配置。
2. **`for_file` 没有展开 `@响应文件`**（`for_file_with` 现在展开，`for_file` 给磁盘读取）。

冷 store 实测（103 文件，6013 个成员问题）：

```text
resolved        4571 (76.0%)  →  4637 (77.1%)
refused          1442         →  1376
其中"有接收者类型"   91         →  55
```

最后一行是重点：**36 个"类型已知、类里查不到成员"的拒绝，根本不是类型问题**，是文件看不见那个头。
可见性现在能到 `CodeActionService.cpp`、`LSPHandle.cpp` 等（各看见 21 条事实）。

**还剩一个**：`CommandService.cpp` 自己仍然是 0 条可见 —— 同一个目标、同一个 rsp，说明这条路上还有第二个原因
（它不是可见性规则能解释的，因为同目录的 `LSPHandle.cpp` 已经好了）。下一步就是它，然后是"成员链 34.2%"那一桶。

### 关于 `CommandService.cpp`：已经排除的与已经定位的

用 `CPPLS_CLASS=<类> CPPLS_FROM=<文件名>` 逐层排：

```text
CommandService.cpp 的 6 条 include 全部解析成功（含 LanguageServer.h → …/languageserver.h）
它的可见闭包有 1031 个文件，LanguageServer.h 在闭包里
索引里也有它的摘要（summary found）
可是 declarations_in("LanguageServer", 它) == 0
```

所以 include 边、可见集、摘要这三层都是通的，**丢在最后一步**：`visible_in_order_of` 把"可见集的文件名"映射成
序号（`sequence.get(&key)`），而 `sequence` 是按"摘要归档时的路径拼写"存的 —— 两个拼写不同就**静默丢弃**，
`filter_map` 连一个警告都没有。

**试过、撤回（两种做法，同一个结果）**：

1. 在查找处加 `normalize` 后备；
2. 在**写入时**就把两种拼写都插进 `sequence`（删除时两种都删）。

两次都在同一个冷 store 上量到同一个结果：**4637（77.1%）→ 4609（76.7%），+28 个 `NotDeclaredHere`，
`Ambiguous` 一个也没有**。也就是说：这些文件**确实**被丢了，但把它们放回来是**净损失 28 个答案**，不是净收益。

所以缺陷不在"丢"，而在"**放回来之后查询怎么选**"：`direct_member` 在可见的声明里取第一个，视野变宽就可能拿到
成员表更空的那个声明。**这条规则必须先修（或先单独度量），否则那个"修复"就是回归。** 两个实验都没有留在树里 ——
树里是现在能发的版本，注释里写清了这段。

**下一步（按顺序）**：

**记分板已经把"选错声明 / 没找到"分开了，结果是决定性的**：

```text
有接收者类型的 77 个拒绝：
   47  "no visible declaration of the class"      ← 类根本不可见
   30  "a list from no visible declaration"       ← 列表是从"零个可见声明"建出来的
    0  "a list that does not hold the member"     ← 一个都没有
```

**所以"在多个声明之间选错了"这个假设被自己的度量否掉了** —— 77 个全是可见性。第二个假设（同一文件两种拼写 →
两份摘要）也**已经被数掉**：

```text
1581 summar(ies), 1581 distinct file(s) by identity, 0 in the index twice
```

索引里没有重复。于是只剩唯一还没被排除的东西：**视野变宽改变了"这个名字有哪些声明"**，而 47/30/0 的分布已经
说明了形状 —— 那 28 个从"答得出"变成"拒绝"的问题，是"以前一个可见声明就够、现在两个不够"的那些。下一次要量的
就是**具体是哪两个**（把改变答案的那对声明打出来），而不是再猜一层。

两次实验都不在树里；树里是现在能发的版本。

### 下一次要做的那个测量（已经设计好，不靠猜）

问题只剩一个：**为什么"放回被丢弃的拼写"会让 28 个问题从"答得出"变成"拒绝"**。三个假设里两个已被数掉，剩下
这个必须用**逐题对比**来回答，而不是再看汇总：

1. 在记分板里加一个 `CPPLS_RECORD=<文件>`：把每个成员问题的 `(文件:行, 成员, 答案)` 写到一行，答案是
   `Yes(<声明所在文件>::<scope>::<name>)` / `Unknown(<理由>)`；
2. 用现在的代码跑一次 → `answers.a`；
3. 把 `visible_in_order_of` 的 normalize 后备**临时**加回去（一行），再跑一次 → `answers.b`；
4. `diff` 两个文件，只看那 28 行从 `Yes` 变 `Unknown` 的 —— 每一行都写明了它以前选中的**是哪个声明**，
   而新的一次连声明都没有了。那个差异就是答案。

**为什么必须是这样**：三次汇总数字（4637 → 4609、+28 `NotDeclaredHere`、0 `Ambiguous`）都已经到手，它们能排除
假设，但指不出因果；只有逐题 diff 能说出"这 28 个问题各自丢了哪个声明"。这个测量不需要新理论，只需要把
`direct_member` 的**候选**和**选中者**打出来。

**再往后**：成员链 34% 那一桶（也是可见性，量完这 77 个之后它应该跟着动）、嵌套别名在实参里的展开
（`_Tp_alloc_type` → `std::size_t&`）、`find()->second`。

**怎么读那 1351 个"没有接收者类型"**：接收者本身是 `auto x = f();` 这类**调用结果**，所以它需要调用路径的返回
类型（`what_a_call_has`）。这一桶是下一步最大的，但在上面那条岔路走通之前不碰。

---

## 怎么验证（每一条改完都跑这些）

```text
cargo test --release -p cpp_code_analysis      # lib + 40 多个集成目标
cargo test --release -p cpp_ls                 # 13 个目标（latency 必须 release）
cargo test --release -p cpp_parser -p cpp_formatter

夹具优先：能用 `crates/cpp_code_analysis/src/session/tests.rs` 里的 Memory 夹具复现的，就不要开真实项目探针。
         §A/§B 三条的夹具在 `crates/cpp_code_analysis/tests/reported_defects.rs`。

真实项目（分钟级，只吃环境变量）：
    CPPLS_REAL_PROJECT=E:\EmmyLuaCodeStyle \
      cargo test --release -p cpp_ls --test real_project -- --ignored --nocapture
        the_two_reported_hovers_answer_on_the_project_they_came_from   ← §A1/§A2 的两张卡片
        the_first_completion_on_a_real_project_says_where_its_time_went

    CPPLS_PROJECT_ROOT=<项目> CPPLS_FILE=<文件> CPPLS_NEEDLE=<文本> \
      cargo test --release -p cpp_code_analysis --test inference_probe -- --ignored --nocapture
    加 CPPLS_NAME=<名字>   打印该名字的全部事实（kind/scope/returns/type_of/parameters）
    加 CPPLS_DIAGNOSTICS=1  打印这个文件的全部检查结果
    加 CPPLS_TRACE_INFERENCE=1  打印成员答案的代换过程（形参/实参/结果）

追踪开关：CPPLS_TRACE_COOKING / CPPLS_TRACE_MEMBERS / CPPLS_TRACE_ANSWERS / CPPLS_TRACE_TIME …
测量开关：CPPLS_NO_GUARD_FILTER、CPPLS_NO_INFERENCE、CPPLS_NO_INDEX_LOOKUP
```
**2026-10-27 第七批：`find(text)->second` 在真实项目上通了。** 根因不在别名走查，而在**容器元素规则**：

```text
range_element_of(map) 要求 arguments.len() == 2
  但 LuaReserved 的类型是 std::map<std::string, LuaTokenKind, std::less<>> ——【三个】实参
  → "没有元素"，iterator_element_type 放弃，调用退回声明里的 `_Mybase::iterator`（一个依赖类型，谁都打不开）
修法：map 系列 >= 2、一参容器 >= 1（多出来的是比较器/分配器，不改变元素类型）
```

实测（EmmyLuaCodeStyle，`CPPLS_TYPE=1`）：

```text
LuaReserved.find(text)->second   →  LuaTokenKind            ✓（修前：Unknown(NotDeclaredHere("std::conditional<_Test, _Ty1, _Ty2>::type::second"))）
LuaReserved.find(text)->          →  class "std::pair": 21 member(s) ["first", "first_type", …]   ✓
```

夹具：`a_maps_find_reaches_the_element_when_the_comparator_is_written`（三实参的 map + `pair` + `->second`）。

**还剩的残差**（诚实记录）：`LuaReserved.find`（**名字**而不是调用）的类型仍是
`conditional<_Is_set, _Tree_const_iterator<_Scary_val>, _Tree_iterator<_Scary_val>>::type` —— 这是一个**依赖类型**：
`_Is_set` 是 `_Tree` 的形参，要解开它得实例化 `_Tree`，这不是"跟随拼写"这一层能做的事。眼下它作为答案原样给出（比错的类型好），
而读者真正会用的 `->` / `->second` 已经能答。

同一批还落地了两处支撑：
① `a_member_the_class_declares` 恢复 `direct_member` 兜底（特化声明、`members_of` 走不到的成员）；
② `finish_a_nested_name` 统一把 **class 连同实参**（`bindings.arguments()`）交给限定名解析，
   于是 `_Mybase = _Tree<_Tmap_traits<std::string, LuaTokenKind, …>>` 里的实参真的代换了（追踪可见）。
**2026-10-27 第八批：A4 的验收标准两条都达到了。** 实测（EmmyLuaCodeStyle + MSVC，`tests/inference_probe.rs`）：

```text
LuaReserved.find(text)->second   →  LuaTokenKind                        ✓
_lineOffsetVec[line]             →  std::size_t&                        ✓（修前停在 _Tp_alloc_type::value_type&）
LuaReserved.find(text)->         →  class "std::pair": 21 member(s)     ✓
```

这一条线一路修掉的东西（按发现顺序，全部有夹具）：

```text
substituted_spelling 拒绝"少写实参"        → 按 C++ 顺序填，缺的留原样
形参取自偏特化                             → template_parameters_for 按 pattern/arity 选声明
["class","class"] 这种形参表               → is_a_parameter_name 挡住（根因仍在事实层，未修）
成员别名指出的基类                          → an_element_type_through_the_members
trait 形态的 element_type                  → 退到"类模板自己的实参"
事实来自缓冲区时不带 parameters/pattern     → fact_from_binding_with 补齐（不变式：两条路说同一件事）
特化的形参—实参配对                         → the_pairing_its_pattern_implies（按 pattern 槽位）
限定名在 finish_a_nested_name 里走不动       → 增加"限定名先过别名走查"
调用点拿不到对象实参                        → finish_a_nested_name 用 bindings.arguments() 补上
map 元素规则要求恰好两个实参                → >= 2（比较器/分配器不影响元素）
```

**剩下的唯一残差**（不再是"找不到"，而是"依赖类型"）：`LuaReserved.find`（**名字**，不是调用也不是箭头）的答案是
`conditional<_Is_set, _Tree_const_iterator<_Scary_val>, _Tree_iterator<_Scary_val>>::type`。`_Is_set` 是 `_Tree` 的形参，
解开它需要实例化 `_Tree`。这是**答案的种类问题**，不是解析链条断了；如果想改善读者体验，可以把它作为一种**显式的答案**
（例如 hover 显示 `typename …::type（依赖类型，需实例化才能确定）`），列在 E 里。

**探针新增开关**：`CPPLS_AT=last` 把光标放在匹配文本的**最后一个字符**上——问"表达式的类型"
（`_lineOffsetVec[line]`）和问"名字的类型"（`_lineOffsetVec`）是两件事，上面的 `std::size_t&` 就是用它量出来的。
**2026-10-27 第九批：两条已知红变绿，一处事实层核实。**

```text
① tests/checks.rs 的 a_named_parameter_is_checked_too —— 去掉 #[ignore]，68 条 checks 全绿
   根因：parameter_spellings_of 返回【写出来的】`int width`，而比较要的是类型 `int`
   修法：读出来先过 sema::overload::a_type_without_its_name（analysis 其它地方早就在用同一个 reader）
   普查：LuaSource.cpp / LuaLexer.cpp 两个真实文件各 0 条发现（这次放开没有制造新的假阳性）
② tests/types.rs 的 a_trait_a_builtin_decides_selects_the_right_specialization —— 第七批起已绿（41 条 types 全绿）
③ B1 核实：`std::vector` 的事实现在是 ["_Ty", "_Alloc"] / ["_Alloc"]（token 读取器那次改动把
   ["class", "class"] 修掉了）；`is_a_parameter_name` 保留为不变式守卫，不再是唯一防线
```

现在整份文档里剩下的 `#[ignore]` 只有三处：`completion.rs:1423`（一条测量）、`nested_conditional.rs`、`macros.rs:37`。
**2026-10-27 续：类型有了，补全没有 —— 渲染器写出来的拼写带空格和绝对限定。**

```text
enenn.size()  →  class "basic_string": 0 member(s)
members_of_inner: :: std :: basic_string 什么都没有声明
```

`std::string` 是 `using string = basic_string<…>;`，而从**渲染**里读出来的目标拼写是 `:: std :: basic_string<…>`
——预处理器写宏展开时每个 token 之间留空格、并且用绝对限定。所有下游查找都以拼写为键，于是：

```text
resolve_aliases_or_unresolved 产出 ":: std :: basic_string"
is_declared(":: std :: basic_string") → false（索引里叫 std::basic_string）
→ 链放弃 → members_of 答 0 个成员 → enenn. 后面什么都没有
```

修法两处，都在 `resolve_aliases_or_unresolved`：

```text
① 读进来的类名去掉所有空白（a_class_name_without_spaces）
② 被接受的候选去掉开头的 "::"（索引不按全局标记存名字）
```

实测（`test_cpp` 的副本加一行 `enenn.size();`）：

```text
class "basic_string": 170 member(s) ["allocator_type", "append", …]      ✓
```

测试：`session/tests.rs::a_member_list_is_reached_through_an_alias_written_absolutely`
（`using string = ::std::basic_string<char>;` 这个形状，就是渲染器写出来的那个）。
**2026-10-27 第十批：宏来自 include 时的着色与 hover —— 一半是代码，一半是配置。**

`EmmyLuaCodeStyle\Test\src` 是一个**独立的 CMake 子工程**（自己的 `CMakeLists.txt`，
`target_include_directories` 里有 `${LuaCodeStyle_SOURCE_DIR}/3rd/googletest-1.13.0/googletest/include`），
而这个仓库**没有任何 `compile_commands.json`**。于是 `<gtest/gtest.h>` 解析不到 → 索引里没有 `TEST` 宏 →
hover 空、着色也不对。实测证据：

```text
（无 .cppls.toml）macro:  索引里根本没有 TEST
（加 .cppls.toml）macro:  Unknown(ConditionalCompilation)   ← 找到了，但 #define 在 #if !GTEST_DONT_DEFINE_TEST 里
```

可用的配置（放在工作区根，实测能让 include 解析、索引到 `TEST` 宏）：

```toml
[compile]
extra_args = ["-I3rd/googletest-1.13.0/googletest/include", "-Iinclude"]
```

本轮为此改的两处：

```text
① semantic 的宏分类现在也接受 ConditionalCompilation —— #if 里的 #define 对读者仍然是宏，
   不确定的是"它在这里存不存在"，不是"这个名字是什么"（这个不确定性由 hover 用文字说明）
② hover 对 ConditionalCompilation 的宏给出卡片（原来什么都不显示）：
   "`TEST` — a macro whose #define is inside a conditional block, so whether it exists here depends on the
    compilation. The analysis is reporting what the header says, not what a compiler concluded."
```

**要彻底变成"确定的宏"**，需要一份编译数据库（`cmake -DCMAKE_EXPORT_COMPILE_COMMANDS=ON`）：有了它，
整个程序会被 cook，`#define` 在渲染里就是无条件事实，`ConditionalCompilation` 自然消失。

探针新增 `CPPLS_MACRO=1`：在光标处打印 `macro_definition` 的答案（宏不是声明，`definitions` 问不到它）。
