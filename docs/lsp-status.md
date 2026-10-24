# LSP 功能自测：哪些能用，哪些要改（2026-10-24）

这份表来自 `crates/cpp_ls/tests/lsp_status.rs`：起真服务器、开真项目（`E:\EmmyLuaCodeStyle`）的
`CodeFormatCLib/src/CodeFormat.cpp` 和 `codeformat.h`，等索引排空（80 次 pull、22 s），然后每个功能问一个
**有形状的**问题（条数、落在哪个文件哪一行），打印一行。

```text
$env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
cargo test --release -p cpp_ls --test lsp_status -- --ignored --nocapture
```

## 表（一次真实运行，2026-10-24 21:2x）

```text
feature                answered n     time   note
hover                  yes      3     22 ms  `_diagnosticStyle = LuaDiagnosticStyle();` → `LuaDiagnosticStyle _diagnosticStyle`
definition (member)    yes      1     2 ms   codeformat.h:56
definition (header)    yes      1     1 ms   codeformat.h:56
implementation         yes      1     5 ms   CodeFormat.cpp:31
references             yes      6     13 ms   codeformat.cpp:32
documentHighlight      yes      0     1 ms   （成员不是"local"，见下）
prepareRename          no       0     10 ms  null
rename                 no       0     10 ms  null
documentSymbol         yes      14    1 ms   GetInstance, CodeFormat, UpdateCodeStyle
workspaceSymbol        yes      512   7 ms
foldingRange           yes      39    1 ms
selectionRange         yes      5     1 ms   5 nested range(s)
semanticTokens         yes      507   31 ms
inlayHint              yes      5     25 ms
documentLink           yes      5     2 ms
diagnostic             yes      0     33 ms
completion (member)    yes      2     27 ms   Editorconfig, Workspace
signatureHelp          no       0     0 ms   null for the first 8 call sites
codeAction             yes      0     1 ms
rangeFormatting        yes      0     2 ms
formatting             yes      0     2 ms
```

## 合理（能用，且有测试或这次实测支撑）

```text
hover                 类型 + 名字，答案里带 LuaDiagnosticStyle；22 ms
definition            成员只给本类那一条（今天的收窄规则），头文件里的声明也算一条
implementation        **今天修好的**：头文件声明 → CodeFormat.cpp:18/31（见"这次发现的缺陷"）
references            6 条、跨文件、含头文件声明；按类过滤 + 接收者类型过滤
documentSymbol        14 个顶层符号，1 ms
workspaceSymbol       512 条（"Format" 前缀），7 ms
foldingRange          39 段，1 ms
selectionRange        5 层嵌套，1 ms
semanticTokens        507 个 token，31 ms（同步、全量）
inlayHint             5 条，25 ms
documentLink          5 条 include，2 ms
diagnostic            pull 模式，0 条（见下"数字对不上"一节）
completion            成员补全 2 条（`Editorconfig`/`Workspace`）——`_configs` 的成员，正确
formatting / rangeFormatting / codeAction   都回答（空数组合法：这份文件已经格式化过、区间里没有诊断）
```

## P1：已完成（2026-10-24 晚）

### ① `rename` / `prepareRename` 支持普通符号 ✅

宏的路由不变（`macro_at` 先问），没有宏时走 `symbol_at`：**拼写和 range 取自文件自身文本**
（`name_at_including_directives`），`definitions` 只用来确认"这是个符号"——它的回答可能在头文件里，而光标不
在那儿。编辑用的就是引用那条规则（`references::name_level_offsets`，抽出来给 rename 用，位置报告和字节偏移
两个视图不会各算一遍），所以"一个类的成员不是另一个类的"由构造保证。

判据（`a_symbol::is_renamed_in_its_own_class_and_the_declaration_follows`）：两个类各有 `_value`、各用一次
→ 2 处编辑（头文件声明 + 本类用法），`Second` 的声明和用法都不在里面。
真实项目上：`prepareRename` 回答 `_diagnosticStyle` 的 range，`rename` 回答 `codeformat.h: 1 | codeformat.cpp: 6`。

### ② `documentHighlight` 支持成员 ✅

