# 现状与计划

> 这份文档取代了 `docs/` 下此前的全部文档（19 份，约 270 KB，已删除）。那些文档记录的是**过程**——
> 每一轮排查的中间状态——而不是**当前的事实**，其中大部分已经与代码不符。这里只写现在成立的事，
> 以及下一步要做什么；代码里指向旧文档的引用已全部改指到本文。

## 一、这是什么

一个用 Rust 写的 C++ 语言服务器。它读取真实工程的方式**先经过预处理**：宏被展开、条件编译按实际
生效的分支解析、`#include` 被内联，得到的"渲染"才是语法分析的输入——因为 C++ 的文本在预处理器
跑完之前并没有确定含义。

## 二、规模与状态

| 部分 | 内容 |
|---|---|
| `cpp_parser` | 词法 + 语法，产出无损语法树（rowan），并带一层**强类型 AST**（`CppTranslationUnit`、`CppIfStat`、`CppClassDef`、`CppParamList`…） |
| `cpp_code_analysis` | 预处理与渲染、索引（每个文件的声明事实）、语义模型（类型推导、成员、作用域）、13 项诊断检查 |
| `cpp_ls` | LSP 服务端：诊断、补全、签名帮助、跳转、引用、语义高亮、折叠、内联提示 |

**这是一个正在开发中的项目**，没有发布版本，接口与内部结构都可能随时改动。

## 三、现在确实能工作的部分（都有测试或可复现命令）

```
cargo build --release --workspace --all-targets     零错误、零警告
cargo test --release --workspace                    全绿
```

- **解析与渲染**：整个 MSVC 标准库闭包（187 个文件）**零语法错误**。
- **13 项诊断**，各有 `DiagnosticCode`（可直接映射到 LSP 的 `code` 字段）、severity 与 tag：

  ```
  syntax-error  include-not-found  macro-redefined  unused  access-invisible
  missing-return  assignment-in-condition  unnecessary-branch  undefined-field
  argument-count  param-type-check  assign-type-check  return-type-mismatch
  ```

- **标准库上的零误报**——这一层的文档把这条测量点名为唯一能约束它的测量，它现在是绿的：

  ```
  cargo run --release -p cpp_code_analysis --example checks_probe -- target/std-cook target/std-cook/assign-case.cpp
    assign-case.cpp:52  [assign-type-check]  `x` is initialised with a value that does not convert to `std::string`
    --- checks over 5 file(s) ---   1  assign-type-check
  ```

  **恰好一条真阳性，其余全零**。`target/std-cook/` 下有复现用的夹具（`assign-case.cpp`、`arity-case.cpp`、
  `broken-case.cpp`）。

- **诊断的判据可以来自整个项目**：报告只针对本文件写的内容，但结论可以读**索引里的事实**——
  例如 `std::string x = 123;` 之所以能报出来，是因为它读的是索引里 `std::string` 构造函数的形参表
  （`DeclFact::parameter_list`），而不是去解析头文件。

## 四、已知的缺陷与未完成项（都是量出来的，不是猜的）

```text
① argument-count 的**跨文件**那一半不解析
   本文件内声明的被调用者已经能判（scale(1, 2, 3) 报出）；
   但 v.reserve(1, 2)（reserve 声明在 <vector> 里）在真实语料上仍是 0 条。
   下一步：查 ProjectIndex::definitions(name, path) 在那里到底答了什么。

② param-type-check 还未接上同一套索引事实
   它目前只看本文件内声明的函数，跨文件实参一律不判——与 ① 同构，修法相同。

③ missing-return / unnecessary-branch 等检查在真实语料上尚无**正例**验证
   负例（不该报的）全过，正例（该报的）还没有在标准库规模上验证过。

④ 测试夹具里的 #include 不解析
   因此任何想借夹具模拟"类/函数来自头文件"的测试都必须先证明包含确实生效，
   否则测的是空气。tests/checks.rs 里有一条守卫测试专门守着这件事。
```

## 五、写这一层时反复踩到的两类坑（值得继续防）

**一、把"我对语法形状的猜测"当成事实。** 同一个缺陷在这一层出现过四次：文件作用域的函数定义不是
`FunctionDef`；`struct` 不是 `ClassDef`；参数表是**孙节点**而不是子节点；简单调用的实参**没有**
`ArgumentList` 节点。每一次都是"编译通过、看着正确、什么都不报"。

> 现在的规则：**判断用强类型 AST**（`cast` / `children::<N>()` / `descendants::<N>()`），
> 而不是手写 `CppSyntaxKind` 比较；文本只用于消息。

**二、拿夹具或探针的产物当产品的行为。** 两次探针（`one_check`、`checks_probe`）都曾静默地什么都不检查，
却打印出"零 findings"这个**最好**的数字；一次夹具的 `#include` 从未解析，却被当成"头文件读过了"。

> 现在的规则：**先说清这次测量在量什么，并给它一个控制组**（例如一个必然报错的输入）。

## 六、下一步

```text
近期   ① argument-count 的跨文件查找 ② param-type-check 接上索引事实
       ③ 给现有 13 项检查逐条补"真实语料上的正例"验证
       ④ 把 checks_probe 变成 CI 里的一键回归（标准库零误报）

中期   补更多 C++ 应有的诊断（访问控制已有 access-invisible，可继续：重复 case 标签、
       返回类型不匹配的更多形态、const 重赋值、自赋值等）
       性能：check 层已经统一走"只遍历本文件自己的节点"，把这条推广到其余通道

长期   让索引与渲染的缓存跨会话复用；把"报告只针对本文件、判据来自整个项目"这条边界
       推广到补全、跳转与引用
```

## 七、怎么自己验证

```bash
# 全部测试
cargo test --release --workspace

# 标准库零误报（需要先用真实工具链 cook 出一份语料，target/std-cook 是现成的）
cargo run --release -p cpp_code_analysis --example checks_probe -- target/std-cook target/std-cook/assign-case.cpp

# 逐项检查、逐条发现（带它依据的声明事实）
cargo run --release -p cpp_code_analysis --example one_check -- target/std-cook target/std-cook/assign-case.cpp unused

# 一个 token 的真实 kind（这一层的 API 之一，曾被我误判两次）
cargo run --release -p cpp_parser --example token_kinds
```
