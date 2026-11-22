# Parser / Toolchain 实现审查

审查基线：当前仓库 `72f24593`。范围包括 `cpp_parser`、`cpp_toolchain`，并检查了
`cpp_code_analysis` 中把二者组合到实际索引路径的调用方式。本文关注会改变 CST 形状、
预处理结果、跨平台行为或公开 API 可靠性的问题，不把单纯的格式、命名风格列为缺陷。

这不是一份 C++ 标准符合性证明。C/C++/CUDA 的语法、预处理和不同编译器扩展面过大，本次
结论来自源码、现有测试、最小复现和真实调用链检查；仍应把编译器差分测试作为长期护栏。

## 结论摘要

整体设计方向是对的：lossless CST、事件流审计、显式回滚、预处理结果携带源码映射、typed
AST 和“已知缺口列表”都很适合语言服务器。当前最值得优先处理的不是再补一批语法节点，而是：

1. parser 的“局部类型名”状态实际上按 brace 深度近似，并且跨兄弟函数泄漏；它可以把合法
   调用静默解析成声明。
2. 标准/编译器特性配置目前不是严格意义上的单一事实来源；严格模式会接受部分 vendor
   扩展，调用顺序还可能覆盖已经设置的 dialect vendor。
3. C++26 前向语法中有“无条件 keyword”做法，已经破坏合法 C++23 标识符。
4. 事件流损坏后的 fallback 与它自己的注释承诺不一致，release 构建下仍可能把后续节点吞入
   错误前驱。
5. 持久化语法 ID 的反序列化对非法输入仍可 panic。
6. include graph 对条件可见性不是顺序计算的：文件前半段刚写入的 `#define` 不会参与后面
   `#if` 的决定，未知分支的定义和未读 include 也没有完整地传播 uncertainty。
7. guard 的复用判断不读取当前宏状态，`#undef GUARD` 后的第二次 include、inactive
   `#pragma once` 都可能被错误跳过。
8. Windows/MSVC 路径仍有明显漏洞：`/I /D /U` 不进配置，quoted backslash header name
   不解析，angle include 还可能被推断出的项目 `include/` 根抢先。

优先级含义：

| 优先级 | 含义 |
| --- | --- |
| P1 | 已经可造成静默错误 CST、合法代码误报、错误预处理结果或进程健壮性问题 |
| P2 | 较窄输入、跨平台、性能或公开 API 方向的问题，建议近期修 |
| P3 | 长期能力、维护成本或测试体系补强 |

## Parser：P1 问题

### P1-1：`TypeNames` 不是真正的作用域表

证据：