local 的路径不变；local 没有答案时走 `symbol_highlights`——同一个 `name_level_offsets`，限定在**当前文件**，
声明那一条是 `Write`、其余是 `Read`。真实项目上从 `0` 变成 **6**。

判据（`a_symbol::is_highlighted_in_this_file_and_another_classs_is_not`）。

### ③ 位置换算：五个 handler 改成"先文件、再渲染" ✅

`hover`、`signatureHelp`、`codeAction`、`documentHighlight`、`selectionRange` 以前
`session.view(&path)?`（渲染）配 `offset_at_position(&view, position)`；现在一律

```text
written  = session.view_of_the_file(&path)      文件自身的文本
offset_f = file_offset_at_position(&written, position)
offset_r = rendering.reading_offset_of(FileOffset::new(offset_f))   需要渲染时
```

`documentHighlight` 和 `selectionRange` 两步都留在文件坐标里（它们的查询本来就是文件坐标的）。

## P2：这一轮做的（2026-10-25 凌晨）

### ④ `signatureHelp` 在真实项目上回答了 ✅

三件事，每一件都是"零答案"链条上的一环：

1. **位置**：光标必须落在**参数表里面**——`signatures_at` 的文档就是这样写的。探针和第一版测试都把光标放在 `(` 或
   函数名上，那里的正确回答就是"没有调用"。
2. **`parameter_list` 缺失**：索引里 `definitions("SetupStyle")` 回答的是 `.cpp` 里的**定义事实**（"own file
   wins"），而它的 `parameter_list` 是 `None` —— 因为 `fact_for` 通过作用域遍历的 declarator 读参数表，而类外定义的
   **名字写在说明符序列里**，declarator 里没有名字，遍历取不到。修：`qualified_declarations` 自己从声明头部读参数表，
   `build` 把它补进事实。
3. **候选被静默丢弃**：`signatures_at` 里 `parameter_list_of(declared_in, offset)` 在同一个文件里也找不到 declarator，
   `sources` 为空 → `for` 循环零次。修：`sources` 为空而事实带 `parameter_list` 时，用事实造标签（和另一个文件那条
   回退路径同一个形状）。

实测（`tests/signature_real.rs`）：`SetupStyle(style, options);` → `SetupStyle(LuaStyle &style, const
FormattingOptions &options)`；LSP 探针同一行：`signatureHelp yes 1 at 82:17 → SetupStyle(...)`。

### ⑤ 一个 local 的改名：真正的缺口是"只有被用到的声明才有条目"

`local_references_of` 把**使用**记在声明名下，声明本身被排除；所以"声明了但没人用"的 local 没有条目。这不是"宏做
数组长度"的问题（`int more[N];` 在 `tests/scopes.rs` 里是正常的）。改名现在这样处理：

```text
local → 只在本文件内改（跨文件会改到别的文件的同名 local）
         列表以光标下的名字开头，再加上条目里的使用（可能没有）
```

`handshake.rs` 断言的就是这个形状：`more`（声明，未使用）→ 1 处编辑，在本文件。

### ⑥ `diagnostic` 的 9 条假报：一个解析器 bug，已修

`CodeFormat.cpp` pull 出 9 条：`result`/`formattedText` 没有 `size`/`begin`/`end`，`config.Workspace` 没有 `size`，
`_configs` 没有 `erase`。`auto result = f.GetFormatResult(t);` 那个 `auto` **推对了**（`std::string`），错的是成员表。
`CPPLS_TRACE_MEMBERS=1` 的实测：

```text
[undefined-field] `std::string` -> `size`  (27 member(s), 0 unlisted) — index: 135 posting(s), 0 in scope `std::string`
[undefined-field] `std::string` -> `begin` (27 member(s), 0 unlisted) — index: 194 posting(s), 0 in scope `std::string`
[undefined-field] `std::vector` -> `erase` (303 member(s), 0 unlisted) — index: 122 posting(s), 278 in scope `std::vector`
```

读法：**成员表不完整，但不是"unlisted"的那种不完整**（`unlisted` 是基类，这里 0）。两个不同的缺口，都追到了根：

* `std::string` 的 27 个成员**全是 typedef**，正好是 `struct _Alloc_hider` 之前声明的那些 —— 那是解析器 bug（见下）。
* `std::vector` 的 303 个成员里没有 `erase`，而 `facts_probe` 在 `bits/stl_vector.h` 上给出
  `erase` → **0 个事实**、`push_back` → 2 个 —— 也是别的 bug，见下面最后一节。

