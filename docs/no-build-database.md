# 没有 `compile_commands.json` 的项目怎么分析

**结论先行。MSVC 项目拿不到 `compile_commands.json` 是常态，不能作为前提。**

```text
1. 配置来源要有一个阶梯：.cppls.toml → compile_flags.txt → CMakeCache.txt → 项目布局推断 → 编译器自身
2. facts 要按“来源文件”归档：一次 TU 解析，头文件拿到正确的(宏展开后的)摘要
3. 限定名声明(分离定义)必须建 fact，声明与定义是同一条符号的两个位置
4. 绝不逐个头文件 cook
```

---

## 一、为什么不能依赖 compile_commands.json

CMake 自己的文档写得清楚（[CMAKE_EXPORT_COMPILE_COMMANDS](https://cmake.org/cmake/help/latest/variable/CMAKE_EXPORT_COMPILE_COMMANDS.html)）：

> This option is implemented only by Makefile Generators and Ninja Generators. It is ignored on other generators.

也就是说 **Visual Studio 生成器（`.sln`/`.vcxproj`）永远不会有 `compile_commands.json`**；
`CMAKE_EXPORT_BUILD_DATABASE`（CMake 3.31+，[文档](https://cmake.org/cmake/help/latest/variable/CMAKE_EXPORT_BUILD_DATABASE.html)）同样写着
"implemented only by the Ninja Generators"。

本机实测（`C:\Users\zc\Desktop\github\EmmyLuaCodeStyle`）：构建目录 `out/build/x64-Debug` 有 `CMakeCache.txt`，
但既没有 `compile_commands.json`，也没有 `.vcxproj`（那是 Ninja 生成的）。我们当前的 `discover` 甚至没读到那份 `CMakeCache.txt`。

## 二、clangd 是怎么做的（值得抄的部分）

来源：<https://clangd.llvm.org/design/compile-commands>、<https://clangd.llvm.org/design/indexing>

```text
① 头文件借用包含者的命令
   “If no command is available for a header, but a file has been opened that transitively includes it,
    then that file's command will be used.”（HeaderIncluderCache）
   没有包含者时，用文件名最接近的那条 CDB 条目（InterpolatingCompilationDatabase）。

② 没有 CDB 也能工作
   compile_flags.txt（所有文件共用一组 flag），以及一个极简的命令 `clang foo.cc`。
   “For a real project this will often fail to find #includes, but it allows clangd to work on toy examples.”

③ 向编译器要它自己的配置
   --query-driver：跑 `<compiler> -E -v -x c++ /dev/null`，把它打印的 search list 折成
   --target=… 和 -isystem=…（这一步我们已经在做，而且做得更宽：不问权限也问）。

④ 索引是“按文件”的
   FileIndex：已打开的文件以及它们包含的头，数据按文件分别存放。
   BackgroundIndex：按 CDB 逐 TU 解析，一个头没有自己的 TU，就在包含它的 TU 里被索引，
   结果按文件归档（standard library 的头走用户缓存目录下的 .idx）。
```

还有一条 clangd 自己承认的局限**我们不必继承**：

> Clangd parses headers like any other source file (which is why it only supports self-contained headers).
> If no command is available for a header … it will typically fail to find a compilation database …

它把"头文件单独当 TU 解析"，所以要求头是 self-contained；
我们可以在**包含它的 TU 里**解析它（下面的方案 ②），这正是它做不到的。

## 三、我们的方案（按优先级，每项带落点与验收）

### ① 配置阶梯：先补两个"不需要 CDB"的来源

落点：`crates/cpp_code_analysis/src/include/toolchain.rs`、`src/project/`（配置装配处）

```text
a. .cppls.toml [compile] args/extra_args/remove_args        已有，保持第一优先
b. compile_flags.txt（与 clangd 同名同义）                   新增：一个目录一份，逐行一个 flag
c. CMakeCache.txt                                           新增：读 CMAKE_CXX_COMPILER / CMAKE_CXX_STANDARD /
                                                            CMAKE_CXX_FLAGS / CMAKE_C_FLAGS
   搜索位置：<root>/build、<root>/out/build/*、<root>/cmake-build-*（VS "打开文件夹"的布局就是 out/build/<preset>）
d. 项目布局推断                                             新增：把"包含头文件的目录"作为候选 include root
   规则（保守）：文件名含 include/inc/include/public 的目录；
                以及被 #include "X/Y.h" 引用时，能拼出 X/Y.h 的祖先目录（按需、缓存）
e. 编译器自身                                               已有（g++/clang++/cl.exe 的 search list）
```

验收：EmmyLuaCodeStyle（无 CDB）上 `discover` 能打印出 `CMAKE_CXX_STANDARD=20` 与 `LuaParser/include` 这类目录；
`#include "LuaParser/File/LuaSource.h"` 在**不**打开任何构建描述的会话里也能解析。

### ② facts 按"来源文件"归档（一次 TU 解析，头文件拿到正确摘要）

落点：`crates/cpp_code_analysis/src/index/mod.rs`（`index_rendering`）与 `summary.rs`

现状：`cook(entry.cpp)` 产出**一个** `FileSummary`（路径=入口文件），且 `map_into_the_file` 只保留范围能映回入口文件的 facts。
于是被包含的头永远只有"原样读入"的摘要 —— MSVC 的 `_STD_BEGIN`（`namespace std {`）在那种摘要里不存在，
`xstring` 的类全被吞进 `_String_const_iterator::_String_val` 这种错位作用域（实测：`members_of("std::string")` = `NotDeclaredHere`）。

做法：`RenderedCooked.spans` 已经记录了每个渲染记号来自哪个文件、哪个范围（`map_into_the_file` 就是用它）。
把"按范围过滤"改成"按来源分组"：

```text
for fact in facts:
    origin = rendered.origin_of(fact.range)      # (file, range)
    summaries[origin.file].declarations.push(fact with range mapped into that file)
```

这样**不用逐头 cook**，STL 头在任何一个包含它的 TU 被解析时顺带拿到正确摘要。

验收（EmmyLuaCodeStyle，无 CDB）：
`members_of(std::string)` 有成员；`members_of(std::basic_string<char>)` 有成员；`xstring` 的 facts 不再挂在 `_String_val` 下。

### ③ 限定名声明也要有 fact（分离定义）

落点：`crates/cpp_code_analysis/src/sema/declarations.rs`（`build_facts`）

现状（实测 `LuaParser/src/File/LuaSource.cpp`）：原文 6507 字节、53 个声明、0 个解析错误，而索引里 **0 条 fact**；
cook 也是 `declarations=0`。原因：facts 只从**作用域绑定**产生，而限定名声明（`LuaSource::From`）按设计不绑定。

做法：`build_facts` 的第二遍。对声明符是限定名的声明，读出名（最后一段）与限定部分（作用域），
补一条 fact（kind/returns/type_of 走现有 `fact_for` 的同一套读取；必要时把 `fact_for` 的 `Binding` 依赖抽出来）。

验收：
`files_declaring("From")` 里同时有头文件的声明与 `LuaSource.cpp` 的定义；
从 `LuaSource.h` 触发 find-implementations 命中 `LuaSource.cpp:5`；定义处 hover 有返回类型。

### ④ 成本控制

```text
· 每个 TU 的 closure 有上限（现在没有），超了就只做"按来源归档"的第一层，不再深入
· 同一头被多个 TU 归档时：合并且按符号身份去重（声明/定义一条符号，两个位置），不要互相覆盖
· 摘要缓存（已有）按"文件+配置哈希"复用；无 CDB 时配置哈希只用工具链与 flag，不随 TU 改变
```

## 四、明确不做（第一阶段）

```text
· 解析 .vcxproj / .sln（clangd 也不做；先看 CMakeCache + 布局推断是否够用）
· 逐头 cook（成本是 TU 数 × 头大小，与 ② 相比是纯浪费）
· 依赖 compile_commands.json 作为唯一来源（本文的全部理由）
```

## 五、风险

```text
② 是索引层的结构改动（facts 从一个摘要变多个），会动到缓存格式与 SummaryKey；
   先做一个开关（默认开、可按 .cppls.toml 关掉），用 EmmyLuaCodeStyle 与 MSVC STL 闭包同时对比索引时间与内存。
③ 的"合成 fact"要保证与绑定产生的 fact 在 qualified_name 上一致，
   否则同一声明会出现两条不同名字的符号（验收里加一条：声明与定义 qualified_name 相等）。
```

---

参考：

- clangd, *Compile commands* — <https://clangd.llvm.org/design/compile-commands>
- clangd, *The clangd index* — <https://clangd.llvm.org/design/indexing>
- CMake, *CMAKE_EXPORT_COMPILE_COMMANDS* — <https://cmake.org/cmake/help/latest/variable/CMAKE_EXPORT_COMPILE_COMMANDS.html>
- CMake, *CMAKE_EXPORT_BUILD_DATABASE* — <https://cmake.org/cmake/help/latest/variable/CMAKE_EXPORT_BUILD_DATABASE.html>

## 实测记录（本地 `E:\EmmyLuaCodeStyle`）

### ③ 的根因比文档写的更窄，也更致命

```text
LuaParser/src/File/LuaSource.cpp   242 行、2085 token、0 解析错误 → **0 条 fact** ✗
```

四个形状在**有 include 的夹具里全部正常** ✓（`std::size_t` 返回 ✓、模板返回类型 ✓、构造函数＋初始化列表 ✓、
`std::shared_ptr<C> C::From(int)` ✓），所以不是语法形状的问题。

**决定性实验**：同一段定义，加上两条**无法解析**的 `#include`，fact 变成 0 ✗。

渲染文本是**对的** ✓（`unsigned long C::GetLine(…​) const { … }` 一字不差），所以问题不在文本，而在
**限定名的前缀从哪来**：`fact_for` 的 `scope` 参数来自 `scopes.qualification_prefix_of(id)`，而作用域树只有在
**类的声明也在本文件里**时才认得 `C::`。真实 `.cpp` 的类在头文件里 ⇒ include 解析不了 ⇒ 前缀为 `None`
⇒ **带限定名的声明符一条 fact 都不产生** ✗。

这比文档 ③ 的描述更窄（不是"限定名声明不绑定"）也更致命（**无 CDB 的工程里，每个 `.cpp` 的每个成员定义都不可见** ✗）。
修法就是文档写的那条第二遍，只是从前缀的来源入手：**从声明符自己的文本取**（最后一个 `::` 之前是作用域、
之后是名字），不要从作用域树取。

### ① 的现状

配置阶梯的第一步 `.cppls.toml` 已有 ✓；`compile_flags.txt` ✗、`CMakeCache.txt` ✗、布局推断 ✗ 都还没有。
本地实测路径是 `E:\EmmyLuaCodeStyle`（文档里的 `C:\Users\zc\Desktop\github\…` 是另一台机器）。

### 尝试过的修法，以及为什么不够

`fact_for` 的 `scope` 现在会在作用域树答 `None` 时**从声明符文本**推（声明符末尾是名字，名字前面的 `C::` 是作用域）。
这一步是对的、也已保留，**但它救不了那 0 条 fact** —— 因为**绑定本身就不存在** ✗：限定名声明符不产生绑定
（文档写的"按设计不绑定"），`fact_for` 根本没有被调用。

⇒ 必须做文档 ③ 的原方案：**在 `build()` 里加第二遍，遍历语法树**找声明符是限定名的声明，直接合成 fact
（名字取最后一段、作用域取前缀，`kind`/`returns`/`type_of` 复用现有读取）。这一遍不经过绑定，
所以"类在头文件里、include 解析不了"不再有影响。

代码里留下的东西：`fact_for` 的前缀回退 ✓（对**有绑定**的限定声明符正确 ✓），以及被 `#[ignore]` 的复现测试
`an_unresolved_include_does_not_erase_the_files_own_facts`（理由就是上面这段）。

### 已实现：第二遍遍历语法树 ✅

```rust
// sema/declarations.rs::build，绑定遍之后
for node in root.descendants() {
    if !declaring_kinds().contains(&kind) { continue }
    let range = source_range(node.text_range());
    if claimed.iter().any(|c| range inside c) { continue }   // 绑定遍已经建过的跳过（按**包含**，不是相等）
    if let Some(fact) = qualified_declarator_fact(root, range) { facts.push(fact) }
}
```

`qualified_declarator_fact` 从文本里读：第一个 `(` 之前的最后一段标识符就是声明符 ✓，含 `::` 才合成 ✓，
名字取最后一段 ✓、作用域取前缀 ✓、返回类型取前缀之前 ✓、参数表取括号段 ✓、范围取自节点 ✓。

**效果（真实工程）**：

```text
LuaParser/src/File/LuaSource.cpp    declarations: 0 → **22**    tokens 2085, parse errors 0
```

复现测试 `an_unresolved_include_does_not_erase_the_files_own_facts` **已转为通过** ✓（不再 ignore ✓）。

**三个测试因此变红，它们是"旧计数"的期望，需要按新事实复核**（本次会话结束前未完成）：

```text
index::project::tests::an_overload_set_is_a_list_and_a_namespace_is_one_entity   重载集现在多了定义一侧 ✓
the_folds_of_a_file_are_the_regions_a_reader_hides                              折叠范围随符号数变化 ✓
a_declaration_in_the_viewed_file_is_reported_on_its_own_line                    报告位置按符号数变化 ✓
```

每一条都该**逐条确认新数字是对的**，而不是把期望改成新数字 —— 这是本轮唯一未完成的收尾。