# 还要修的东西

这份文档是给**后面接着做的人**看的：每一条都是这一轮（正确性测试）里实测定位过、但还没修的问题。
不是愿望清单 —— 每一条都写了现象、根因、改哪里、怎么验收，按这个顺序做即可。

先读 `docs/architecture.md` §9：那里记了已经修掉的九个真缺陷（`#pragma` 被当条件指令、无名枚举、枚举数值、
`assign-type-check` 的裸名表、`substituted_spelling` 拒绝少写实参、偏特化抢形参、`["class","class"]`、成员别名基类、
trait 形态的 `element_type`、类外定义的限定名），以及为什么"读整个项目"被删掉。

---

## A. hover 两条（用户可见，优先级最高）

### A1. 类模板的实例化拼写没有显示出来

**现象**

```text
光标在 `std::map<std::string, LuaTokenKind, std::less<>> LuaLexer::LuaReserved = { … };` 的 `map` 上，卡片是

    class map —— a type template over `_Kty`, `_Ty`, `_Pr`, `_Alloc`
                  derived from `_Tree<_Tmap_traits<_Kty, _Ty, _Pr, _Alloc, false>>`, in `std`

读者写的是 `std::map<std::string, LuaTokenKind, std::less<>>`，卡片却只有模板的形参名。
```

**根因**：`declaration_markdown` 用**事实**重建语句（`signature_line` / `kind_words`），而事实记的是*声明*的那一面
（`pattern` = `map<_Kty, _Ty, _Pr, _Alloc>`），不是*使用*处写出来的拼写。

**改哪里**

* `crates/cpp_ls/src/handlers/hover/mod.rs`
  * `declaration_markdown`（约 680 行）：`DeclKind::Type` 且 `fact.parameters` 非空时，用**光标处的 token 文本**做签名行
    （handler 里有 `view` 和偏移；`cpp_parser::token_at(&view.root, offset)` 就能拿到 `std::map<std::string, …>`）；
  * 卡片第二行（`` `map` — a type template over … ``）在那种情况下也应改成"`std::map<std::string, LuaTokenKind, std::less<>>` 的声明"。

**验收**

```text
夹具：`std::map<std::string, int> m;` 里 hover `map`
期望：卡片里出现 `std::map<std::string, int>`（写出来的拼写），且仍然给出 `Declared in …:行`
真实项目：EmmyLuaCodeStyle `LuaLexer.cpp` 的 `LuaReserved` 那一行
```

### A2. 匿名 struct 的变量被显示成 `void`

**现象**

```text
    static const struct
    {
        unsigned char left;
        unsigned char right;
    } priority[] = { … };

hover `priority` → `void[] priority` —— a variable
```

**根因**：`crates/cpp_code_analysis/src/sema/types.rs` 的 `read_specifiers_named` 里，`ClassDef | StructDef | UnionDef |
EnumDef | EnumClassDef` 这几个分支是**空**（"类体本身不是它的名字"），而这类说明符序列里没有别的名字 —— 于是函数结尾回落到
`Type::builtin("void")`，事实的 `type_of` 成了 `Some("void")`，hover 认为"它能拼出签名"，就印了 `void[]`。

**改哪里**

1. `read_specifiers_named`：说明符里出现**未命名的**类体时，不要回落成 `void` —— 要么返回一个空拼写（让 `type_of` 为
   `None`），要么返回类体自己的文本（`struct { unsigned char left; … }`）。
2. `crates/cpp_ls/src/handlers/hover/mod.rs` 的 `spells_a_signature`（约 582 行）：`type_of` 为空时它会走
   `declaration_text`（文件的原文）—— 那正是这里想要的输出。

**验收**

```text
夹具：上面那三行 + `hover priority`
期望：卡片里的签名包含 `unsigned char`（原文），而不是 `void`
```

---

## B. 一个事实层的真缺陷（现在只在用的时候挡着）

### B1. 有些类模板事实的 `parameters` 是关键字

**现象**：MSVC 的 `<vector>` 里，某一份 `std::vector` 声明的 `parameters` 是 `vec!["class", "class"]`（关键字，不是名字）。

**现在的处理**：`crates/cpp_code_analysis/src/index/project.rs` 的 `is_a_parameter_name` 在**选择形参**时把这种表拒掉。
这是挡住症状，不是修因。

