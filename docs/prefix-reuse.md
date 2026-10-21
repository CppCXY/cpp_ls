# 解析前缀复用：可行性、实测结论与后续工作

**结论先行。机制成立，但按"复用事件流"实现是负收益。要继续，必须复用 green tree，而不是事件流。**

```text
1. 现状：每次编辑只 cook 文件自己（17 ms），前缀只贡献宏，不贡献文本（见 §一）
2. "把前缀拼进来再 cook" 在编辑路径上不成立：2 MB 渲染 / 899 ms 渲染 / 5.1 s 全读（见 §二）
3. 事件流可拼接：四条规则，22 个真实头文件里 19 个逐项相同（见 §三、§四）
4. 但复用事件流不划算：省下 252 ms 解析，付出 ~30 ms 复制 + ~60–100 ms 重建 green tree（见 §五）
5. 下一步：共享绿色子树，目标每键 ≤5 ms（见 §六.1）
6. 另一半成本是索引 sweep 的 3.8 s，要靠"facts 按来源文件归档"（见 §六.3 与 `no-build-database.md` §②）
```

---

## 一、现在的 cook 策略与实测

一个文件被编辑时，`Session::cook` 做四件事：

```text
① 取 unit（宏环境）      TranslationUnit 走一遍 include 闭包，只收 #define；磁盘缓存 + 内存上限 16
② 渲染该文件自己的文本   cook_with(file.text, tokens, unit 的宏表)  → 指令解析 + 宏展开，不拼 include 内容
③ 解析渲染               CppParser::parse(rendering)
④ 索引渲染               facts + 诊断（错误范围用 spans 映射回文件坐标）
```

**前缀（所有 include 的内容）在这些步骤里从不被重新渲染**，它只贡献宏定义，而宏环境是缓存的。

在 EmmyLuaCodeStyle 的 `LuaParser/src/File/LuaSource.cpp` 上实测：

```text
每次编辑的 cook（只这个文件）     17.2 ms
它的渲染                          5 111 字节（就是文件自己的文本）
该文件的翻译单元（TU）             1 991 163 字节，103 个文件
  · 渲染                         899 ms
  · 解析（590 135 tokens）        399–479 ms
  · 索引 sweep                    ≈3.8 s
  · 合计 unit read                5.13 s
这个文件自己                      2 085 tokens，解析 1.23 ms
前缀占 TU 的 token 比例           588 050 / 590 135 = 99.6 %
```

## 二、为什么"把前缀拼到文件前面再解析"在编辑路径上不成立

这就是 `read_the_unit` 已经在做的事（整段 TU 渲染 + 解析 + 按文件归档 facts），它是**批处理**用的：

```text
每个按键都重做                    899 ms 渲染 + ~400 ms 解析 + 3.8 s 索引
```

所以这条路线只能用于"一次会话读一次"，不能用于"每次编辑"。但它指对了一个真问题：
**头文件的 facts 目前来自原样摘要**，宏没展开、作用域是错的（xstring 有 1018/1444 条 fact 落在 `scope: None`），
`std::string` 一度因此不认识。

## 三、事件流拼接：机制与四条规则

`cpp_parser` 的输出是事件流 `Vec<MarkEvent>`：

```rust
pub enum MarkEvent {
    NodeStart { kind: CppSyntaxKind, parent: usize },   // parent = 另一个事件的下标
    EatToken  { kind: CppTokenKind, range: SourceRange },
    NodeEnd,
    Trivia,
}
```

事件只引用**事件下标**和**字节范围**，所以"前缀事件 + 后缀事件"在结构上可以拼接。已验证的四条规则：

```text
① 前缀保留自己的 NodeStart(TranslationUnit)，丢掉末尾的 NodeEnd（根不能关）
② 后缀丢掉自己的 NodeStart(TranslationUnit)（下标 0），保留末尾的 NodeEnd（它是合并树的根关闭）
③ EatToken / Trivia 的 range 整体 + 前缀字节长度
   NodeStart 的 parent 整体 + (offset - moved + p - 1)
     · offset = 后缀第一个保留事件在合并 vec 中的下标
     · moved  = 被搬走的后缀 leading trivia 事件数
     · p == 0 指向共享根；p == NO_FORWARD_PARENT 保持不变
④ 后缀的 leading trivia **搬家**到前缀最后一个内部 NodeEnd 之前
```

第 ④ 条是最容易错的一条，原因：整段解析时，边界处的 trivia 被**最后一个声明的 closing bump** 消费，
发出在最后一个 `NodeEnd` **之前**；而后缀单独解析时，同一批字节变成**根下的 leading trivia**。
所以拼接不是"两边都保留"，而是**搬一次** —— 搬完 trivia 在结果里仍然只有一个消费者。

## 四、普查：哪些前缀能拼

用项目自己的 22 个头文件（`LuaParser/include/**/*.h`）作前缀，`LuaSource.cpp` 作后缀：

```text
22 header(s): 19 identical, 0 trivia-shifted, 3 other
  other: LuaSyntaxMultiKind.h, LuaSyntaxNodeKind.h, LuaTokenKind.h
```

- **19/22（86 %）逐项相同**，且 **0 个**属于"边界 trivia 位置"这一类（说明规则④在真实头文件上是对的）。
- 失败的 3 个是同一族、且**可判定**：前缀最后是**尾随注释**（`}// namespace detail`，注释后无换行），
  单独解析会把注释重新词法分析成 `DocComment` 节点并在 EOF 关闭，而整段解析把它挂在别处 —— 差的是**一个节点**，不是位置。
