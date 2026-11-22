# `cpp_code_analysis` 实现审查

审查基线从 `091fc1ba` 开始，并直接读取现有工作区源码。审查范围是
`cpp_code_analysis` 的 file/view、index/cache/session、scope/name/type、completion、
diagnostic、module 与跨文件查询；parser/toolchain 本身的结论沿用前一轮审查，不重复展开。

审查期间 DeepSeek 同时修复上一轮 parser 发现，因此 worktree 又出现了 parser/C++26
相关修改；其中 `cpp_code_analysis/src/session/mod.rs` 只补了 `__cplusplus` 的 C++26
值，未改变本文涉及的 lookup/type/cache 路径。本文不依赖这些并行改动，且未修改任何实现
文件。

## 总体评价

这是一个很强的“编辑器语义引擎原型”，不是一个缩小版 C++ 编译器。

它最优秀的部分是工程结构：

- `Known<Yes / No / Unknown>` 对不确定性建模明确；
- token、rendering、source span、written/report offset 的坐标体系完整；
- summary/index/cache/session 分层清楚；
- 大量性能 measurement 被写在对应代码旁，而不是只写在 benchmark；
- 诊断倾向于少报、保持 `Unknown`，而不是拿启发式结论冒充事实。

不足集中在“语义事实的粒度”：

- 名字可见性多以“文件 closure 中曾经出现”为单位，而不是“当前 offset / 当前
  translation unit / 当前预处理状态”；
- type 主要是 spelling + 少量形状，`const/volatile`、literal 后缀、value category 和
  overload 规则都存在明显压缩；
- raw file summary、cooked file、program-unit reading 会同时存在，查询选择路径仍不完全
  一致；
- 磁盘 cache 的身份有些以 64-bit hash、closure 或 file-self environment 表示，仍有跨
  translation unit 和 stale filesystem 入口。

如果目标是“可用的 C/C++/CUDA LSP，宁可少答也不乱答”，当前实现已经明显高于脚本式
language server。若目标是“结果要和编译器语义一致”，目前还不应这样宣称。

### 建议评分

| 维度 | 评价 |
| --- | --- |
| 架构与数据流 | 8.5 / 10 |
| 增量索引与缓存设计 | 8 / 10 |
| 作用域/名称可见性准确度 | 6 / 10 |
| 类型/overload 准确度 | 5.5 / 10 |
| 诊断可靠性（precision） | 8 / 10 |
| 诊断完整度（recall） | 5 / 10 |
| 可维护性 | 6 / 10 |
| 作为编辑器引擎 | 适合继续投入 |
| 作为编译器替代 | 不适合 |

## P1：会影响正确结果

### P1-1：名称解析没有统一的“program point”

跨文件查询的核心参数仍是 `name + visible_from(file)`，不是
`name + file + offset + reading/program`。例如：

