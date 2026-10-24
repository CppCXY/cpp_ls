# 语义正确性：已知问题与下一步（2026-10-24）

用户报的三个问题，按"诊断闪现 → 别名 → 定义查找"的顺序处理。这里记的是**还没解决的**，和为什么。

## ① 诊断闪现：已修（`c2c0357` 之后的 `sticky report` 规则）

**机制**：pull 的诊断在文件"已编辑但还没 cook"时返回 `None`，handler 把它变成**空报告**——
而空报告对客户端意味着"这个文件干净"，于是它把波浪线擦掉；pump 随后 cook 完，`workspace/diagnostic/refresh`
让客户端再问一次，波浪线又出现。**每次 keystroke 重复一次**，就是"闪现"。

修法：`AnalysisState::last_reports` 记住每个文件上一次真正给出的报告；**没有答案的 pull 重复上一次的报告**，
只有真正的答案（包括空答案）才替换它。规则抽成 `the_report()`，三个分支有单测
（`no_answer_repeats_the_last_report_instead_of_claiming_the_file_is_clean`）。

为什么这不是"错误地使用 push"：这个服务**只**有 pull（`grep publishDiagnostics` 只剩注释），
也没有乱发 refresh——问题在**答案本身在交替**，不在投递方式。

## ② `using` 别名与 `auto`：修了一半，量出来的结果

`CodeFormatCLib/src/CodeFormat.cpp`（libstdc++ 闭包）修前 **10 条 `undefined-field` 假报**，修后 **1 条**。

### 已修 a：`auto` 的每一种写法都不该参与"转换"判断

`auto& config = configs.emplace_back(...)` 被报 "does not convert to `auto&`"：检查只跳过了字面上的
`auto` / `decltype(auto)`，没跳过 `auto&`、`const auto&`、`auto&&`、`auto*`。
`assign_type_check::is_a_deduced_auto` 现在剥掉 `const`/`volatile`/`&`/`*` 再判断。
（**注意**：把用户那张图钉成 fixture 时发现，纯 fixture 里 `Widget& → auto&` 回答的是 `Unknown` 而不是 `No`，
所以这个假报的触发还需要真实类型；结论按代码读，按项目文件验证。）

### 已修 b：别名指向"没有人声明过的类"时，成员表是**缺口**，不是空表

`std::string` → `typedef basic_string<char> string` 在 libstdc++ 的 `inline namespace __cxx11` 里，
解析落到 `std::basic_string`（只有前向声明），`direct_members` 找不到任何成员 → 返回
**空但完整**的列表（`unlisted: []`）→ `undefined-field` 把 `result.size()`、`formattedText.begin()`
等全部报成"没有这个成员"。

现在两处：
- `resolve_aliases_or_unresolved` 第三个返回值 = "链的终点是没人声明的拼写"；
- 别名解析后的类**一个成员都没有**时，列表带上 `unlisted`（"答案到此为止"），`undefined-field` 因此闭嘴。

### 未修：`std::vector::erase` 不在事实里

剩下的那一条 `_configs has no member erase` 不是检查的问题，是**索引里没有这个声明**：

```text
[undefined-field] `std::vector` -> `erase` (303 member(s), 0 unlisted) — index: 121 posting(s) of `erase`,
                  278 in scope `std::vector`
[member] 第一次的成员表: _M_allocate, _M_erase, ... —— 没有裸 `erase`
```

`std::vector` 在 `bits/stl_vector.h` 里，`erase` 在 `#if __cplusplus >= 201103L` 的区域里。CDB 写的是
`-std=gnu++20`，`__cplusplus` 本应是 `202002L`（`cplusplus_value` 处理了 `gnu++` 前缀）。下一步：
用 `CPPLS_TRACE_MACRO=1` 打印 `stl_vector.h` 的宏环境，看 `__cplusplus` 有没有进去；如果没有，
追 `CompilerConfig::standard` 是否从 `-std=gnu++20` 解析出来了。**判据**：`members_of(std::vector)`
里出现裸 `erase`，且那一条假报消失。

## ③ 定义/引用仍然"按名字"查找（用户图 1）

`_diagnosticStyle` 在 `LuaCodeFormat::UpdateDiagnosticStyle` 里，Go to Definition 列出 **3 个**：
本类的、`FormatState.h` 里另一个类的、`luacodeformat.h` 里第三个类的。也就是查询**是**按名字扫可见集合的，
没有用"光标所在的类"。