- 结论：实现策略 = 快路径拼接 + **可判定的回退**（最后一个非 trivia token 之后还有注释 → 整段解析）。

## 五、为什么复用事件流是负收益（关键测量）

在真实 TU 上切分（前缀 1 736 378 字节 / 后缀 6 532 字节，TU 共 509 229 tokens）：

```text
整段解析                              310.8 ms     509 229 tokens
ParsedPrefix::of（只解析前缀）        252.3 ms     506 916 tokens
splice（复用前缀事件 + 解析后缀）      144–260 ms
```

**splice 比整段解析还慢**。成本分解：

```text
省下的：解析前缀                      ≈252 ms
付出的：clone 507k tokens + 509k events ≈30 ms
        1.74 MB 文本拷贝                ≈1 ms
        splice/extend 搬运 509k 事件     ≈10 ms
        **在 509k 事件上重建 green tree** ≈60–100 ms   ← 最大项
而整段解析同样要付建树这一项
```

**所以真正的成本在"建树"，而不是"解析"，而建树没有被缓存。**

## 六、后续该做什么

### 6.1 共享绿色子树（核心，必须先做）

```text
目标：splice = 解析后缀（1.23 ms）+ 建后缀树 + 根**直接持有前缀的绿色子节点**（rowan 不可变、可共享）
目标耗时：≤5 ms/键
```

步骤：

1. 确认 rowan 0.15 是否允许"用已有绿色子节点构造新根"（`GreenNode::new` / `GreenNodeBuilder` 的 children 入口）。
2. 若不允许，给 `crates/cpp_parser/src/syntax/tree/cpp_green_builder.rs` 的 `CppGreenNodeBuilder` 加
   `push_green(GreenNode)`；给 `CppTreeBuilder`（`crates/cpp_parser/src/syntax/tree/cpp_tree_builder.rs`）
   加"以一个已有绿色子节点开始"的入口。
3. splice 入口放在 `crates/cpp_parser/src/parser/cpp_parser.rs`（`parse_inner` 旁），返回 `CppSyntaxTree`。
4. 缓存放在 `crates/cpp_code_analysis/src/session/mod.rs`，键与 `renderings` 同构（内容哈希），
   存四件：**渲染文本 / token 流 / 绿色根 / 边界状态**。
5. 边界状态用现成的 `Checkpoint`（已包含 `events_len`、`token_index`、`open_marks`、
   `declaration_type_name`、`declaration_type_is_qualified`、括号与模板深度、`terminator_came_from_a_branch`、journal）。

### 6.2 守卫与回退

```text
· spliceable 判定：前缀最后一个非 trivia token 之后仍有注释 → 回退整段解析（§四）
· 前缀失效：任一 include 改动 → 前缀内容哈希变化 → 重解析一次（编辑自己文件不会触发）
· 内存上限：2 MB 文本 + 590k tokens + green tree（估计 60–120 MB），需要像 MAX_UNITS/MAX_VIEWS 一样设界
```

### 6.3 另一半成本：facts 按来源文件归档

即使解析免费，`unit read` 里还有 ≈3.8 s 的索引 sweep。要让"每键都能拥有整个 TU 的正确作用域"，
必须让 sweep 变成增量：**一次 TU 解析，facts 按"写在哪个文件"归档**（spans 已经记录了每个 token 的来源文件）。
方案见 `docs/no-build-database.md` §②，本文件的 §6.1 是它的**前置条件**。

### 6.4 验证方式

```text
· 等价性：splice 出的树与"两段拼起来整段解析"的树，token 数 / 根覆盖文本 / 错误数三项相等
· 普查：22 个头文件里 ≥19 个逐项相同（比例守卫 3/4），三个已知例外保持为"回退"
· 性能：目标 splice ≤5 ms；不达标就说明绿色子树没有真正共享
```

## 七、复现方式（实验代码在 git 历史里）

四段实验代码已 revert，需要时按顺序 cherry-pick：

```text
bac1b9d  事件流拼接实验（四条规则）+ grammar::cpp 改 pub(crate)
7cebafc  parent 重基修复：offset - moved + p - 1
59ba4f9  普查测试（19/22 + 3/4 比例守卫）
66c1ada  ParsedPrefix API（of / splice / spliceable）+ 负收益测量
```

## 八、风险与未验证

```text
· 宏环境：前缀解析依赖 ParserConfig 的宏表（with_macros_from_includes）；不同 unit 的宏不同，
  缓存键必须包含宏环境（或按 unit 缓存），否则会拿另一个 unit 的读法回答
· 错误位置：splice 后错误的 offset 需要 re-base；实验里是把两边的 errors 直接串起来，未逐条验证
· 样本太小：22 个前缀 + 1 个后缀；需要对 MSVC 标准库闭包（126 文件）和 478/597 文件项目再做普查
· 跨文件构造：前缀若在一个未闭合的 namespace / extern "C" 里结束（MSVC 头文件的常见写法），
  规则①（前缀必须"解析完整"）就不成立，需要显式检测 open_marks == 0
· 内存与失效：2 MB 前缀 + 590k tokens 的常驻成本，以及头文件改动导致的整体失效，都要先量
```

## 九、相关文档

```text
docs/no-build-database.md     §② facts 按来源文件归档（本文件 §6.3 的上游方案）
docs/architecture-review.md   层与边界、写锁预算、通知/请求顺序
docs/syntax.md                语法树与事件流的结构
```