（`scope_postings` 是"这个 scope 下有多少 posting"，**不是某个名字在这个 scope 下的数量**；早先那行
`0 in scope std::string` 被我读成了"别名没把成员挂上去"，是误读。真正的证据是 `facts_probe` 打印的 scope 名。）

所以下一次不是"关掉这个检查"，而是让 `members_of` 与 `name_postings` 对同一个类给出一致的答案。**这一条是 P2 的
第一优先项。**

#### 追下去的结果：一个解析器 bug，注释吃掉了类的右花括号

`members_of("std::string")` 的 27 个成员**全是 typedef/嵌套类型**，正好是 `struct _Alloc_hider` 之前声明的那些。
看 `facts_probe`：

```text
@36242   std::basic_string::_Alloc_hider::size    kind Function ...
```

`basic_string` 的每个成员都被归到了**嵌套类** `_Alloc_hider` 名下。再往上，最小复现是：

```cpp
struct S {
    int x; // note
};
int size();
```

```text
ClassBody {
  Declaration int x;
  DocComment // note
  ErrorNode }          ← 类的右花括号
  Token ;
  Declaration int size();    ← 被吞进类体
}
```

原因在 `parse_class_body_members` 的循环里：循环条件在 `emit_trivia_after_current_token()` **之前**求值，所以
"右花括号前面有注释"时游标带着**零债务**走到那个分支，而分支原来无条件地 `unclosed_braces -= 1`（变成 `-1`）并把
`}` 收成 `ErrorNode` —— 类体于是越过自己的结尾继续。libstdc++ 的 `_Alloc_hider` 后面正好有一句
`// TODO check __is_final`，于是 `std::basic_string` 之后的一千多个成员全部落到嵌套类里，`size()` 成了"没有成员"。

修：那个分支先看债务，`unclosed_braces <= 0` 时就是**本体的右花括号**，`break`。同一形状的第二个循环（文件作用域的
声明序列）一起修。

```text
std::string      27 → 257 member(s)   size/begin/end 都在
std::basic_string 27 → 257
CodeFormat.cpp   9 条假报 → 0（服务器 pull）；批处理仍报 1 条：`std::vector::erase`
```

#### `std::vector::erase`：也是解析器 bug，已修

```cpp
      _GLIBCXX20_CONSTEXPR
      iterator
#if __cplusplus >= 201103L
      erase(const_iterator __position)
      { return _M_erase(begin() + (__position - cbegin())); }
#else
      erase(iterator __position)
      { return _M_erase(__position); }
#endif
```

`#if` 夹在**返回类型和函数名之间**。索引的第一次读取用的是文件**自身文本**（指令还在，见下一节），所以
`iterator` 后面跟着的是 `#`；而"这个名字后面还有没有声明符"这个问题遇到 `#` 时回答"没有" —— 于是 `iterator` 被当成
声明符的名字，声明在 `#` 处结束，`erase` 变成了另一个没有参数表的声明。`facts_probe` 在这个头文件上：
`erase` **0 个事实**、`push_back` 2 个。

修法：那个问题（`a_declarator_still_follows_the_name`）**看穿指令**——跳过它整条**逻辑行**
（`logical_line_end`，和 `parse_preprocessor_directive` 的边界同一个函数），再看下一个 token。于是
`iterator` 加入类型、说明符序列自己的"两条说明符之间的指令"规则读掉 `#if`、声明符是 `erase(const_iterator)`。

```text
bits/stl_vector.h    erase: 0 → 2 fact(s)，scope `std::vector`
std::vector          303 → 285 member(s)，erase 在里面
CodeFormat.cpp       批处理最后一条假报消失（9 → 1 → 0）
```

#### 为什么"已经预处理过了"还会看到 `#if`

索引有**两种读数**：

```text
文件自身文本（eager）   快，不需要 include 闭包；指令、宏都还在 —— 启动时和没 cook 过的头文件用它
渲染（cooked）          指令已解析、宏已展开；一次 cook 要走过 include 闭包，是重的那条路
```