**改哪里**：`crates/cpp_code_analysis/src/sema/declarations.rs` 的 `declared_template_parameters_with`（约 2069 行）——
读 `template <…>` 头时跳过 `class`/`typename`/`struct`/`union`/`enum`/`concept` 这些引入词，只取随后的标识符。

**验收**

```text
夹具：`template <class A, typename B, int N> struct S {};`  → parameters 必须是 ["A", "B", "N"]
回归：`an_arrow_through_a_base_named_by_a_member_alias_in_another_file` 与 `a_class_templates_members_…` 仍绿
```

---

## C. 探针与测试的可用性（不影响产品，影响下一轮的速度）

### C1. `members_real.rs` / `signature_real.rs` 只对一台机器上的那个项目成立

它们把 `E:\EmmyLuaCodeStyle` 和 libstdc++ 的 `<vector>` 路径写在默认值里，在这台机器（MSVC + 另一个路径）上是红的。
`tests/inference_probe.rs` 是后来的写法：**只吃环境变量**（`CPPLS_PROJECT_ROOT`、`CPPLS_FILE`、`CPPLS_NEEDLE`、
`CPPLS_NAME`、`CPPLS_DIAGNOSTICS`、`CPPLS_TRACE_INFERENCE`）。

**改法**：把这两个探针改成同一种写法（`CPPLS_REAL_PROJECT`、`CPPLS_STL_HEADER`），并在找不到时**跳过而不是失败**。

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

---

## D. 不确定性（`docs/architecture.md` §7.12）

同一个项目跑两遍，`xfilesystem_abi.h` / `xmemory` 的**摘要事实数**会差 120–1 000 条（`43 483` vs `42 395`/`42 517`/
`42 526`/`43 238`/`43 340`/`43 363`），而**答案**是稳定的（36/36 个名字一致）。

**根因方向**：第二遍（每个文件按自己的闭包重读）对某些文件的覆盖取决于时序。**验收方式**：跑两遍 `patch_probe`，
比较"多少文件有渲染"与摘要总数；修法是让第一遍就渲染（§7.6），或者把"为什么这个文件没被重读"记进日志。

---

## E. 可选的性能工作（不是缺陷，是"以后可以更好"）

1. **打开文件的 TU 仍然每次启动重读一次**（走闭包 + 渲染 + 解析，热态约 0.4 s、冷态约 2.8 s）。clangd 的对应物是
   **preamble 缓存**：把渲染结果按（文件集合 + 宏环境）持久化，第二次启动直接命中。位置：
   `crates/cpp_code_analysis/src/index/store.rs` 的 `a_timeline_of_its_own`（2112 行）与 `prepare_the_re_read`（1444 行）。
2. **trait 形态的 `element_type`**：`remove_extent_t<_Ty>` 现在靠"退回到类模板自己的实参"绕过去
   （`the_class_an_arrow_points_at` 的最后一段）。真正的做法是跟随 `remove_extent_t<T>` → `T`（`remove_extent` 是标准库
   的 trait，索引里能查到它的定义）。

---

## 怎么验证（每一条改完都跑这些）

```text
cargo test -p cpp_code_analysis                      # 696 lib + 42 个集成目标
cargo test --release -p cpp_ls                       # 13 个目标（latency 必须 release）

夹具优先：能用 `crates/cpp_code_analysis/src/session/tests.rs` 里的 Memory 夹具复现的，就不要开真实项目探针
         （5 秒 vs 95 秒）。这一轮的 `shared_ptr` 箭头就是这样修出来的。

真实项目探针（分钟级，只吃环境变量）：
    CPPLS_PROJECT_ROOT=<项目> CPPLS_FILE=<文件> CPPLS_NEEDLE=<文本> \
      cargo test --release -p cpp_code_analysis --test inference_probe -- --ignored --nocapture
    加 CPPLS_NAME=<名字>   打印该名字的全部事实（kind/scope/returns/type_of/parameters）
    加 CPPLS_DIAGNOSTICS=1  打印这个文件的全部检查结果
    加 CPPLS_TRACE_INFERENCE=1  打印成员答案的代换过程（形参/实参/结果）

追踪开关：CPPLS_TRACE_COOKING / CPPLS_TRACE_MEMBERS / CPPLS_TRACE_ANSWERS / CPPLS_TRACE_TIME …
测量开关：CPPLS_NO_GUARD_FILTER（补丁 2 的 A/B）、CPPLS_NO_INFERENCE、CPPLS_NO_INDEX_LOOKUP
```