- [`definitions`](../crates/cpp_code_analysis/src/index/project/macro_walk.rs#L2987)
  只接收 `name` 和 `visible_from`；
- [`definitions_at`](../crates/cpp_code_analysis/src/sema/resolve.rs#L1166) 逐 scope
  搜索，但没有按 binding 的 declaration point 过滤；
- [`using_directive_targets`](../crates/cpp_code_analysis/src/sema/resolve.rs#L1351)
  明确收集文件内所有 `using namespace`，不区分 cursor 前后；
- [`global_names`](../crates/cpp_code_analysis/src/index/project/completions.rs#L1195)
  将 buffer 的全局名和整个 include closure 合并；
- completion 只对 function/block/lambda 的局部 binding 用
  [`declared_above`](../crates/cpp_code_analysis/src/index/project/completions.rs#L1087)。

这使 declaration/include/macro/using directive 的先后顺序丢失。当前实现可稳定复现：

```cpp
void f() { later(); }
void later() {}
```

`cpp_inspect define` 会从第一个 `later()` 跳到后面的声明。

```cpp
void f() { x = 1; }
int x;
```

completion 会在第一个函数体里提供后面声明的 `x`。

```cpp
namespace A { int secret; }
void f() { secret; }
using namespace A;
```

use 仍可经过后面的 `using namespace A` 找到 `A::secret`。

宏也有同样问题：

```cpp
#undef SECRET
void f() { SEC }
#define SECRET 1
```

completion 仍提供 `SECRET`。调用位置只读取
[`summary.macros`](../crates/cpp_code_analysis/src/index/project/completions.rs#L453)
和 direct includes，没有询问 cursor 处最后一个 define/undef。

#### 影响

- go-to-definition 可能跳到尚未声明的实体；
- completion 提供当前不能使用的名字；
- undefined-name/range query 可能在不同 feature 间得出不同答案；
- 一个声明被某个 later include 引入时，整个 file closure 都被视为已可见。

#### 建议

引入统一 `ProgramPoint`：

```rust
ProgramPoint {
    file,
    offset,
    program_reading,
    conditional_branch,
}
```

所有 lookup 都接收这个对象：

1. local/binding 要满足 `name_range.start < point.offset`，class member complete-class
   context 例外；
2. include 路径只展开 point 之前的 include event；宏 include 的 expansion 用 macro
   environment resolve；
3. using declaration/directive 按 point 筛选；
4. macro completion 走 `macro_definition(name, path, offset)` / positional macro
   environment，不直接遍历 `summary.macros`；
5. cache key 包含 program reading；可把 “at offset” 建成 timeline 查询，而不是每个
   request 重新扫描。

### P1-2：raw、cooked 与 program reading 没有统一选择规则

编辑器需要保留 `#if` 的两个分支，所以 raw scope 不应删除；但一个编译态查询不应无条件
让 raw fact 说话。

当前可复现：

```cpp
#if 0
struct Dead {};
#endif
struct Live {};

void f() { Dead d; }
```

即使走 `--cook`，`cpp_inspect define` 仍能从 `Dead d;` 跳到 `#if 0` 中的声明，
completion 也仍提供 `Dead`。

代码中确实存在两个冲突路径：

1. [`definition_across_files`](../crates/cpp_code_analysis/src/index/project/queries.rs)
   优先查询当前 raw scope；
2. [`visible_declarations_upto`](../crates/cpp_code_analysis/src/index/project/macro_walk.rs#L1744)
   在某些条件下 fallback 到 file summary；
3. 当前项目还有明确的红测试：
   [`a_file_read_as_a_program_needs_no_guard_filter`](../crates/cpp_code_analysis/src/session/tests/programs.rs#L138)
   被 `#[ignore]`。

这不是 `#if 0` 的一个特例。它还影响未知 branch、多个 TU 使用同一 header 时的宏环境，
以及 inactive declaration 对 undefined-name / completion / go-to-definition 的结论。

#### 建议

查询先决定 reading mode，不允许 downstream 再“fallback raw”：

- cursor 位于 active branch：只使用该 program 的 cooked facts；
- cursor 位于 inactive branch：只使用 written/raw facts，并标记 `Inactive`，这是
  编辑该 branch 时的阅读；
- branch 未决：返回 `Unknown` / 显式候选；
- cursor 在 active code 外部：不允许 inactive fact 成为候选。

也就是说，raw/cooked 是一个查询计划的选择，不应由各 feature 的 `or_else` 临时混合。

### P1-3：`volatile` 被模型改成 `const`

[`Type::Qualified`](../crates/cpp_code_analysis/src/sema/types.rs#L187) 只保存
`of: Arc<Type>`，没有保存 qualifier 是 `const`、`volatile` 还是
`const volatile`。

后果不只显示错：

- [`Type::is_const`](../crates/cpp_code_analysis/src/sema/types.rs#L260) 对任意
  `Qualified` 都返回 true；
- [`Display`](../crates/cpp_code_analysis/src/sema/types.rs#L653) 把任意 qualifier
  打成 `const`；
- parsing 先通过 `strip_trailing_qualifiers` /
  `split_leading_qualifiers` 丢掉 qualifier spelling，见
  [`parse_type_spelling`](../crates/cpp_code_analysis/src/sema/types.rs#L945)。

可复现：

```cpp
struct S {};
volatile S v;
void f() { v; }
```

类型结果是 `const S`，不是 `volatile S`。

下面这个 fixture 还能影响 member overload 选择：

```cpp
struct S {
    int f();
    double f() const;
    char f() volatile;
};
volatile S v;
void g() { v.f(); }
```

`v.f()` 的类型被推成非-volatile 的候选，而不是 `char`。

#### 建议

保留 qualifier 集合和位置：

```rust
CvQualifiers {
    const_: bool,
    volatile_: bool,
}
```

`Qualified` 继续保留 wrapper 结构，因为 `const T*` 与 `T* const` 的位置不同，但
wrapper 需要带上 qualifier 数据。`is_const`、`is_volatile`、display、conversion、
member overload filtering 必须分别读取。

### P1-4：literal 类型推断忽略后缀和 language standard

[`type_of_expression_inner`](../crates/cpp_code_analysis/src/index/project/type_of.rs#L427)
当前把：

- 所有 integer literal / char literal 都判为 `int`；
- 所有 floating literal 都判为 `double`；
- C++20 的 `u8"..."` 判为 `const char*`。

可复现：

```cpp
void f() {
    auto a = 1u;       // 当前回答 int
    auto b = 1ull;     // 当前回答 int
    auto c = 1.0f;     // 当前回答 double
    auto d = 1.0L;     // 当前回答 double
    auto e = u8"x";    // C++20 当前回答 const char*
}
```

这会继续污染 overload 选择：

```cpp
int  pick(int);
long pick(unsigned int);

pick(1u);
```

当前 literal 是 `int`，可以错误地选择第一个重载。

字符 literal 还同时受语言族影响：C 的普通 character constant 与 C++ 的普通 character
literal 规则并不相同；`L/u/U/u8` prefix 也需要独立类型。

#### 建议

1. 在 SemanticModel 中携带 `LanguageConfig`、target ABI 和 standard。
2. 写一个 `LiteralTypeParser`：radix、suffix、fixed-width suffix、encoding prefix、
   base candidate 均进入结构化结果。
3. integer literal 按 candidate-list 选择第一个能容纳值的类型，不能只看 suffix。
4. C++20 `u8` string 的 element 是 `char8_t`；C/C++/CUDA 的 character literal 规则
   分开。
5. string literal 至少保留 array extent；使用时再 decay 成 pointer。

### P1-5：include resolution 的“当前结果”会跨文件系统变化失效

[`SummaryStore::where_it_resolves_now`](../crates/cpp_code_analysis/src/index/store.rs#L3389)
先查：

1. session 内 `include_searches`；
2. persistent `INCLUDE_SEARCHES`；
3. 最后才真正调用 resolver。

但文件新增/删除时：

- [`forget`](../crates/cpp_code_analysis/src/index/store.rs#L3325) 不清除这些结果；
- [`invalidate`](../crates/cpp_code_analysis/src/index/store.rs#L3495) 也不清除；
- persistent table 的 key 不包含 filesystem generation 或被验证过的 candidate stamp。

实际 failure：

```text
1. #include "x.h" 先解析到 /sdk/x.h
2. 用户在 includer 目录新建 x.h
3. summary validation 调用 where_it_resolves_now
4. memory/DB 仍返回 /sdk/x.h
5. 旧 summary 被认为 valid，继续使用错误 header
```

反向删除 winner 时也一样。

这是 cache correctness，不是性能问题：一个 stale hit 会改变 macro、types、members 和
diagnostics 的整条 closure。

#### 建议

- 搜索答案只做 per-snapshot memo，并让每次 filesystem reconcile 增加 generation；
- persisted entry 保存 `candidates -> existence/stamp/generation`，命中时做受控验证；
- 文件创建/删除时删除相同 spelling/form/directory 的 memory rows，并使 persistent
  rows 失效；
- 增加“先 miss，再在更前目录创建 header，旧 session 与 warm session 都必须切换 target”
  的测试。

### P1-6：unit reading 的磁盘 key 没有携带真正的 importer environment

session 的 program read 已经在内存中按 `program id` 分别保存：

```text
insert_cooked_in_a_program(path, program, cooked)
```

但 [`commit_a_unit_reading`](../crates/cpp_code_analysis/src/session/units.rs#L1189)
同时调用 [`put_the_cooked_reading`](../crates/cpp_code_analysis/src/index/store.rs#L1972)。
这个 persistent key 使用
[`the_record_environment(path)`](../crates/cpp_code_analysis/src/index/store.rs#L1814)，
它从该 file 自己的 closure 重新算 environment，而不是从当前 root/program frame 算。

同一个 header 被两个 TU 包含时：

```cpp
// a.cpp
#define MODE 1
#include "shared.h"

// b.cpp
#define MODE 0
#include "shared.h"
```

`shared.h` 在内存可以有两份 cooked reading；落盘时却可能共享一个 file-self key。
下一次 warm start 中，哪个 translation unit 的 facts 被
`take_the_stored_refinements` 恢复，就可能取决于写入顺序。

#### 建议

- cooked record 的 key 使用实际 `program frame` 的 environment fingerprint；
- `program root -> frame env` 要进入 key，而不只是 file path 的 closure；
- 持久化 header 时记录 `program/config/sequence` provenance；
- 加双 root 测试：同一 header、不同 `#define`、同一 source hash，warm restart 后两边 facts
  都不能串。

### P1-7：prepare / commit 没有磁盘 snapshot 绑定

[`prepare_wave`](../crates/cpp_code_analysis/src/session/pump.rs#L684) 保存的是 session
revision；[`index_one`](../crates/cpp_code_analysis/src/session/indexing.rs#L164) 提交时
再 `vfs.load(path)`，最后记录一个新的
[`FileStamp::of`](../crates/cpp_code_analysis/src/file/stamp.rs#L26)。

竞态：

```text
prepare 读取 A 版本并解析
磁盘文件变成 B（watcher 丢事件）
commit 把 A summary 放进 index
commit 又从磁盘记录 B 的 fresh stamp
```

reconcile 看到 stamp 已经是 B，于是不会要求重读，session 可能长期持有 A summary + B
text。`FileStamp` 同时只由 `(mtime, size)` 构成；同尺寸且 mtime coarse 的 rewrite 还有
第二个漏检路径。

#### 建议

- `Prepared` 携带自己的 text hash、stamp 与 filesystem generation；
- commit 前校验当前 VFS text 与 prepared text/hash 一致；
- 不一致则 discard/requeue，不记录新 stamp；
- stamp 需要 content cheap fingerprint、inode/change time 等额外字段，或在 reconcile
  对不可信时间粒度做抽样 content hash。

## P2：近期应修

### P2-1：parameter / template parsing 过度依赖二次字符串拆分

类型已经有 AST 和 `Type` 结构，但 overload 层仍从缓存的 parameter-list string 中重新
切类型：

- [`a_type_without_its_name`](../crates/cpp_code_analysis/src/sema/overload.rs#L513)；
- [`parameters_in`](../crates/cpp_code_analysis/src/sema/overload.rs#L593)；
- [`defaults_in`](../crates/cpp_code_analysis/src/sema/overload.rs#L541)。

例如：

```cpp
void f(unsigned long);
void g(const std::string);
void h(T x = S{1, 2});
```

第一个的最后一个 word `long` 会被当作参数名；第二个可能把 `std::string` 当作参数名；
第三个 braced initializer 里的 comma 不在 depth counter 的 `< > ( ) [ ]` 范围内。

这会影响 arity、default argument、overload ranking 和 signature help。

#### 建议

summary 直接记录结构化参数：

```rust
ParameterFact {
    type_of: Type,
    name: Option<SmolStr>,
    has_default: bool,
    pack: bool,
}
```

`parameter_list` 只保留展示字符串，不再重新从字符串推回语义。

### P2-2：overload resolution 是明确子集，不应显示为确定答案

[`overload::the_shape_of`](../crates/cpp_code_analysis/src/sema/overload.rs#L313)
主动删除 reference、top-level const/volatile，使：

```cpp
void f(int&);
void f(int&&);

f(lvalue);
f(rvalue);
```

很容易退化成同一 rank。当前 module 文档也明确没有完整 reference binding、
user-defined conversion 与部分 template ordering。

对于 diagnostics，这种 conservative `Ambiguous` 通常可接受；但 go-to-definition、
signature help、return type inference 会把 “没有决定” 变成少一个 answer 或第一个
candidate。

建议让 API 输出：

- `Known::Yes(unique)`；
- `Known::Unknown(Ambiguous)`；
- `RankedCandidates` / partial order。

Hover/signature 可以显示候选集，而不要在类型推断路径偷偷选第一项。

### P2-3：重复 namespace 同名段被硬折叠

[`ScopeTree::qualified_name_of`](../crates/cpp_code_analysis/src/sema/symbol.rs#L982)
和
[`enclosing_namespaces_of`](../crates/cpp_code_analysis/src/sema/declarations.rs#L4625)
都执行 `segments.dedup()`。

因此合法：

```cpp
namespace A {
namespace A {
void f();
}
}
```

会被记成 `A::f`，而不是 `A::A::f`。

这是为修复 stitched translation-unit 中 `std::std::...` 人为 leakage 而加的全局规则。
它把真实 C++ nested namespace 与 parser/file-boundary 修复混在了同一层。

建议：

- 不在 scope qualification 中无条件去重；
- stitched unit 的文件边界应在 parser/render layer 修复；
- 确实需要合并宏打开的 `std` 时，给 synthetic scope 带来源标记，并仅合并
  `MacroScopeReading` / unit-file seam。

### P2-4：`undefined-name` 不是完整的 undefined check

[`UndefinedName`](../crates/cpp_code_analysis/src/sema/check/undefined_name.rs#L61)
只检查：

1. member access 的 bare receiver；
2. `CallExpr` 的 bare callee。

普通使用完全不检查：

```cpp
void f() {
    missing = 1;
}
```

当前 diagnostics 为空。

这与防误报策略相符，但 `undefined-name` 的名称和用户预期更宽。建议：

- 在 diagnostic 展示和文档里改称 `unresolved-call-or-receiver`，或
- 增加一个 opt-in `strict-undefined-name`，只在 closure trusted 且 parser clean 时
  扫 ordinary identifiers。

### P2-5：VFS、cache handle 和磁盘 cache 都可随会话寿命持续增长

三个独立增长点：

1. [`Vfs::accounting`](../crates/cpp_code_analysis/src/file/vfs.rs#L273) 明确没有
   budget/eviction；除 open buffer 外，磁盘文件可全部保留。
2. [`open_handles`](../crates/cpp_code_analysis/src/cache/cache_db.rs#L983) 用进程全局
   `HashMap` 持有强 `Arc`，没有 workspace 关闭后的释放。
3. [`CacheDb::collect`](../crates/cpp_code_analysis/src/cache/cache_db.rs#L856) 只清理
   summaries/renderings/projections；include-search、cooked、environment、unit、
   contribution 等表没有完整 GC。idle path 只调用 collect，并不压缩。

#### 建议

- VFS 增 byte-budget LRU，open buffers pin；
- cache handle 在最后 session 关闭后 release；保留 single-writer 保证即可；
- GC 以“当前 generation 的 reachable rows”统一清扫所有表；
- collection 后在可获得 exclusive lock 时 compact，避免数据库只删行不缩水。

### P2-6：rendered span 映射仍以线性扫描为基础

[`FileView::reading_offset_of`](../crates/cpp_code_analysis/src/file/view.rs#L369)、
[`file_range_of`](../crates/cpp_code_analysis/src/file/view.rs#L304)、
[`was_written_here`](../crates/cpp_code_analysis/src/file/view.rs#L345) 都会扫描
`rendering` spans；completion 是高频调用方。

宏展开可能产生重复 written/reported ranges，普通数组二分不能直接服务所有问题，
但完全线性扫描也不必要。

建议在 `ParsedView` 构造时建立：

- cooked-start index；
- written-start / end index；
- reported-start / end index；
- macro-origin overlap chain。

每个 query 再按坐标空间选 index，而不是每次从第一项找。

### P2-7：scope / qualified-name 索引只保留第一个 reopened scope

[`ScopeTree::scope_with_qualified_name`](../crates/cpp_code_analysis/src/sema/symbol.rs#L1195)
背后的 map 对同一 qualified name 只插入 first。reopened namespace、anonymous scope 和
inline namespace 的合并需要查多个 scope；当前 query 会依赖 ProjectIndex 的额外事实
补救。

建议把 `by_qualified_name` 改成 `Vec<ScopeId>`，lookup 再按 program point、file 和
visibility 排序。

## 明确的能力边界

这些不一定是“bug”，但应在产品说明中明确：

1. **`auto` / `decltype(auto)` 函数返回推导仍是语法子集。** 多返回一致、branch、
   template instantiation、dependent conversion 无法完全覆盖。
2. **`auto x{...}` / braced initializer-list 的 deduction 未完整建模。**
3. **函数模板 deduction 只覆盖基础 pattern。** pack、nested type construction、
   dependent member、concept partial ordering 不完整。
4. **member completion/definition 对 class aliases、nested aliases、allocator traits 的
   深链可能退回 Unknown 或保留 template spelling。**
5. **module 解析仍以项目内 source scan + naming convention 为主。** 外部 BMI、build
   database module metadata、compiler module cache 不属于当前语义模型。
6. **diagnostics 追求低 false positive。** 当前 `missing/undefined` 类规则不等同于
   compiler diagnostic completeness。
7. **`Known::Unknown` 是正确结果的一部分。** 不能把 “没有 answer” 当缓存 miss 或
   user error。

## 值得保留的实现

1. **三值 `Known<T>` 是核心优点。** 它避免将 “不在当前分析集里” 写成 “不存在”。
2. **FileView 把 written/reading/report 的坐标显式分开。** 这比到处共享 `usize` 更可靠。
3. **SemanticModel 是 request-scope memo。** 它没有给跨编辑缓存增加 invalidation
   问题。
4. **SummaryStore 的 prepare/commit split 有实际价值。** parse 不长期持 write lock，
   这是 language server 交互性的关键。
5. **NameIndex、DeclarationShapes、ShapeTable、member list memo 减少了真实热点。**
6. **诊断先问 `Known`，Unknown 时保持安静。** 对编辑器而言，这是正确优先级。
7. **大量测量和失败样例写在源码旁。** 代码虽长，但很多行为有明确反例支撑。

## 建议推进顺序

### 第一阶段：统一一个 query-at-point

1. 为 definition、completion、hover、type、diagnostic 加 `program + offset + reading mode`。
2. 将 point-of-declaration、using directive、include event、macro state、conditional branch
   都放进同一个 visibility resolver。
3. raw/cooked 选择变成一次决策，不允许 feature-local fallback。

### 第二阶段：type truth

1. qualifier model；
2. literal model；
3. structured parameter fact；
4. value category / overload candidate set。

### 第三阶段：cache correctness

1. include-search filesystem generation；
2. cooked record 的 actual program environment key；
3. prepared disk snapshot；
4. VFS/cache LRU 和完整 GC。

### 第四阶段：compiler differential

最小集应该按 `clang++/g++/cl` 对比：

- use-before-declaration；
- include/macro before-after cursor；
- inactive conditional；
- `const/volatile/const volatile` member overload；
- suffixed integer / float / character / string literal；
- reference/rvalue reference overload；
- repeated namespace / inline namespace；
- same header under two macro environments。

## 测试与验证记录

当前全量基线：

```text
cargo test -p cpp_code_analysis
  889 unit passed, 3 ignored
  1133 integration passed, 9 ignored
  2 doctest ignored
```

其中至少有两个 `#[ignore]` 是已知的行为缺陷，而不是纯 probe：

- cooked program query 仍返回 conditional raw fact；
- undecidable macro branch 被当作 false。

本轮还用 `cpp_inspect` 固定了这些可复现行为：

- 后声明的函数/变量可被前面的 `define` / completion 找到；
- 后置 `using namespace` 对前面 use 生效；
- later/undef'd macro 被 completion 提供；
- `#if 0` 中的 `Dead` 在 active code 中仍可 resolve/completion；
- `volatile S` 被答成 `const S`；
- `1u` 被答成 `int`，`1.0f` 被答成 `double`；
- C++20 `u8"..."` 被答成 `const char*`；
- bare `missing = 1;` 没有 undefined diagnostic。

测试套件数量和测量质量都很高，但它目前更擅长防止已知回归，尚不足以证明与
compiler 的 point-of-use/type/overload 语义一致。