服务器后台会 cook（日志里的 `N to cook`），cook 完的摘要替换掉未 cook 的。所以 `#if` 只在**未 cook 的窗口**里存在：
`members_real.rs` 里实测，`bits/stl_vector.h` 在被要求渲染之前 `erase` 不在，cook 之后就在（305 个成员）。
这也意味着**解析器必须在任何位置都容忍指令**，不能指望"preprocessed 就没有 `#` 了"。
## P2 ⑦ 未 cook 窗口的成员损失：一个探针，一类 bug

`docs/lsp-status.md` 上一节解释了为什么"预处理过了"还会看到 `#if`：索引先读**文件自身文本**。那么问题就从"某个
名字找不到"变成"这些文本里还有多少成员根本没读出来"。新探针 `examples/parse_loss_probe`：

```text
parse_loss_probe <dir-or-file> …     每个文件：函数事实里 parameter_list 为 None 的个数、ErrorNode 个数
parse_loss_probe --show <file> <n>   把那些声明本身打出来 —— 形状才是修法的依据
```

在 libstdc++ 的 579 个头文件上：

```text
15 253 个函数事实，1 937 个没有参数表（12.7%）
bits/stl_algo.h  88/353     bits/basic_string.h 73/336     bits/cow_string.h 72/221     bits/ostream.h 53/66
```

### 第一个形状：声明符里套了一层前缀（已修）

`bits/ostream.h` 的 53/66 全是同一种：

```cpp
      __ostream_type&
      operator<<(long __n)
      { return _M_insert(__n); }
```

解析树是

```text
InitDeclarator
  Declarator  &  operator<<(long __n)     ← 名字和参数表在这里
    Declarator  &                         ← 而这是路径上最后一个声明符
    NameExpr    operator<<
    ParameterList (long __n)
```

事实层取的是**路径上最后一个**声明符（那个 `&`），它没有参数表。凡返回类型以 `&`/`*` 结尾的函数都受影响 ——
签名帮助、hover、参数检查看到的都是"没有参数"。

修：取**直接子节点里含这个名字**的那个声明符（`names_the_binding`）。函数指针的参数表不受影响 ——
`void (*f)(int)` 的名字不是那个含参数表的声明符的直接子节点。

```text
1 937 → 312（12.7% → 2.1%）
std::vector 的成员现在带签名：assign(size_type __n, const value_type& __val)、
                             assign(_InputIterator __first, _InputIterator __last)、…
```

### 第二个形状：限定名调用的最易解析歧义（已修）

`--show` 剩下的 312 条里，`bits/stl_algo.h` 的那 22 条是这样的：

```text
Declaration  std::iter_swap(__result, __b);
  DeclSpecifierSeq  std::iter_swap        ← 类型名
  InitDeclarator  (__result, __b)         ← 参数表当成了声明符
```

`std::iter_swap(__result, __b);` 是**调用**，被读成"类型 `std::iter_swap`、声明符 `(__result, __b)`"。作用域遍历里
有一条分支专门为**类外定义**写的（`size_t LuaSource::GetLine(…) { … }`：限定名 + 声明符里没有名字），它把这种
声明当成了"外来函数的定义"，于是**把函数本身绑定进了 `std` 作用域** —— 22 个 `kind Function`、`local: false`、
还带着外层模板参数的事实。

定义不可能嵌在函数体里，所以判据就是"在不在 body 里"，和 `qualified_declarations` 那条同一句话。

```text
libstdc++ 579 个文件    56 734 → 55 549 事实        （1 185 个幽灵事实消失）
                        15 253 → 14 609 函数事实    （644 个是调用语句）
                        没有参数表的函数 312 → 18
CodeFormat.cpp          150 834 个事实，0 条诊断
```

剩下的 18 是两种形状，都不是"成员丢了"：`alloc_traits.h` 的 6 个是 **concept**（`concept __has_allocate_hint = requires (…) { … };`，里面还夹着 `#if`），`fs_path.h` 的 12 个是**转换运算符**（`operator string() const`）。把最小夹具单独喂进去**复现不出来**（concept 正确成为 `Type` 事实、转换运算符没有留下碎片），所以它们依赖上下文（concept 的 `requires` 里夹 `#if`、转换运算符所在的类）。0.12%，留作下一轮的一个小目标。