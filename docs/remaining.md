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

### A4. 依赖形式的限定别名没有跟随（`LuaReserved.find(text)->second`、`_Tp_alloc_type::value_type&`）

**现象**：`LuaReserved.find(text)->second` 报 "`std::map<…>` has no member second"；`lineStartOffset` 的类型停在
`_Tp_alloc_type::value_type&`；`LuaReserved` 找不到引用。

**已经做的（`25e5733` 之后的那次改动）**：`a_spelling_through_the_aliases` —— 把一个拼写里**类自己声明的别名**一路跟下去：

```text
pointer                     → _Val*                 （无限定：问这个类）
_Alloc_traits::reference    → 先解析限定符，再问它     （限定：先跟限定符）
```

配套：`split_at_the_last_scope`（角度括号之外的最后一个 `::`）、`split_the_operators_of` 丢 `typename`/`class`/
`struct`/`union`/`enum`（此前查找被问成 `typename _Alloc_traits`，什么也找不到，然后按**裸名**猜了一个类 —— 实测跟进了
`std::basic_string` 的 `_Alloc_traits`）、以及"限定符没解析出来就停"的守卫（不再按裸名猜）。

**现在停在哪（实测，`CPPLS_TRACE_INFERENCE=1` 每一跳都有日志）**：

```text
vector<size_t>::reference        → typename _Alloc_traits::reference
_Alloc_traits                   → __gnu_cxx::__alloc_traits<_Tp_alloc_type>
   …（每一跳都正确）…
最终                             → _Tp_alloc_type::value_type&
```

残差全是**没有实参的模板形参**（`_Alloc` 用了默认实参 `std::allocator<size_t>`，而**默认实参不在事实里**）。这和
C2 里 `a_defaulted_template_parameter_of_a_variable_template_substitutes`（`twice<float>` 答 `U`）是**同一个缺口**，
也是 `map::find(…)->second` 停下的地方：箭头那一侧还要先按同一套把 `operator->` 的 `pointer` 跟成 `_Val*`。

**下一步（按顺序）**：

1. 事实里记下模板参数的**默认实参**（`declared_template_parameters_with` 旁边的表，`template_parameters_in` 已经在读
   参数节点，只需要把 `= …` 那一段也留下），并把 `substituted_spelling` 的配对改成"缺实参用默认值"。
2. 箭头路径（`through_an_operator_arrow`）里对 `operator->` 的返回拼写先跑一次
   `a_spelling_through_the_aliases`，再取 pointee —— 这是 `find()->second` 的最后一块。

**怎么验收**：`inference_probe` 在 `LuaSource.cpp` 上 `type_at` 给出 `std::size_t&`；`map<…>::find` 的 `->second`
答到 `pair<const K, V>::second`。


### 其余

### C2. 已知红（`#[ignore]`，语义缺口，不是回归）

```text
tests/checks.rs:1685   a_named_parameter_is_checked_too
                       `int` 实参对 `int width` 形参，两侧都解析不出来（见 docs/generics.md）
tests/types.rs:853     a_defaulted_template_parameter_of_a_variable_template_substitutes
                       模板参数的默认值不在事实里，`twice<float>` 答 `U`
tests/types.rs:922     a_trait_a_builtin_decides_selects_the_right_specialization
                       声明自身类型上的别名跟随（`enable_if_t<A, B>` 的 `type_of` 是拼写，不是别名目标）
tests/completion.rs:1423  a_constructors_parameters_are_in_scope
                       一条测量，不是断言
tests/nested_conditional.rs  生效条件的判断（文件头注释解释了为什么现在会失败）
tests/macros.rs:37    宏查询的一个已知缺口（文件里有 "# Why this is #[ignore]d"）
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