要做的事：

```text
1. `definition` 的入口先看光标处是不是**成员**（在某个类的成员函数体里、名字是该类的成员）：
   是 → 只在该类的作用域里找（`definitions_in_scope` 已有这个规则的一半），不要返回别人的同名成员。
2. 引用（references）同一个规则：一个类的成员 `_diagnosticStyle` 与另一个类的同名成员是两个实体。
3. 现有 `definitions_in_scope` 的注释说"裸名字不进类"，但这里光标处是**类内**，规则相反：
   类内的裸名字**先**进本类。缺的正是这一步。
```

判据：`LuaParser`/`CodeFormatCLib` 里给 `_diagnosticStyle`、`_session`、`_configs` 这类成员做
Go to Definition，每条只给一个答案（本类声明处）；`references` 不再跨类。
## ④ operator 推断：实现过、测过、**没有留下**（2026-10-24）

现状：`DeclFact::name` 对 operator 是**空**的——事实层给它建了位置，没建名字。于是
`members_of(Widget)` 里 `operator[]` 是一个无名成员，`operator_returns_of`（所有 `v[i]`、`-v`、`a + b`
都走它）永远找不到，`v[0]` 在"文件里声明的类"上回答 `Unknown(UnknownType("v[0]"))`
（`tests/types.rs::a_subscript_has_the_operators_return_type` 与
`a_classs_own_operators_have_their_declared_return_type` 两个 `#[ignore]` 就是这条）。

### 做过的改动（全部通过测试，然后因为性能回滚）

```text
1. `fact_from_binding_with` 的 name 从 `identifier_text().unwrap_or_default()` 改成 `Name::text()`，
   与索引层（`fact_for`）一致 → members_of 里出现 `operator[]`、`operator()`、`operator+`、`operator-`、
   `~Widget`（不再是空名字）。`a_destructor_hides_nothing_in_a_base...` 的断言随之更新。
2. `declared_returns_of_with` 允许 `OperatorFunction` 读返回类型。
3. 引擎：给 `-`、`+`、`!`、`~` 加一元 arm（原本没有，`-a` 一律 Unknown）；
   下标/一元/二元三个 arm 都改走 `operator_returns_memoed`（每请求按 `(class, operator)` 记忆）。
4. `substituted_members` 的别名展开从"只对模板实参"放宽到"所有类"（`cents operator+` 这类需要），
   并在 operator 的 `returns` 上做安全网：仍然是"类自己作用域里的名字"（如 `std::vector` 的
   `reference`）就置 None，避免那 60 条假报重新出现。
```

验证：两个 `#[ignore]` 测试全部转绿（`v[0]`→`int`、`a(3)`→`int`、`-a`→`Money`、`a+a`→`Money`、`*a`→`int`），
全仓测试绿。

### 为什么回滚：LuaSyntaxTree.cpp 上诊断 207 → 510 ms

```text
[check] ImplicitBoolConversion    12 → 184 ms
[check] UndefinedField            93 → 220 ms
诊断（keystroke 2）              207 → 455–515 ms，findings 8 → 9
```

原因不是 46 次 `std::vector<…>::operator[]` 查询（试过按请求记忆，没变）。最可能是**答案变多了**：
成员表里出现了 `operator bool` / `operator[]`，`implicit-bool-conversion` 与 `undefined-field`
于是第一次真的往下走，而它们**每个表达式**都要再问一次类型。也就是说这不是 operator 的错，
是这两个检查的"每个表达式一层"的成本，在答案存在之后才暴露出来。

### 下一刀的顺序（判据都写好了）

```text
1. 先只落"事实层的名字"（第 1 条），`returns` 不动：operator 出现在成员表里（补全/hover 立刻受益），
   引擎行为不变。判据：全仓测试绿、LuaSyntaxTree.cpp 的 207 ms/8 findings 不变。
2. 再量 `implicit-bool-conversion` / `undefined-field` 在"每个表达式问一次"上的成本，
   `[question]` 探针 + `CPPLS_TIME_CHECKS=1` 已有；把这一步变便宜（类型记忆、或按条件缓存），
   然后把 `returns` 与一元 arm 一起打开。
3. 最后才是 `substituted_members` 的放宽——它独立可测。
```