- [`TypeNames::bindings`](../crates/cpp_parser/src/parser/type_names.rs#L57) 是
  `HashMap<name, depth>`，只保留一个“最近声明”。
- [`enter_scope` / `leave_scope`](../crates/cpp_parser/src/parser/type_names.rs#L111)
  只增减整数深度；离开作用域时不会删除或恢复同名绑定。
- [`is_a_type`](../crates/cpp_parser/src/parser/type_names.rs#L88) 用
  `declared_at <= current_depth` 判断可见性。

因此：

* 两个兄弟函数通常处于相同深度。第一个函数中声明的局部类型，进入第二个函数后仍被视为
  可见。
* 内层同名类型覆盖外层类型后，外层绑定被永久丢弃。
* 普通变量对外层类型的遮蔽没有记录，`TypeNames` 只能表达“type / unknown”，不能表达
  “这个名字在当前作用域是 value”。

最小复现：

```cpp
void first()
{
    typedef long Leaked;
}

void second()
{
    auto Leaked = [] {};
    Leaked(); // 合法调用
}
```

当前严格 C++23 解析结果没有报错，但第二个 `Leaked()` 被放进 `Declaration`：

```text
Syntax(Declaration)
  Syntax(DeclSpecifierSeq)
    Syntax(NamedType) "Leaked"
  Syntax(InitDeclarator)
    Syntax(Declarator)
      Syntax(ParameterList) "()"
```

也就是说 `CallExpr` 整体丢失。这个问题会直接影响 hover、补全、引用、签名帮助和后续语义
分析，而且是“无诊断的错误形状”，lossless 和“没有 parse error”都无法发现。

建议：

1. 将每个块的类型绑定改为真正的 scope stack；退出时恢复上一 binding。
2. 记录 declaration/value shadow，至少能表示 `Type | Value | Unknown`。
3. 规范化名字（见下文 universal character name），不要直接以源码字符串为唯一键。
4. checkpoint 在回滚时也能恢复符号表版本；不要只恢复事件流和 cursor。
5. 加入“兄弟函数局部类型泄漏”和“内层类型覆盖后恢复外层类型”的 CST 形状测试。

`4096` 个不同名字的硬上限目前还会静默停止记录新类型，见
[`MAX_NAMES`](../crates/cpp_parser/src/parser/type_names.rs#L75)。先修正作用域语义，再考虑
用按作用域分配或可测的淘汰策略替代这个全局上限。

### P1-2：`LanguageConfig` 没有成为配置的权威来源

`Feature` 已经有标准和 vendor 的需求模型，但 lexer/grammar 只在少数点读取它；大量
vendor 语法依靠 token 形状直接接受。

已复现的严格 C++23 输入：

```cpp
void f()
{
    __try {} __finally {}                 // MSVC SEH
    auto block = ^(int value) {           // Clang block literal
        return value;
    };
}
```

即使 `ParserConfig::with_language(LanguageConfig::Cpp23)` 没有启用 MSVC 或 Clang vendor，
`__try` 仍被建成 `TryStat`，`^...` 仍被建成 `LambdaExpr`。直接证据包括
[`parse_try_statement` 的 spelling 判断](../crates/cpp_parser/src/grammar/cpp/stats.rs#L1889)
和
[`Caret` 的 block literal 分支](../crates/cpp_parser/src/grammar/cpp/exprs.rs#L2927)，二者
没有 `Feature::StructuredExceptionHandling` / `Feature::BlockLiteral` gate。

同类风险还包括：

* `<=>` 在 lexer 中直接生成 `Spaceship`，没有读取
  `Feature::ThreeWayComparison`。
* `Feature::Char8T`、`AutoC`、多个 vendor feature 虽然定义在配置表中，却没有对应 grammar
  gate。
* `CppLanguageLevel`、`LexerConfig::language_level`、`LanguageConfig` 和 `Dialect` 同时存在。
  后两个可以表达不同标准+vendor，但前两个仍是公开可读且可能陈旧的状态。

另外，当前 builder 是顺序敏感的：

```rust
ParserConfig::default()
    .with_dialect(dialect)
    .with_language(language)
```

实际索引路径正是这个顺序，例如
[`FileIndexer::index`](../crates/cpp_code_analysis/src/index/mod.rs#L300)。但
[`with_dialect`](../crates/cpp_parser/src/parser/parser_config.rs#L173) 修改
`lexer_config.language`；随后
[`with_language`](../crates/cpp_parser/src/parser/parser_config.rs#L189) 用整套 bits
覆盖它。于是：

* `/std:c++20` 只得到标准 bits 时，之前选定的 MSVC vendor 可能被清除。
* `Dialect` 字段仍保留 MSVC，少数由 `Dialect` 驱动的规则与多数由 `LanguageConfig` 驱动的
  规则可能看见不同目标。
* 反过来，仅对默认 config 调 `with_dialect(Msvc)` 也不会从默认的“所有 vendor”
  配置中删掉 GNU/Clang。

现有测试只固定了 `with_language(...).with_dialect(...)` 的一种顺序，没有固定公开 API
的顺序无关性。

建议：

1. 只保留一个权威配置对象；让旧 `level` / `language_level` 成为计算 getter，或明确标记
   deprecated。
2. `with_dialect` 和 `with_language` 都只能改变自己的语义切片，且对任意链式顺序幂等。
3. 每个 syntax extension 在入口处读取同一 `Feature`；不要只在 token 形状上判断。
4. 增加“strict std 只接受标准语法”的负向矩阵，GNU/MSVC/Clang 扩展分别用正向和负向配置测试。
5. 将 Clang 单独建模，不要长期折叠成 GNU；两者共享一部分后端能力，但语法扩展不同。

### P1-3：C++26 `contract_assert` 无条件抢占了 C++23 标识符

[`name_to_kind`](../crates/cpp_parser/src/lexer/cpp_lexer.rs#L247) 无条件把
`contract_assert` 变成 keyword。最小合法 C++23：

```cpp
int contract_assert = 1;
```

当前严格 C++23 解析为 `expected primary expression`，并且错误节点会进一步吞住后续声明。
`pre` / `post` 是 contextual，保留了旧代码；`contract_assert` 不是，因为它在 C++23
仍然可以是普通标识符。

这和反射使用的 `^^`、splice token 不同：那些 token 在旧标准通常根本没有有效表达式解释；
这里存在真实的关键词/标识符兼容冲突。注释中“没有更早标准含义，所以无需 gate”的判断对
identifier keyword 不成立。

建议增加 C++26 mode（至少增加 `Feature::Contracts` / `Standard::Cpp26`），并且只在该模式
下将 `contract_assert` 提升为 keyword。严格 C++23 的负向测试必须检查变量、函数名和调用
三种标识符位置。

### P1-4：事件流恢复 fallback 与承诺不一致

[`balance_events`](../crates/cpp_parser/src/syntax/tree/cpp_tree_builder.rs#L168) 的注释
明确说未闭合节点应在“真实结束处”插入 `NodeEnd`，不能追加到 EOF；实现末尾却对每个
open node 直接执行：

```rust
events.push(MarkEvent::NodeEnd);
```

见 [实现](../crates/cpp_parser/src/syntax/tree/cpp_tree_builder.rs#L219)。

这会把 fallback 变成原本注释明确反对的行为：未闭合的前一个节点可以吞掉剩余文件。
debug build 有 marker stack assertion，测试也很强调 event audit，但用户运行的 server 通常是
release build，且 parser 的契约是“任意输入仍得到可用树”。一旦新语法或 recovery 遗漏再次
触发泄漏，这个 fallback 会扩大损伤。

建议：

* 先使 `balance_events` 真正推导 open node 的最晚局部结束位置，或在 `CppParser::recover_to_level`
  完成一致的 eager closure。
* 至少把 `EventStreamAudit` 的 unbalanced 结果从仅测试 API 变成 release 可观测指标。
* 增加“故意制造未闭合 inner node，随后声明必须保持 sibling”的 builder 单测，不依赖 grammar
  已经没有泄漏。

### P1-5：`CppSyntaxId` 反序列化可 panic

[`CppSyntaxId::deserialize`](../crates/cpp_parser/src/syntax/id.rs#L139) 在 malformed
cache / IPC 输入下调用：

```rust
CppKind::from_raw(kind)
TextRange::new(...start, ...end)
```

`from_raw` 对不在有效 discriminant 范围的 kind 会 panic；倒序 range 也没有先验证。
序列化格式是公开 `serde` 契约，反序列化失败必须返回 `D::Error`，不能使 reader 进程退出。

建议：

* 使用 `CppKind::try_from_raw` 并映射成 `serde::de::Error`。
* 校验 `start <= end`。
* 加 malformed hex、out-of-range kind、反向 range、超长输入测试。
* 格式中加入 parser/version/schema，避免 kind 数值跨版本静默改变含义。

## Toolchain：P1 问题

### P1-6：include graph 的宏可见性不是按位置演进的

[`visit`](../crates/cpp_toolchain/src/include/graph.rs#L540) 虽然顺序应用
`#define` / `#undef`，但在循环开始前调用了一次
[`directive_visibilities`](../crates/cpp_toolchain/src/include/graph.rs#L592)，把整个
文件的条件结果都从同一个 inherited state 一次算完。随后循环只修改 `macros`，并不会更新
已经存好的 visibility。

这会直接破坏最基本的编译顺序：

```cpp
#define FEATURE 1
#if FEATURE
#  include "feature.h"
#endif
```

`#if FEATURE` 仍可能按 inherited state 中“未定义”来决定，`feature.h` 从 graph 中消失。
反过来，`#undef FEATURE` 后的 `#if FEATURE` 仍可能看到 define。

问题不只影响 include edge。condition、guard、macro environment、cache completeness 共享
这条状态链，因此错误的 visibility 会继续影响 cooker 选择的分支和后续 parser facts。
源码注释也承认这里原本需要逐位置 state，当前只是“先用一份诚实 state”绕过了成本。

建议把 directive walk 改成真正的状态机：

1. 每个 directive 在当前 `Marked` 上决策。
2. `#define/#undef` 立即更新 state。
3. 条件分支的 open-region stack 保存每个 arm 的 active / inactive / unknown。
4. 条件决策只保留当前位置，不为后续 directive 复用过时 verdict。

若性能回归，就维护从 position 到小型 macro mutation journal 的版本，而不是预先固定所有
verdict。

### P1-7：unknown、未解析 include 没有完整传播不确定性

`Marked` 已经提供
[`mark_incomplete` / `mark_uncertain`](../crates/cpp_toolchain/src/include/graph.rs#L1144)
，说明设计层理解三值状态；但 include graph 没有在所有必要点使用它。

当前有三条具体路径：

1. visitor 只跳过 `Some(false)`，`None` 可见性里的 `#define/#undef` 被当成确定事实应用。
2. unresolved include 只记录到
   [`graph.unresolved`](../crates/cpp_toolchain/src/include/graph.rs#L624)，没有把当前
   `macros` 标成 incomplete。
3. resolved 但 unreadable 的
   [`follow`](../crates/cpp_toolchain/src/include/graph.rs#L935) 返回 `None` 后，调用者
   继续保留原 macro state，也没有标记未知。

结果是：文件可能定义某个 macro，但 walk 只因为没有读到而把名字当作确定未定义；后续
`#ifdef` / `#if` 可能得到“有把握但错误”的 true/false。

建议：

* unknown branch 中的 define/undef 使用 `mark_uncertain(name)`。
* unresolved/unreadable include 调 `mark_incomplete()`。
* `incomplete` 的 FileEntry 不只记录 root 是否缺上下文，也记录 walk 在当前点之后新产生的
  缺口。
* 测试“unknown branch 定义 macro”、“缺失 include 后 `#ifdef`”、“无权限文件后 `#ifdef`”
  三类输出必须是 `Unknown`，不能是 confidence-shaped false。

### P1-8：guard 被当作 visit-once，而不是“当前状态是否阻止访问”

[`decide_visit`](../crates/cpp_toolchain/src/include/graph.rs#L894) 只检查文件是否已经被
visited 以及 `entry.guard.is_guarded()`：

```rust
if guarded && state.visited.contains(&file) {
    return Skipped(AlreadyVisited);
}
```

它没有读取当前 `Marked`。这和
[`Guard::blocks_a_visit`](../crates/cpp_toolchain/src/preprocess/guards.rs#L33) 的公开
契约矛盾：macro guard 只有在 guard macro 当前仍定义时才阻止第二次 visit。

```cpp
#include "guarded.h"
#undef GUARDED_H
#include "guarded.h" // 编译器会再次读取
```

当前 graph 仍跳过第二次 include。

另一个独立问题是
[`has_pragma_once`](../crates/cpp_toolchain/src/preprocess/guards.rs#L170) 对所有 directive
做纯语法扫描：

```cpp
#if 0
#  pragma once
#endif
```

也会被记录成 `Guard::PragmaOnce`，但 inactive pragma 不应使后续 include visit-once。

建议：

1. 把当前 macro state 传给 `decide_visit`，macro guard 调 `blocks_a_visit`。
2. `#pragma once` 只在 active directive 位置计入 guard。
3. 重复 include 的 graph edge 既要记录，也要保留“这次是否真的读取”的区别。
4. 增加 macro guard 被 undef、同名 guard 宏来自别的文件、inactive pragma 三个回归测试。

### P1-9：推断的 `include/` 根不应参与 angle include

[`IncludeResolver::resolve`](../crates/cpp_toolchain/src/include/mod.rs#L210) 的注释明确说
向上推断出的 `include/` 仅服务 quoted include，因为它必须不能抢占 `<vector>`。
实现却无条件遍历
[`include_roots_above`](../crates/cpp_toolchain/src/include/mod.rs#L230)，没有
`IncludeForm::Quote` 判断。

结果是一个没有 compile database 的项目，只要祖先目录有 `include/vector`，就可能把系统
`<vector>` 解析到项目文件，产生整棵错误 include closure 和错误 macro table。

建议加 guard：

```rust
if include.form == IncludeForm::Quote && skip_through.is_none() {
    // local + inferred project roots
}
```

并用相同 spelling 同时存在于 system path 和推断根中的 fixture 固定“angle never shadows
system”。

### P1-10：MSVC 常用编译参数没有被读取

[`config_from_arguments`](../crates/cpp_toolchain/src/include/config.rs#L700) 支持
GNU `-I/-D/-U` 和 `/std:`，但没有读取：

* `/Ipath`、`/I path`
* `/DNAME=value`、`/D NAME=value`
* `/UNAME`、`/U NAME`

这会让 `cl.exe` compile database 同时丢失项目头文件目录和 feature macro。Windows 项目不
只是标准库路径会错，条件块、platform header 和 generated config header 也会错。

建议与 GNU 路径共享一个 option parser，支持 MSVC 的 joined/separated spellings，并补
compile database 回归测试。注意不要把 `/I-`、`/external:I` 一概同化，先记录明确支持集合。

## Toolchain：P2 问题

### P2-7：target-level command-line 字段被解析但没有应用

[`TargetSection`](../crates/cpp_toolchain/src/project/config.rs#L355) 接受
`args`、`extra_args`、`remove_args`，文档语义与项目级
`[compile] args` 相同；但
[`CompilerConfig::for_file`](../crates/cpp_toolchain/src/include/config.rs#L430) 在 target
匹配后只应用 `includes`、`defines`、`standard`。

例如：

```toml
[[target]]
args = ["-Igenerated", "-DFEATURE=1"]
```

会被成功反序列化但没有任何效果，属于“配置成功但结果未变”的静默 API 缺陷。

建议复用项目级的 `args -> remove -> extra -> config_from_arguments` 顺序；target 的
`args` 替换、`remove_args` 删除、`extra_args` 追加必须在合并 include/defines 的同一位置
固定并测试。

### P2-8：相对 `compile_commands.json` 的 `file` 没有按 `directory` 归一化

`build_database.json` 的
[`parse_translation_unit`](../crates/cpp_toolchain/src/include/config.rs#L931) 已经把相对
`source` 与 `work-directory` 拼接；`compile_commands.json` 的
[`parse_command_object`](../crates/cpp_toolchain/src/include/config.rs#L1251) 却直接保存
`file`。

这不会只改变显示路径。header borrowing 的
[`a_command_for_a_header`](../crates/cpp_toolchain/src/include/config.rs#L1132) 比较
command 文件的 parent 与目标 header 的 parent；相对 command 路径不能得到相同目录，从而
无法借到源码旁的 flags。

建议在两种数据库格式的入口统一执行“相对 path + directory”归一化；增加
`directory = /p/build, file = ../src/a.cpp` 和 header borrowing 的测试。

### P2-9：大小写语义硬编码为 OS，而不是 filesystem

[`FileProvider::is_case_insensitive`](../crates/cpp_toolchain/src/paths.rs#L73) 的默认值是
`cfg!(windows)`。源码注释也承认 Windows 可以挂载 case-sensitive volume、macOS 常见
case-insensitive volume，实际行为却仍按平台猜测。

同时 compile database matching 的
[`normalize_for_comparison`](../crates/cpp_toolchain/src/include/config.rs#L1194) 总是
传 `true` 做 case fold。Linux 上 `Foo.cpp` 与 `foo.cpp` 可能是两个 TU，却可能选中同一
command。

建议：

* `DiskFiles` 在启动/项目根 probe volume case policy，或从配置显式覆盖。
* include/path interner 使用 provider 给出的 policy。
* compile command matching 先按当前 provider/filesystem 的精确规则，再只在“读取跨机器
  database”的明确 fallback 中 case-fold。
* 覆盖 Windows case-sensitive directory、macOS case-insensitive volume，以及 Linux 大小写
  同名双文件。

### P2-10：`-m32 / -m64` 没有传给编译器 discovery

[`config_from_arguments`](../crates/cpp_toolchain/src/include/config.rs#L817) 会保留
architecture target，但 compiler discovery 的
[`search_list_flags`](../crates/cpp_toolchain/src/include/toolchain.rs#L734) 只转发
target/sysroot/toolchain march 等，不转发 `-m32/-m64`。

32-bit 项目会查询 native-architecture compiler：

* system include 搜索顺序错误；
* predefined macro table（指针大小、ABI、架构 feature）错误；
* 后续 conditions 选择错误分支。

建议把 `-m32/-m64` 加入 `search_list_flags` 的白名单，并用 fake compiler runner 固定
query argv。若工具链是 MSVC，则同时检查等价 `/favor`、target arch 是否来自编译器
命令，而不是从 host 猜。

### P2-11：header-name 与 quoted path 仍是跨 crate 的重复实现

Parser 的 header-name folding、toolchain 的
[`fold_quoted_header_names`](../crates/cpp_toolchain/src/preprocess/directive.rs#L719)
和 lexer 的独立 scanner 使用不同规则，quoted path 中的 `\` 都会导致不 fold。
这意味着 `#include "sub\header.h"` 甚至可能没有 graph edge。

这与 Parser P2-2 是同一个问题，不应在两个 crate 各打局部补丁。header-name grammar 必须有
一个共享接口；toolchain scanner 不能再复述“no escapes”规则。

## Toolchain：明确但未完成的能力

以下项源码已经注明，适合进入 backlog，但不应被当作已经完成：

1. **Per-file `-D/-U` 尚未用于 macro seed。**
   `CompilerConfig::for_file` 目前只替换 per-file include paths，定义仍是 project-wide seed。
   多 target 的同名 header/源码可能继承另一个 TU 的宏状态。
2. **`#include_next` 没有保留 origin。**
   graph 调 `resolve(..., None, ...)`，wrapper header 可能再次找到自己或错误 header。
3. **Macro target include 和外部模块/BMI metadata 不受支持。**
   `#include HEADER` 无法扩展 target；`build_database.json` 的
   `provides/requires/visible-sets` 仍未进入模块图。
4. **未知 compiler option 的词法覆盖很窄。**
   `-imacros`、`-include`、MSVC `/FI`、forced include、`/external:I`、clang
   `-Xclang`/`-isysroot` 等会继续造成配置漂移，应逐条建成熟 option model，而不是在多个
   调用点添加 string prefix。

## Parser + Toolchain 的联合风险

### 1. “解析正确”和“配置正确”必须组合测试

Parser 单测的 strict config 测试与 toolchain 的 include/config 测试分别通过，仍可能漏掉
这条真实路径：

```text
compile database
  -> CompilerConfig::language_for
  -> ParserConfig builder
  -> parser feature gate
```

应为每个 vendor 至少增加一个端到端 fixture：

* GCC/Clang `c++20` + GNU/Clang extension；
* MSVC `/std:c++20 /I... /D...`；
* strict `c++20` 负向 rejection；
* C/C++ mixed workspace；
* 32-bit target。

### 2. uncertainty 必须贯穿 graph、macro expansion、parser facts

`Unknown` 不能只存在于 condition evaluator。include 未读、条件未决、guard 不确定时，结果
至少要让 parser facts / inactive-region / cache key 都能表达“不完整”，而不是把 `None`
压成 empty set。

### 3. 预处理源码映射和 CST 坐标都要使用同一 origin

Header name fold、`parse_tokens`、宏展开 spans、rendered text 与 file boundaries 都在移动
坐标。建议把“原文件 absolute range”与“rendering relative range”设计成不同类型，避免
某层只因 `usize` 相加相减而把坐标系混用。

## Parser：P2 问题

### P2-1：大量固定 lookahead 窗口仍被当作语法边界

源码中存在 `64 / 96 / 128` token 扫描。它们常常不是性能预算，而是语法判定本身：

* C-style / range `for`、cast、lambda、requires-expression、placement `new`、using
  alias、template id 等。
* [`a_qualified_name_is_followed_by_an_equals`](../crates/cpp_parser/src/grammar/cpp/decls.rs#L5489)
  的注释说 `=` 可能“任意远”，实现却只看 96 token。
* [`an_operand_follows_the_parentheses`](../crates/cpp_parser/src/grammar/cpp/exprs.rs#L1386)
  等规则把 128 当作“没找到匹配括号”。

对于宏生成代码、很长 template argument、attributes 和自动生成的参数列表，超过窗口会返回
`false`，而不是“需要更完整判断”；于是合法语法被静默选择成另一棵树。

建议：

1. 对 delimiter matching 使用无固定窗口的 significant-token iterator，并在文件总 token 数上
   保证终止。
2. 如果确实需要 DoS budget，达到 budget 时返回 `Unknown`，再走 bounded speculation /
   recovery，不要等价于负结果。
3. 为每个 64/96/128 临界点增加 `window - 1 / window / window + 1` 测试。
4. 对频繁看一个 token 的路径提供 `token_at(relative)`，避免反复分配
   `Vec<CppTokenKind>`。

### P2-2：header-name 有两套不一致实现

Parser 的 [`try_lex_header_name`](../crates/cpp_parser/src/parser/cpp_parser.rs#L913) 与
lexer 的 [`lex_header_name`](../crates/cpp_parser/src/lexer/cpp_lexer.rs#L1279)
重复实现：

* parser folding 在 angle form 可接受 `;` 等字符；独立 lexer 的测试明确拒绝 `<a;b>`。
* 独立 scanner 和 parser folding 都拒绝 quoted path 中的 `\`，但 header-name token 内的
  backslash 没有普通 string literal escape 语义，Windows 项目确实可能使用
  `#include "dir\file.h"`。
* 两条路径分别折叠 token stream 与字符 stream，后续必须靠测试保持同步。

建议把 header-name 扫描规则提炼成一个接收 range/token boundary 的组件，并覆盖：

```cpp
#include "dir\file.h"
#include <name;part.h>
#include MACRO
```

其中第三项必须仍能回退为普通 preprocessing token（不是强行 header-name）。

### P2-3：`LineIndex` 只以 `\n` 建立行

[`LineIndex::parse`](../crates/cpp_parser/src/text/line_index.rs#L28) 只在 `\n`
处推行偏移；lexer 的 `eat_newline` 同时接受 `\r\n`、裸 `\r` 和 `\n\r`。含裸
`\r` 的文件会被 lexer 当成多行，却被诊断位置、LSP range 和 line body 当成一行。

建议统一“逻辑换行”定义，至少覆盖 `LF / CRLF / CR`，并用 parser lexer 与 `LineIndex`
共享同一个迭代器测试。

### P2-4：`parse_tokens` 是公开 API，却没有验证 token range

[`parse_tokens`](../crates/cpp_parser/src/parser/cpp_parser.rs#L549) 接收调用者给出的
任意 token/range。token range 越界、重叠、逆序、落在 UTF-8 字符中间或不覆盖同一 `text`
时，多个切片和转换会直接 panic。

同时语法节点 range 从 token 长度重新构造、相对 slice 从 0 开始，而
`CppTokenData::range` 保持原始绝对坐标。typed token accessor 再从 red tree range 重建
`CppTokenData` 时也可能丢失这个 anchor。

建议：

* 在公开入口验证单调、非空、字符边界、全部落在 text 内。
* 无效输入返回结构化错误，而不是让 safe API panic。
* 引入明确的 `ReadingRange { absolute_anchor, relative_range }` 类型，避免一个 `usize`
  同时表示原文件和 parse slice 坐标。
* 用 `Arc<[CppTokenData]>` / `Arc<[CppParseError]>` 替代 `CppSyntaxTree` 的深 clone，
  当前 `#[derive(Clone)]` 会复制整条 token vector。

### P2-5：kind discriminant 是隐式 wire format

[`CppKind::try_from_raw`](../crates/cpp_parser/src/kind/mod.rs) 依赖 fieldless enum
连续 discriminant，并用上界 transmute。源码通过“新 variant 必须 append”维持这个约束；
`CppSyntaxId` 又直接序列化数值。

它目前由测试和 `VERSION` cache key 保护，但仍存在几个维护风险：

* 中间插入 variant 是源码上很自然的操作，却会改变持久化含义。
* 旧 `Lua` 时代的 dead kinds 仍占据连续编号，证明 kind 表已经承载兼容包袱。
* `try_from_raw` 的 soundness 建立在所有 discriminant 连续且没有显式值的组织约定上。

建议用显式稳定 `Kind::wire_id()` / `from_wire_id()`（可生成），或 `num_enum` 一类 derive；
为序列化 ID 携带 schema version。所有 variant 的双向遍历测试应覆盖“当前全部集合”，而不只
边界。

### P2-6：已知 grammar 缺口仍需显式产品化

当前 `tests/gaps.rs` 很好地把缺口集中化了，仍明确未支持：

* function-try-block：`void f() try {} catch (...) {}`。

除此之外，C++26 contracts/reflection、module import 目前主要是语法形状，不能替代预处理器、
模块图、语义 name lookup 或编译器诊断。文档/产品层应继续明确三类能力：

1. `recognises syntax`；
2. `resolves a target/definition`；
3. `matches compiler acceptance and diagnostics`。

不要把第二、三类能力隐含在“parser 支持”一词下。

## Parser：P3 / 长期补强

### P3-1：增量重解析

Parser 当前完整重 lex / parse，README 也明确没有 incremental reparse。`parse_tokens`
避免了重复 lex，但尚未形成 editor edit path，language server 在每次 edit 后仍承受整文件
grammar、event stream 和 green tree 构建。

建议先做低风险分层：

1. 复用 token stream，对 edit 后受影响行/括号预算做局部 re-lex。
2. 以 declaration / block 为重解析边界，保留未受影响的 green child。
3. 对跨编辑的 scope/macro/directive 依赖保留失效规则，而不是先做细粒度 token 替换。
4. 把 latency、peak allocation 和 cache hit 一起作为验收指标。

### P3-2：测试体系缺“严格配置负向测试”和并发/平台维度

现有基线测试全部通过：

```text
cpp_parser:   42 unit + 503 integration
cpp_toolchain: 148 unit + 21 integration
```

优点是 lossless、prefix/deletion invariant、shape regression 和真实头文件病例都很扎实。
仍建议补：

* 每项 standard/vendor feature 的正向+负向矩阵。
* malformed serialized ID / invalid token stream。
* 超过 lookahead 阈值的输入。
* 真正的 fuzz target（现有 prefix/deletion corpus 不是任意 token stream fuzz）。
* 一个 nightly/weekly `clang`, `gcc`, `cl` preprocessor/parser differential job。
* Windows/macOS 路径、大小写、`CR`、Unicode、symlink/canonicalization 的多平台测试；
  当前 test job 只跑 Ubuntu，build matrix 会编译其他平台但不执行测试。

### P3-3：清理失效文档引用和旧 Lua-era 源码

源码和测试仍引用仓库中不存在的：

* `docs/parser-review.md`
* `docs/parse-architecture.md`
* `docs/buglist.md`
* `reference/README.md`

同时 `cpp_syntax_kind` / typed AST 中仍有 Lua 时代的兼容命名和未接线的
`syntax/traits.rs`。这些不会是本轮 correctness P1，但会继续提高理解成本。建议删除死文件，
或把兼容层明确放到 `legacy` 模块并禁止新增依赖。

## 建议推进顺序

### 第一阶段：先消除静默错误答案

1. 修复 `TypeNames` scope/shadow 模型。
2. 将 include graph directive walk 改成逐位置 state machine，并传播 unknown/incomplete。
3. 修复 guard 的 macro-state 判断和 inactive `#pragma once`。
4. 统一 `ParserConfig` builder 顺序，完成端到端 standard/vendor matrix。
5. gate `contract_assert`，保证 C++23 identifier 不回归。
6. 修复 `balance_events` fallback，并让 unbalanced audit 在 release 可观测。

验收标准不只是“测试通过”：

* 合法 callable 不再变成 declaration。
* local `#define/#undef` 改变的 include edge 与真实编译器一致。
* unknown/include-gap 不得产生确定 false。
* strict / vendor 配置的同一源码得到不同且可解释的树形。
* 任何 serializer 输入错误都返回 error，不能 panic。

### 第二阶段：补齐配置与跨平台边界

1. `/I /D /U`、target args 和 per-file defines。
2. 统一 header-name reader（含 Windows backslash）。
3. inferred root 只服务 quoted include。
4. compile command relative path、filesystem case policy、`-m32/-m64`。
5. `#include_next` 和 macro target include。

验收应包含 compile database 端到端 fixture，而不只是单独调用 option parser。

### 第三阶段：公共 API、性能和长期覆盖

1. `parse_tokens` 输入验证与坐标类型化。
2. kind schema version / stable wire id。
3. 固定 lookahead 替换和 boundary tests。
4. fuzz / compiler differential / 跨 OS CI。
5. 在前三类 correctness 指标稳定后再推进增量重解析。

## 值得保留的设计

以下方向应该保留并继续加强：

1. **Lossless CST 是正确语言服务器基石。** token、trivia、comment、directive 都回放到树，
   source mapping 依赖这个不变量。
2. **Marker + checkpoint 分离了“节点 nesting”和“token cursor”回滚。** `events`、错误、
   bracket/template state 共同回滚，是很多 parser 最容易漏的部分。
3. **错误恢复后检查结构，而不只检查是否有诊断。** “无 error 但被读成另一棵树”已经被多个
   shape tests 抓住。
4. **真实语法矩阵和 `gaps.rs` 不是样板测试。** 它让构造是否得到支持、得到什么 node 成为
   可审查状态。
5. **typed AST 的 accessor 全部返回 `Option`**，适合编辑中的半成品代码，不应改成 panic 式。
6. **把 parser 的版本纳入 cache key** 是必要方向；下一步应把 serialized kind/schema 也纳入。
7. **`FileProvider` 把磁盘语义和 include 逻辑分开**，`MemoryFiles` / fake runner 使
   toolchain 行为可做 hermetic 测试。
8. **`Marked` 已经有 incomplete / uncertain 三值语义**；当前问题主要是 wiring 不完整，而
   不是缺少表达能力。
9. **编译数据库、项目 config、compiler discovery 的责任边界清楚**，只需把 option model 和
   组合顺序修正，不需要推倒重写。

## 验证记录

本轮未修改 parser/toolchain 行为，只生成审查文档。验证过：

```text
cargo test -p cpp_parser -p cpp_toolchain
  cpp_parser    42 unit + 503 integration passed
  cpp_toolchain 148 unit + 21 integration passed
```

另外用两个临时最小输入确认：

* 前一函数的局部 typedef 会把后一函数的 callable `Leaked()` 解析为 declaration；
* strict C++23 仍接受 MSVC `__try` 和 Clang block literal，并把普通变量
  `contract_assert` 解析为 C++26 keyword。

这也说明现有 suite 能覆盖稳定 invariant，但对“配置严格性 + CST 语义形状”的组合回归覆盖
仍不足。
