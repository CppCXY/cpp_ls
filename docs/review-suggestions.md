# 代码审核与实现建议

> 范围:审核当前代码(`cpp_parser` ≈ 3.0 万行、`cpp_code_analysis` ≈ 4.6 万行、`cpp_ls` ≈ 0.8 万行,测试 ≈ 3.0 万行),
> 对照 [`plan-units.md`](./plan-units.md) 给出建议。
> 验证:`cargo test --workspace` **全绿**(analysis lib 512 通过;3 个用例因"需要 release profile"被 `#[ignore]`)。
> 生产代码里 `unwrap/expect` 共约 58 处、`panic!/unreachable!` 14 处、`unsafe` 4 处,整体克制。
> 说明:本文的结论来自读代码与跑测试,**没有**重新做性能实测;标注"待验证"的条目需要先写复现再动手。

---

## 已完成(第二轮:按本文建议落地)

| 项 | 做了什么 | 读数 / 证据 |
|---|---|---|
| **A3 名字倒排** | 新增 [`index/names.rs`](../crates/cpp_code_analysis/src/index/names.rs):`by_name` / `by_scope` / `macro_definers` 三张倒排 + 按小写名排序的 `sorted` 表;文件改用**永不复用的序号**(`order: BTreeMap<u32,_>`,`forget` 不再 O(N))。`ProjectIndex` 是唯一写者,`insert_at` / `forget` / `insert_cooked` / `forget_cooked` 同一调用里维护 | 合成 20 000 文件 / 52 万声明 / 12 万个名字(`examples/index_scale.rs`,release):`workspace/symbol "wid"` **266 ms → 0.07 ms**、`"run"`(8 万处声明)**160 ms → 0.07 ms**、`"ns7::Widget7_1"` **165 ms → 2 µs**;`files_declaring` / `declarations_in` 只遍历**可见文件**(按序号排序)而不是全部摘要;宏定义者查询 8 ns;一次编辑(forget + insert)0.4 ms |
| 正确性护栏 | 随机语料上的**对照测试**(`name_index_agrees_with_the_scan`):建库 → 熟读一半 → 忘记并重读 → 换 key → 忘记熟读 → 重复熟读 → 全部忘记,**每一步之后**把 `files_declaring` / `declarations_in` / `symbols_matching`(3 种 limit)/ `files_defining_macro` 与旧的全量扫描逐条对比 | 全部一致;另有 `names.rs` 的 6 条单测与"热门名字截断"测试 |
| 排序语义变化(**有意的**) | workspace symbol 同一档内由"按限定名字母序"改为"**按名字**、再按限定名、再按文件"——名字有序才能让搜索在拿够 `limit` 条时停下;声明超过 4 096 处的名字(`size`/`begin`)按文件序截断 | 旧测试里的期望顺序据此更新(`Widest` 在 `Widget` 前:同档、按名字) |
| **A1 依赖作废** | 新增 `DirectiveSignature`(`preprocess/directive.rs`):每个文件的**指令文本序列**(`environment`)与**含偏移**的版本(`layout`)。`invalidate_dependents` 只在 `environment` 变了时走,不再看"编辑**前**是否定义宏" | 3 条新测试:头文件**第一次**获得 `#define`、给无宏头文件**新增 `#include`` 都会使依赖者的熟读作废;旧判据两条都漏 |
| **A4(部分)/ 阶段 3 边界规则** | `units` 改为 LRU 表(上限 16);`units_for_the_pass` 的去重由 O(N²) 改为 `HashSet`;**函数体里打字不再清 `units`**(只有 `layout` 变了才清,即某条指令的位置或文本变了) | 测试 `a_unit_survives_typing_in_a_body_and_not_a_change_to_a_directive` |
| **A2 缓存回收** | `cache::prune`:删除本二进制**永远读不到**的分片(读 40 字节头:codec/format/`READING_FINGERPRINT` 不符)、过期 `.tmp`,再按 2 GiB 预算删最旧的;`Session::open` 起一个后台线程扫一次(测试用的 `with_config` 不启动) | 3 条新测试;真实 MSVC STL 工程上,旧二进制的缓存被新二进制启动时清掉 |
| 第二遍 pass 不再读全项目 | `re_read_what_a_body_changes` 在有单元时只读"被重解析的文件"与"定义了它们提到的宏的文件",候选由 `files_defining_any_macro` 查出;没有时间线的调用方(探针、测试)仍走原来的全量路径 | 真实 MSVC STL 工程(158 文件)A/B:与 HEAD **逐行一致**(147 行输出,含 `reused/rebuilt/unstored` 与全部 definition 表) |
| 文档 | `architecture.md` 视为已废弃:README / `Cargo.toml` 里对它和 `ls-architecture.md` 的引用已去掉,设计契约以模块文档注释为准 | — |

### 第三轮:冷启动并行(性能)

| 项 | 做了什么 | 读数(本机 6 核,MSVC STL 工程,158 文件,冷启动,release) |
|---|---|---|
| 摘要读取拆成 `prepare`(纯函数:读 → 哈希 → 查盘 → 解析 → 写盘)与 `commit`(写索引 + 计数) | `SummaryStore::prepare / commit / prepare_many`;`FileProvider` 加了 `Sync` 约束;摘要写盘的临时文件名改为每次唯一(相同文本的两个文件共享一个 key,并发写会互相覆盖) | — |
| `Session::advance` 按"波"并行 | 弹出一个还没准备好的文件时,把它和队列里排在后面的文件**一起**并行 prepare,再按原顺序逐个提交;所以索引看到文件的顺序、include 的发现顺序、`Step` 的内容都和串行时一致,只是等待被分摊到各核上 | 3.3 s → 2.25 s |
| 第二遍 pass 的重读并行 | 有单元时间线覆盖的文件用 `parallel_map` 一起重解析,再顺序提交;没有时间线的调用方(探针、测试)仍走原来的串行闭包环境路径 | 2.25 s → **1.7–1.8 s**(约 1.9×) |
| 结果一致性 | 与改动前逐行对比 `workspace_probe` 输出(147 行,含 `reused/rebuilt/unstored` 与全部 definition 表)**一致**;新增测试:并行 prepare + 顺序 commit 与逐个 `get` 得到相同的索引,写盘的条目能被下一次运行完整读回 | 全部 1 479 个用例通过,clippy 零警告 |

### 第四轮:预扫描 `#include`

| 项 | 做了什么 | 读数(同上) |
|---|---|---|
| 预扫描 | `FileIndexer::scan_includes`:只做词法 + 指令扫描 + 解析 include 路径(约为解析成本的 1/10),在**解析之前**就得出该文件的 include 列表 | 冷启动 1.7–1.8 s → **1.3 s**(相对最初的 3.3 s 是 2.5×);暖启动 ~450 ms → **~235 ms** |
| `SummaryStore::prepare_closure` | 各核共享一个"待读文件"前沿:worker 取一个文件,先扫描它的 include 并把目标放进前沿,再去解析它——第一个文件一词法完,其余核就在并行读它引用的整个图(广度优先),并行宽度 = include 图的宽度,而不是"一层"的宽度 | 预算与过滤:`Session::advance` 每次最多预读 `2×steps` 个,且跳过已经读过的文件;编辑/关闭/文件事件时清空预读表 |
| 不重复搜索 | 扫描解析出的路径通过 `with_scanned_includes` 交给随后的解析,只对**完全相同的指令**复用(形式/目标/`include_next` 都一致才用),所以 include 路径的文件系统搜索只做一次 | 详细计时里 `includes` 102 ms → 38 ms;原有测试"命中时不搜索、构建时每个候选只探测一次"仍通过 |
| 与解析一致 | 引号形式 `#include "x.h"` 词法只给 `StringLiteral`,解析器才会折成 `HeaderName`;预扫描按同一条规则(无转义)折叠,并有测试断言"预扫描的 include = 解析记录的 include" | 与改动前逐行对比 `workspace_probe` 输出一致 |
| 遇到 panic 不死锁 | worker 里 `catch_unwind`,记下 payload,等所有 worker 停下后在调用线程重新抛出 | — |

`INDEX_SLICE` 由 16 提到 32(并行后同样的锁占用时间里能读更多文件)。已知的上限:include 图的关键路径是串行的(先解析才知道 include 什么),
所以并行度受图的宽度限制;再往下要靠"预扫描 `#include` 行"提前发现文件(词法 147 MB/s,比解析便宜两个数量级)。

**没做(诚实登记)**:子串搜索(`"widget7_1"` 这种不是任何名字前缀的查询)仍是对 12 万个名字的一次扫描,8 ms;要更快需要 trigram 倒排。
括号/作用域"栅栏"(§1.2(a))、文件级并行(§3.3)、`project.rs` / `session.rs` 拆分(§4.1)、跨文件 references(§5)都还没动。

### 第五轮:语义高亮重构(正确性 + 一次 600 ms 的测量)

本轮只动 [`semantic.rs`](../crates/cpp_code_analysis/src/semantic.rs) 与
[`semantic_token/mod.rs`](../crates/cpp_ls/src/handlers/semantic_token/mod.rs),以及 `ProjectIndex` 上一个新查询。
文档以 `semantic.rs` 的模块注释为准;这里是读数与理由。

| 项 | 改前 | 改后 |
|---|---|---|
| **"这个用途指的是哪条声明"** | 按**拼写**查表:先把本文件声明的名字建成 `spelling → declaration`,再拿每个用途的拼写去查。便宜,但答错的就是读者最先看见的那几类 | 按**位置**解析:`sema::resolve::definition_at`(同一个文件自己的作用域树,就是"跳转定义"用的那一个)。`int scaled(int size) { return size * size; }` 里的 `size` 现在是**参数**,不再是同名的文件级函数 |
| 成员访问 `w.size` | 只要文件里有任何一条声明叫 `size`,就画成那条声明 | 识别出成员访问**形状**,交给索引按 `Widget::size` 问;答案有歧义时**不着色**(颜色是一个断言,说不清就不说) |
| 索引查询 | `definition()`:多条候选一律 `Ambiguous` | 新增 `ProjectIndex::kind_of()`:候选**种类一致**就给答案(重载集是同一种东西),不一致才 `Ambiguous`。`definitions()` 与它共用 `certain_declarations()` 的候选走查 |
| 每个答案的来源 | 未记录 | `Name.provenance` 四档:`DeclaredHere` / `Held`(作用域树)/ `Found`(索引)/ `Macro`。探针按档打印 |
| LSP 图例 | 类型索引靠 `TOKEN_TYPES.iter().position(...)` 现算 | `LEGEND` 表把**数字写出来**,并有测试断言"数字 == 它在图例里的位置";`encode` 一次查表同时完成"取数字"和"客户端画不画得了" |
| 读数(MinGW 闭包 356 文件,取最大的 45 个,19 797 个标识符,release) | 28 ms / 5 952 条着色 | **35–40 ms / 5 591 条**(1.8–2.0 µs 一个标识符) |

**改对的那几条**(`tests/semantic.rs` 逐条钉住):参数不再被同名函数顶掉;成员访问不再被同名声明顶掉;
`size_t` 这类"只被成员访问用到"的拼写在索引有歧义时不再猜;宏优先于同名声明(预处理看到的是文本)。

**三个数量级级别的发现,都来自"先量再改"**(每个都曾在错误的地方找过):
`qualified_name_at` 是**按偏移下降树**,按拼写调用一次就是一次下降 —— 在 3 669 个标识符的 `bits/version.h` 上,
它是 630 ms 里的 **600 ms(95%)**;`(a && b)` 与 `w.size` 是**同一个节点种类**,所以 447 条
`#if defined(…)` 让"识别成员访问"付出每条件一次子树走查;而"这个拼写本文件根本没声明"这一档,
一层 `HashSet` 就省掉了全部无用的作用域走查。三处修完:**600 ms → 40 ms**。

**没做(诚实登记)**:成员访问仍然只走"按拼写问索引"这一条路,没有做**类型推断再按类成员解析**
(`widget.size` 的 `widget` 是什么类型,`index::project` 里已经会算,只是没有按文件批量提供)。
所以"文件里恰好也有一个同名函数"时,`w.size` 会不着色而不是画错 —— 这是有意的选择。

---

## 第六轮:类型推断与类型检查(第 1 阶段:基线 + 类型模型)

这一轮的目标是**先量出类型这一层到底坏在哪**,再落地一个能承载推断的类型模型。读数全部来自新探针
[`member_probe.rs`](../crates/cpp_code_analysis/examples/member_probe.rs)(MinGW 标准库闭包 356 文件,release)。

### 基线:成员访问在哪一步停下

一个成员访问要连续过三关,任何一关失败都只表现为"没有答案":

```text
1. 成员名写出来了        `w.` 后面还没写,那是补全不是跳转        → 14 453 个有名字,56 个正在输入
2. 对象有类型            声明里的 `type_of`、`auto`、`*p`、`f()`    → 5 934 个有类型(41%)
3. 那个类型的成员列得出来 类在索引里、基类能解析、名字不歧义        → 1 307 个(9%)
```

失败原因的分布,这才是可执行的清单:

| 计数 | 停在哪 | 说明 |
|---|---|---|
| 8 237 | `type: UnknownType` | 类型读不出来。其中"对象是个名字"7 001、"成员访问"1 058、"调用"385 |
| 2 808 | `members: NotDeclaredHere` | 类型读出来了,但那个类名**没被索引到** |
| 1 723 | `members: Ambiguous` | 同名类有多条声明(`lldiv_t`、`pair`),没有东西在它们之间做选择 |
| 282 | `type: NotDeclaredHere` | 名字本身没解析到 |
| 96 | 列出来是空的 | 类找到了但没有成员 |

### 建了什么:[`sema/types.rs`](../crates/cpp_code_analysis/src/sema/types.rs)

问题不在于"没有类型",而在于**类型一直是一个 `String`**。所以修法是把它变成一个形状:

```text
Type::Builtin / Named{name, arguments} / TemplateParameter / Pointer / Reference{rvalue}
     / Array{extent} / Function{returns, parameters} / Pack
```

三件事是这一轮真正落地的:

1. **从语法读类型,而不是剪字符串**。`type_of_declaration(specifiers, declarator, name)`:specifier
   序列给基类型(`unsigned long long` 是**三个** `BuiltinType` 节点),declarator 给操作符(`*`、`&`、`[4]`、
   `(int)`),按"离名字最近的层是最外层操作符"的顺序装配。属性、`static`、`constexpr`、`friend` 一律**不进类型**。
2. **类名与类型名分开**:`std::vector<int>` 是类型,`std::vector` 是类;`Type::class_name()` 就是那条缝。
   这就是"`_Cont>` / `std::map<std::string, int>` 被整个当成类名去问"这个错误的修法。
3. **模板参数替换**:`Type::substituted(&TypeSubstitutions)` 按**名字和位置**把 `T` 换成实参,并有
   `depends_on_parameter()` 让调用方在花一次索引查询之前就知道"这个答案只有实例化能给"。

   子类型用 **`Arc<Type>`(`TypeOf`)而不是 `Box<Type>`**:类型是一棵树,而推断会不停地往里看
   (`substituted` / `decay` / `pointee` / `class_name`),`Box` 下每一次"往里看"都是**整棵树的深拷贝**——
   问一次 `std::map<K, V>::value_type` 要把 `K`、`V` 的拼写整个复制一遍才看得见,而一条成员访问链会每个环节复制一次。
   `Arc` 把这件事变成一次引用计数自增,顺带买到三样:`inner()` 能返回**句柄**而不是副本、
   同一个拼写只分配一次、以及**安全代码里没有 `get_mut`** —— 类型是值,不会在作为 map 键的时候被人改掉。
   用 `Arc` 而不是 `Rc`,因为索引要在 LSP 的读线程之间共享,类型跨线程是常规而不是特例。
   测试 `a_sub_type_is_handed_out_shared_rather_than_copied` 用 `Arc::ptr_eq` 钉住这条性质 ——
   它不会因为"两个相等的类型"而误过:分开构造的两个相等类型是两次分配。

同时把**已接线的**那条路换掉了:[`declared_type_of_with`](../crates/cpp_code_analysis/src/sema/declarations.rs)
原来剪字符串,现在把两个节点交给新读者。这一步立刻修掉了一个真实缺陷 ——
**MSVC 的 `cin` 声明**:`_EXPORT_STD extern "C++" __PURE_APPDOMAIN_GLOBAL _CRTDATA2_IMPORT istream cin;`
里 specifier 序列有三个名字,类型是**最后**一个。老读者取第一个,于是同一条声明在裸读里是
`__PURE_APPDOMAIN_GLOBAL`、在熟读里是 `istream`,两者不一致 → `std::cin` 永远 `Ambiguous`。
这条规则原本写在 `type_spelling_of` 里,而它**没有任何调用者**(唯一调用者就是我刚换掉的那段)。

### 读数

| | 改前 | 改后 |
|---|---|---|
| 畸形类型拼写(30 185 条声明) | 11(7 条残留 specifier + 4 条丢名字的 `_Cont>`) | **0** |
| `friend constexpr iter_difference_t` 这类"关键字当类型" | 出现 | 0 |
| `std::map<std::string, int>` 被当成类名去问 | 是 | 否(问 `std::map`) |
| `cargo test --workspace` | 全绿 | 全绿(543 + 全部集成);clippy 零警告 |

畸形计数**到 0 的过程本身是一条证据链**:先修 `friend`/`ClassDef` 一类的 specifier 分桶(7 → 0 中的 7),
再把"没有规则的节点当类型名"这条兜底收成**最后手段** —— 一个关键字当类型名的代价是假的类型,
而拒绝的代价是一个调用方看得出来的 `void`。

### 还没做(下一阶段,按读数排序)

1. **表达式类型还挂在旧读者上**:`type_of_expression` 仍返回 `String`,`auto` 去重也用文本手术
   (`deduction_inputs`)。这一步把推断层换成 `Known<Type>`,成员访问/补全/hover 才能拿到规范化的类名。
2. **类模板的成员**:`std::vector<int>::size_type` 需要"类模板参数列表 ↔ 实参"的配对;
   `Type::substituted` 已经是那个操作,缺的是"从声明里读出参数列表"。
3. **同名类多声明时的选择**(1 723 次):现在一律 `Ambiguous`,而 `std::pair` 的若干声明
   其实是同一个模板的多次声明。

### 第 2 阶段:推断层换成 `Type`(已完成,见下)

---

## 第七轮:类型推断与类型检查(第 2 阶段:推断层接线 + 类型拼写解析)

第 1 阶段把"声明 → 类型"做成了形状;这一阶段把**整条推断链**从 `String` 换成 `Type`,并补上反向的那一半:
把索引里记的拼写读回形状。

### 改了什么

| 位置 | 改前 | 改后 |
|---|---|---|
| `type_of_expression` | `Known<(String, PathBuf)>`,用 `pointee_type_name` / `element_type_name` 剪字符串 | `Known<(Type, PathBuf)>`;`*` 是 `Type::pointee()`、`&` 是 `decay()` 加一层指针、下标是数组的 `pointee()` |
| `declared_type` / `NamedDeclaration::type_of` | 拿摘要里的 `type_of` 字符串 | 本文件走**语法**(`type_of_declaration` + 定位声明者 declarator),索引里的走**拼写解析**(`parse_type_spelling`) |
| 成员查询的"类" | `base_type_name("std::vector<int>")` → `std::vector<int>`(一个没人声明的类) | `member_access_class(&Type)`:`class_name()`,并且**看穿一层指针**(`p->size` / `(*p).size`) |
| `auto` 的合并 | 文本替换 | 文本拼写决定**限定符位置**,形状由 `Type` 决定 |
| `const` | 被丢掉(不在模型里) | `Type::Qualified` 包装:`const Widget*` 与 `Widget* const` 是两个不同的类型,`Display` 按位置写回去 |
| 缓存 | `CODEC_VERSION 15` / `FORMAT_VERSION 1` | **16 / 2** —— 见下 |

### 新增的那个东西:`parse_type_spelling`

索引里的事实是磁盘上的一个 `String`,它来自的文件根本没打开 —— 所以"类型"必须能从**拼写**读回来。
这函数是 `Type::Display` 的逆,而且它只保证一件事做对:**`class_name()`**。其余都丢得起(把 `int` 读成
`Type::Named("int")` 只是丢了一个标签),而一个**错的类名**会把成员查询送去一个不存在的类 —— 所以剥离顺序是
围着它写的:先脱尾部的 `const`(`Widget* const` 的 `const` 在操作符**后面**),再脱 `&&`/`&`/`*`,再脱数组维度,
最后才是名字与实参表。

顺手修掉的两个真实形状错误:

```text
std::map<std::string, int>   逗号在实参表里,不是类型的   → 切分按尖括号深度走
int                           内建词表里没有它            → `int` 被当作类名,成员访问去问"谁声明了 int"
```

第二条是一类错误的样板:`int` 读成 `Named("int")` 时,对 `(*q).size` 的回答是
**"`int` 没有在这个文件里声明"**,读起来像"这个类丢了"而不是"这不是个类";现在答案是 `UnknownType("int")`。

### 读数(MinGW 标准库闭包 356 文件,14 453 个成员访问,release)

| | 第 1 阶段基线 | 现在 |
|---|---|---|
| 读出了类型 | 5 934(41%) | **5 959(41%)** |
| 列得出成员 | 1 307(9% / 有类型的 22%) | **1 348(9% / 有类型的 23%)** |
| `members: Ambiguous` | 1 723 | 2 034 |
| 畸形类型拼写(30 185 条声明) | 11 | **0** |

**`Ambiguous` 涨了 311,这是好消息**,而且这是这一阶段最值得看的一行:以前这些访问撞上的是
`_Cont>`、`Point>` 这种**没人声明的假类名**(报 `NotDeclaredHere`),现在是
`::__gnu_cxx::__normal_iterator<_Ite, _Cont>` —— **一个格式正确的类名,只是索引里有多条声明**。
失败原因从"这个名字不存在"变成"这个类有多个声明",而后者是可以用一条规则解决的(下一阶段的第 3 项)。

### 缓存版本:两个数字都要动

`DeclFact::type_of` **还是同一个字段**,但它现在可能装的东西变了:老读者会把
`const [[nodiscard]] constexpr size_type` 和 `friend constexpr iter_difference_t` 写进这个字段。
字节格式没变,所以老条目**能解码** —— 这正是 `FORMAT_VERSION` 存在的情形:"能解码,但说的是这个构建
不会再写的东西"。所以 `CODEC_VERSION` 15 → 16、`FORMAT_VERSION` 1 → 2,老缓存下次启动就清掉。

### 诚实登记

- `type_of_expression` 仍然只认识**名字 / `this` / 初值列表 / 调用 / 括号 / `*` / `&` / 下标 / 成员访问**。
  算术、比较、`new`、`?:` 一律 `UnknownType`,这是有意的。
- 6980 个"对象是个名字但读不出类型"里,最大的一块是**依赖类型**:`declval<_Tp&>()._Tp`、
  `__t._Tp`、`typename T::value_type` —— 要实例化才有答案,而这一层不实例化。这是下一阶段的主项。
- `Type` 仍然**不解析别名**:`using Int = int;` 之后 `Int` 与 `int` 是两个类型。相等仍是按拼写。

---

## 第八轮:类模板的形参与实参配对(依赖类型的第一半)

第 2 阶段之后剩下的最大一块是**依赖类型**:`std::vector<int>` 的成员类型写着 `_Ty&`,而 `_Ty` 在
`<vector>` 里,查询手里只有 `a.cpp`。这一轮把"形参名"和"实参"配起来,`std::vector<int>::front` 于是答 `int&`。

### 三件事

| | 做了什么 | 为什么必须这样 |
|---|---|---|
| **形参名随事实存下来** | `DeclFact` 新增 `parameters: Vec<String>`;`declared_template_parameters_of` 从**模板声明的父节点**里找类体,`template_parameter_names` 读名字 | 查询手里是**另一个文件**:`member_fact` 被问 `std::vector<int>::reference` 时,形参表写在 `<vector>` 里,而索引不保存文本。重新解析那个头文件 = 每次成员查询一次解析 |
| **配对** | `TypeBindings`(自持有)与 `TypeSubstitutions`(借用)一对:`member_bindings` 把类的形参名与对象的实参按**位置**配对 | 一处是"手里就有",一处是"得去问",生命周期不同;合成一个类型等于让所有调用方都分配 |
| **替换** | `Type::substituted` 现在把**名字在形参表里的 `Named`** 也当作形参 | 形参不是一种能从文本认出来的类型:`template <class _Ty> struct vector { _Ty& front; };` 里的 `_Ty` 拼写和类名**完全一样** —— 让它成为形参的是它被声明的那张表,而那张表属于类、不属于成员 |

替换落在**一处**:`NamedDeclaration::type_of` 与 `what_a_call_has`。中间试过在 `member_fact` 里替换并写回拼写,
结果是**替换跑了两次** —— `int&` 再替换一次,`int` 被当形参弄丢,答案又变回 `_Ty&`。教训是这类"改写结果"的步骤
只能有一个执行点。

### 读数与测试

新增 3 条测试(`tests/types.rs`),都经由索引、跨文件:

- `std::vector<int>` 的 `front`(声明为 `_Ty&`)→ **`int&`**;`data`(声明为 `_Ty*`)→ **`int*`**;
- `std::vector` 不带实参时形参表**原样保留**(`["_Ty"]`),不猜;
- **边界写成测试**:成员的类型是**同模板里的别名**(`reference` 是 `_Ty&`,`front` 声明为 `reference`)时,
  替换无处落脚 —— 答案是名字 `reference` 本身,`class_name()` 给 `None`。要往下走就得把
  `std::vector<int>::reference` 当成一次**带同一套配对的成员查询**,这一步这一层还没做。

`cargo test --release --workspace`:**49 个二进制全绿**;`clippy` 零警告。这样本轮的每次验证都是 release。

### 这轮修掉的两个真实错误

1. **"这个模板引入了哪个类"判断错了两次**。`template <…>` 是 `TemplateDecl`,类体在**兄弟节点**里:搜它的
   *后代* 会撞上 `TemplateParameter` 自己的 `DeclSpecifierSeq`(`class _Ty` 就是一个),于是"是,这就是我的类";
   搜它的*子节点*只有形参表,于是没有任何模板引入任何类。两个方向都量过,最后是"看父节点的子节点"。
2. **`type_at` 的偏移取错了**:名字**开头**的偏移落在对象上(`v.front` 的 `v` 上),不是成员上。这不是产品
   的错,是我的探针/测试的错,但它让"替换没生效"和"取错了位置"看起来一模一样。

### 还没做

- **嵌套别名**(上面第三条):需要"带配对的成员查询"再走一层。
- **`v.data()` 这条调用路径**:成员访问能替换了,但成员**函数调用**的返回类型还没接上(调用走
  `declaration_of_a_callee` → `what_a_call_has`,配对照理应在那里生效,但要单独量)。
- **`Ambiguous` 的 2 034 次**:同一模板的多次声明仍然一律 `Ambiguous`,需要一个"这些声明是同一个实体"的规则。

---

## 0. 总评

**强项**

- 分层清楚:词法 → 指令/宏 → 熟 token 流 → 语法树 → 索引 → LSP 壳,每层契约写在模块文档里。
- LSP 壳的并发模型是对的:单写者更新队列 + 请求前按序号应用通知 + 读快照 + 同文件 `didChange` 合并 + 每文件诊断防抖。
- 缓存键用内容哈希 + 阅读器指纹,"过期答案 = 错误答案"这条原则被贯彻。
- 答案有"未知"这一档(`Known::{Yes,No,Unknown}`),拒绝给半个答案(rename 遇到不完整搜索就拒绝)。
- 度量文化(`stages.rs`、探针、A/B 对照)——`plan-units.md` §11–§17 的"先量再改"已经证明有效(冷启动 23.8 s → 1.9 s)。

**主要风险**(按优先级,详见下文)

| # | 类别 | 一句话 |
|---|---|---|
| A1 | 正确性(待验证) | `invalidate_dependents` 的"只有定义宏的头文件才作废依赖者"判据用的是**编辑前**的摘要,漏掉新增 `#define`、增删 `#include`、`#undef` 等 |
| A2 | 可靠性 | 缓存目录只写不删:每次源码变化(指纹变)都会留下一整套孤儿分片,磁盘无界增长 |
| A3 | 性能 | `ProjectIndex` 没有按名字的倒排;`workspace/symbol`、可见名收集都是全量线性扫描 + 每条事实分配字符串 |
| A4 | 性能 | `Session.units` 无上限、每次按键全清(计划 §2.2 已识别,还没做) |
| A5 | 工程 | 单文件过大(`index/project.rs` 7 271 行、`session.rs` 3 951 行),`docs/architecture.md` 在工作区被删除但 README 仍指向它 |
| B | 产品功能 | 最有价值的两项——普通符号的 references/rename——目前一律拒答;另有一批便宜的功能可补 |

---

## 1. 对 `plan-units.md` 的评审

### 1.1 认同的部分

1. **"工作单位应该是编译单元"** 的诊断成立,§11–§16 用读数把顺序修正过来(先修 O(n²) 下潜、再让第二遍 pass 吃单元时间线)是正确的工程方法。
2. **不做**清单(不落盘 AST/PCH、不做增量重解析、不用 mtime、不做无界记忆化)——全部同意,尤其"落盘的是事实不是树"。
3. §18–§19 对"接线后答案变差"的处理——**不接线,先查清**——是对的;`errors != 0` 就不落盘这道门也对。

### 1.2 建议调整的地方

**(a) 阶段 1(单元读数)的前置条件应改写成"解析错误不跨文件"**

§19 的根因是:一个文件读不干净,在"整个程序一条流"的读数里会**污染后面所有文件的作用域**(`vc_attributes` 名字空间没配对)。
"把 26 条错误变成 0"是治标——真实工程里总会有解析不了的文件(编译器扩展、宏技巧)。建议把它作为**架构不变式**而不是一次性修复:

- 单元流里**每个被包含文件的边界处做括号/作用域"栅栏"**:进入 `#include` 时记下解析器的括号深度与作用域栈深度,退出时若不一致就**强制回滚到进入时的状态**,并把该文件标为"读不干净"。
  这正是编译器的语义(头文件里未配对的 `{` 在真实编译里是硬错误,但编辑器必须容错)。
- 有了栅栏,§19 结尾的"3. 只关一半的门"就自然成立:出错只影响出错的文件,不需要整条读数作废。
- 门禁:构造合成用例——头 A 含未闭合 `namespace x {`,头 B 声明 `struct S`;单元读数里 `S` 不能落进 `x`。

**(b) 阶段 3 的"边界"规则先做,而且比计划写得更便宜**

`buffer_changed` 每次按键 `units.clear()` 是最大的单点浪费,且**不需要**阶段 1/2 就能修:

- 判据:新旧文本的**公共前缀之后**第一处差异,是否落在"最后一个顶层 `#include`/`#define`/`#undef`/条件指令"的**之后**。
  实现上不必先求 clangd 式的 preamble 边界——只需要比较**新旧摘要中"指令事实"的序列**(includes、macros、guards 的名字与位置无关部分)是否相同:相同 → 环境未变 → 不清 `units`、不 `invalidate_dependents`。
- 这也顺带解决 A1(见 §2.1):把判据从"编辑前是否定义宏"改成"**指令事实前后是否相同**"。
- 门禁按计划:函数体里打字,`units` 不被清空——写成测试。

**(c) 阶段 2(并行)需要先解决一个前置问题:`Session` 的写锁粒度**

当前泵在 `advance` 里持有写锁"一个 slice"(`update_queue.rs` 注释里量过:每条消息等 ~90 ms)。并行 worker 引入后有两条路:

1. worker 持有**不可变的输入快照**(`Arc` 的 VFS 文本 + 配置 + 时间线),纯函数地产出 `UnitFacts`,协调者只在**合并**时短暂持写锁。这是计划的设想,方向对。
2. 前提:`FileProvider`/`Vfs` 需要能廉价产出只读快照(`Arc<str>` 已有,`OpenDocuments::text` 目前返回 `String`,会复制整个缓冲区——见 §3.4)。

建议把"**只读输入快照**"作为阶段 2 的第 0 步单独立项、单独有门禁(合并一次写锁 < 5 ms)。

**(d) 统一"事实归属"(§4.2)之前先加计数器**

"被盖掉的那份要计数"这一条便宜、无风险,建议**现在就加**(哪怕单元读数还没接线):
`ProjectIndex` 里同一 `(name, kind)` 在裸读与熟读中的冲突数、同名多定义数,由探针打印。它既是阶段 1 的门禁数据,也直接给出 §18 里"少 49 个名字"的差集。

**(e) 文档状态**

- `docs/architecture.md`(2 310 行,git 里仍有)当前在工作区是**已删除**状态,`README.md` 仍链接它,`Cargo.toml` 注释还引用不存在的 `docs/ls-architecture.md`。
  请确认是有意的:若是,更新 README/注释;若否,`git checkout docs/architecture.md`。
- `plan-units.md` 自己写着"落地后并进 architecture.md 再删除";现在 §11–§19 已是**实测日志**而不是提案,建议拆成两份:`plan-units.md`(只留目标结构 + 剩余阶段 + 门禁)与 `journal/perf-*.md`(读数日志),否则 900+ 行的文档会越来越难当"计划"读。

---

## 2. 正确性与可靠性

### 2.1 A1(待验证):`invalidate_dependents` 的判据不完整

位置:[`session.rs`](../crates/cpp_code_analysis/src/session.rs) `Session::invalidate_dependents`(约 876 行)。

```rust
let defines_macros = self.store.index().summary(path)   // ← 编辑**之前**的摘要(buffer_changed 在 forget 之前调用)
    .is_some_and(|s| s.macros.iter().any(|f| f.kind.is_definition()));
if !defines_macros { return; }
```

以下编辑会改变依赖者的环境,但**可能**不触发作废:

1. 头文件原本没有 `#define`,用户**新加**了一个(旧摘要里没有宏 → 直接返回)。
2. 头文件不定义宏,但**增/删 `#include`**(引入的宏集合变了)。
3. 增/删 `#undef`、`#pragma once`/include guard、改变 `#if` 条件(哪条 `#include` 被取)。

后果是依赖者持有**过期的熟读**——文档自己说"过期答案是错的不是缺的"。
注释里承认"gate 是名字而不是 body",但没有覆盖上面三条。

建议:按 §1.2(b) 比较**新旧指令事实序列**(或最简单:比较新旧摘要的 `(includes, macros(name,kind), guards)` 的哈希),不同就走反向遍历;相同才跳过。
先写一条失败测试:头文件初始无宏 → 打开依赖者并熟读 → 给头文件加 `#define NS namespace ns {` → 断言依赖者的熟读被作废。

### 2.2 A2:缓存目录无回收

`READING_FINGERPRINT` 把 `cpp_parser/src` + `cpp_code_analysis/src` 全部文本哈希进键(注释、文档改动也算)。
代码里没有任何 `prune/evict/remove` 逻辑(全仓库只有一处测试里的 `remove_file`)。结果:

- 开发期每改一次源码,`.cppls/summaries` 里就新增一整套分片,旧的永远留着;
  计划 §1 里"工作区 `.cppls/summaries` 已有约 150 个条目"很可能就包含这类孤儿。
- 发布后每次升级同理。

建议(按成本排序):

1. **启动时按指纹清扫**:分片文件名带指纹前缀(或放进 `summaries/<fingerprint>/` 子目录)→ 启动时删除所有不是当前指纹的目录。一行规则,零风险。
2. 加**容量上限**(如 512 MB / 按 atime 或写入序淘汰),超出时后台清理。
3. 指纹对**注释/文档改动不敏感**:build.rs 里在哈希前剥掉 `//`、`///`、`//!` 行(简单行首判断就够,不需要完整词法),
   这样改文档不再让缓存全废。注意 `plan-units.md` §1 的"每次改源码冷启动 26 s"主要来自这条。
4. 开发模式(`debug_assertions`)下可把指纹粗化到"`git` HEAD 的 tree hash + 未提交文件列表哈希",避免每次保存都失效;发布构建仍用内容哈希。

### 2.3 写入原子性

已核对:摘要写入是 `.tmp` + `rename`(`index::write_summary`),解码对截断/损坏返回 `DecodeError`(有测试)。
翻译单元缓存(`tu_cache.rs`)是原地 `fs::write`,且按 (root, key) 覆盖——一次撕裂写会被下次 `get` 的解码/闭包校验拒绝,
不会被当成答案,但建议也改成 `.tmp` + `rename`,与摘要一致。

### 2.4 阻塞与锁

- [`analysis_state.rs`](../crates/cpp_ls/src/context/analysis_state.rs) 的 `update`:先 `gate.write_owned().await`,再 `block_in_place` 里 `RwLock::write()`——
  等读者时是**阻塞**当前工作线程。可接受(用了 `block_in_place`),但 `block_in_place` 在 `multi_thread` 运行时里会**吃掉一个 worker 线程**直到写锁拿到。
  已有 `Semaphore` 限制读并发到 `available_parallelism`,若 permits == 线程数,更新期间可能造成 tokio worker 饥饿。
  建议:`blocking_permits` 取 `available_parallelism() - 1`(至少 1),或把 `update` 也改成 `spawn_blocking`。
- `run_blocking` 遇到 panic 会 `resume_unwind`。分析器里一次 panic(如解析器的某个 `unreachable!`)会把该请求的任务拉下水。
  `util/catch_unwind.rs` 已存在,请确认**所有**请求入口都经过它(一个 panic 只应变成一次失败的请求 + 一行日志)。
- 锁中毒:代码统一用 `unwrap_or_else(|p| p.into_inner())`,这是"继续用可能不一致的数据"。
  对 `Session` 这种多字段不变量的结构,建议中毒后**重建会话**(`AnalysisState::open` 已经支持替换),而不是继续读。

---

## 3. 性能与数据结构

### 3.1 A3:`ProjectIndex` 缺少名字倒排

现状:`ProjectIndex` 的字段只有 `summaries`(按路径)、`order`、`included_by`、`macros`、`cooked`、`visibility_answers`。
这意味着:

- `symbols_matching`([`project.rs`](../crates/cpp_code_analysis/src/index/project.rs) ~3375)每次按键对**全部摘要的全部声明**做:
  `qualified_name()`(分配)+ `to_lowercase()`(分配)+ 排序;并且对每个文件调用 `normalize(&summary.path)` 再分配一次;
  还有"熟读 vs 裸读"的去重里 `raw.iter().any(...)` 是 O(n²)/文件。
- `visible_declarations_upto`(~3545)对所有摘要循环,再按 `visible` 映射过滤;
  `files_defining`(references.rs)对全部摘要扫一遍找宏定义。

138 文件的小工程感觉不到;**几千个文件的真实工程会线性变慢**,而且是在 LSP 交互路径上。

建议(与计划 §3.3 的 clangd `Dex` 思路一致,但要小得多):

1. 在 `ProjectIndex::insert/forget/insert_cooked` 里维护 `by_name: HashMap<Box<str>, SmallVec<(FileIdx, u32 /*fact idx*/)>>` 与 `macro_defs: HashMap<Box<str>, Vec<FileIdx>>`。
   `files_defining` 立刻变成一次查表;"名字 → 候选声明"变成一次查表 + `visible` 过滤。
2. 用**整数文件 id**(`FileIdx: u32`)取代 `String` 路径做内部键,`normalize()` 只在边界上调用一次。
3. workspace/symbol:预先存 `lowercase_qualified` 和一个**小写名字的 trigram 倒排**(或最简单:按首字母桶 + 子串扫描),
   `limit` 之后就停;排序键(rank, name, file)只对命中项计算。
4. 门禁:合成 5 000 文件 × 200 声明,`symbols_matching("wid", 50)` < 20 ms(release);现在应该是数百毫秒——先量再定。

### 3.2 A4:`units` 与其它中间产物没有上限

`Session.units: HashMap<String, Arc<TranslationUnit>>`(session.rs:336)只在 `buffer_changed` 里整表清空。
多根项目(几百个 `.cpp`)里,`units_for_the_pass` 会为每个"没被现有单元覆盖"的候选建一个单元并**全部留住**。
计划 §3.6 第 2 条已写"不做无界记忆化",但代码里这条还没落地:

- 给 `units` 加 LRU(建议上限 8–16)+ 计数进 `StoreStats`;
- `units_for_the_pass` 里 `candidates.contains(&path)`(Vec 线性查)对 N 个工程文件是 O(N²),换成 `HashSet<String>` + 保序 `Vec`;
  `units.iter().any(|u| u.environment_of(..))` 也是每候选一次线性扫描,单元少时没事,多根时建议按"文件 → 单元"建一张反查表。

### 3.3 冷启动之后的下一批热点(按 plan §16 的读数)

`parse` 895 ms(194 文件)、`sweep` 574 ms、`encode` 164 ms 已经没有"重复"。剩下的收益在:

- **并行**:文件级 `parse + sweep + encode` 是纯函数(输入:文本 + 环境),在**单元时间线已知**后可以用 `rayon`/线程池并行做 194 个文件——
  这一步**不需要**计划阶段 2 的整套"协调者 + worker"重构,收益就是核数倍。建议把它作为阶段 1.5:
  串行走 walk(顺序语义)→ 并行 `index_one` → 串行合并。
- **语法器超线性**:计划 §1 记了"4× 输入 → 19× 时间"。建议现在就写一个 `parse_scale` 的回归门禁(斜率 < 1.3),并找出原因
  (常见嫌疑:回溯时反复重新词法/重复 `checkpoint` 复制、`type_names`/`macro_names` 集合在每个回溯点克隆、按 token 序号线性扫描)。
  这是"下一个数量级"的问题,越早量越便宜。

### 3.4 内存与拷贝

- `OpenDocuments::text()` 返回 `Option<String>`(整份克隆);`FileProvider::read` 同为 `Option<String>`。
  编辑器里大文件每次按键至少复制 2–3 次。建议统一为 `Arc<str>`(`CachedFiles` 已经是),`FileProvider::read` 返回 `Option<Arc<str>>`。
- `Session::view()` 返回 `FileView`,请确认里面是 `Arc` 而不是拥有的 `String`/树(`view.source.clone()` 在测试里出现)。
- `DeclFact` 在索引里被大量 `fact.clone()`(`ProjectSymbol { fact: fact.clone() }`);字段里的 `String` 建议换 `Arc<str>`/`Box<str>` 并用 interner(`internment` 曾在依赖表里被删过,这里其实是一个合理的使用点)。

---

## 4. 工程质量

### 4.1 拆分超大文件

| 文件 | 行数 | 建议 |
|---|---|---|
| `index/project.rs` | 7 271(测试从 4 921 行开始,生产代码 ≈ 4 900) | 拆为 `project/{mod, lookup, members, completions, symbols, macros, visibility}.rs`;`ProjectIndex` 一个 `impl` 块 3 127→4 317 行(1 200 行)本身就该按主题分文件 |
| `session.rs` | 3 951(测试约 1 500) | 拆 `session/{documents, queue, cooking, units, queries}.rs`;`Work`/`Cooking` 队列结构体已是自然边界 |
| `cpp_parser/tests/gaps.rs` | 7 194 | 按主题拆文件,失败时定位更快;并把"已知缺口"用 `#[ignore = "gap: ..."]` 或单独 `gaps/` 目录标注,避免和回归测试混在一起 |
| `grammar/cpp/{types,decls}.rs` | 5 265 / 4 875 | 可以保持,但 `types.rs` 的规模说明"类型 vs 表达式"歧义处理集中在一处——建议在文件头列出所有启发式/回溯点索引 |

拆分是纯搬移(`lib.rs` 已经有"路径不变"的 re-export 习惯),风险低;建议**一次一个文件、单独提交**。

### 4.2 `lib.rs` 的 re-export 面

[`lib.rs`](../crates/cpp_code_analysis/src/lib.rs) 里同一批类型有 `crate::directive::…`、`crate::preprocess::directive::…` 两条路径,再加大量平铺 `pub use`。
它保护了迁移期,但对外 API 面因此很宽、`cargo doc` 也很乱。建议:迁移稳定后把平铺 re-export 收成一个 `prelude`,其余按目录访问,并在 `Cargo.toml` 里用 `#![warn(missing_docs)]` 之类逐步收紧。

### 4.3 测试

- 3 个 `#[ignore = "needs the release profile"]`:CI 里应有一个 `cargo test --release -- --ignored` 任务,否则这些"标准库闭包 156 文件"的用例永远不跑。
- `cpp_ls/tests/handshake.rs`(2 610 行)是端到端测试,用真实子进程 + 临时目录日志——建议给它加**超时护栏**(每用例 30 s)和并行安全的临时目录(测试之间 `target/debug/logs/*.log` 已经区分了名字,好)。
- 建议加**属性/模糊测试**:`lex → 拼回文本 == 原文`(无损性)、`parse` 任意字节不 panic、缓存分片解码任意字节不 panic。
  解析器容错是核心承诺,`cargo fuzz`/`proptest` 对它性价比最高。
- 建议加 **CI**(仓库里没有 `.github/workflows`):`cargo fmt --check`、`cargo clippy -D warnings`、`cargo test --workspace`、release 的 ignored 用例、Windows + Linux 两个平台(路径规范化 `normalize_path(.., cfg!(windows))` 分支只有跨平台才测得到)。

### 4.4 其它小项

- `build.rs` 里的 `expect/panic` 在失败时报错信息较好,但 `collect()` 在 `../cpp_parser/src` 缺失(例如单独发布 crate 时)会直接 panic;发布 crate 前需要处理(把指纹输入改成 `cpp_parser` 自己导出的常量更干净:`cpp_parser::READING_FINGERPRINT`,由它自己的 build.rs 生成)。
- `Cargo.toml` 注释里引用了不存在的 `docs/ls-architecture.md`。
- `.cppls.toml` 的 `exclude = ["target/**"]` 很好;建议默认排除 `target/`(Rust)、`build/`、`cmake-build-*`、`.git`、`node_modules` 之外的**仅当存在 `compile_commands.json` 指向**时才纳入,避免用户工作区被生成物拖慢(现在"不猜"是有意的,但至少对 `.git`/`node_modules` 可以内置)。

---

## 5. 产品功能建议(按价值/成本排序)

现有能力:definition、hover、completion、documentSymbol、folding、inlayHint(参数名)、references(仅宏)、rename(仅宏)、selectionRange、signatureHelp、semanticTokens、workspaceSymbol、diagnostics(pull)。

| 优先级 | 功能 | 为什么现在可做 | 备注 |
|---|---|---|---|
| P0 | **局部符号的 references / rename / documentHighlight** | 已有 `ScopeTree` + `sema::resolve`:同一文件内"这个名字是否解析到那条声明"是**树上问题**,不需要跨文件 parse | 只对**局部变量、参数、文件内 static、类私有成员**开放;跨文件符号继续拒答。这样绕开"每个候选一次 parse"的成本,同时覆盖 80% 的日常重命名 |
| P0 | **跨文件 references(类/函数/成员)** | 计划阶段 1 之后,一个单元一次 parse,`written/reported` 已按文件分组——**每个单元的树里就有所有引用点** | 存 `(USR 式键 = 限定名+kind+声明位置) → 引用列表` 到单元事实里,落盘随单元缓存(计划 1c)。这是 ccls/clangd 索引真正的价值 |
| P1 | `textDocument/declaration`、`typeDefinition`、`implementation` | `DeclFact` 已区分声明/定义;`members_of` 已有基类链 | `implementation` 需要"派生类"反向索引(基类链的反向表) |
| P1 | **codeAction**:生成缺失的 `#include`(名字已解析到某头文件但当前文件闭包里不含)、"实现声明"(函数定义骨架) | `HeaderTarget`/`include` 图已有 | `#include` 补全已是强项,反向的"缺头文件"是自然延伸 |
| P1 | `callHierarchy` / `typeHierarchy` | 有引用索引后是免费副产品 | 依赖上面的跨文件 references |
| P2 | `textDocument/formatting`(转调 `clang-format`,若在 `PATH`/工具链里) | 工具链发现已实现 | 不要自己实现格式化器 |
| P2 | `codeLens`(引用数、"未取分支"提示)、`documentLink`(`#include` 行,已能跳转) | 数据现成 | |
| P2 | **`$/progress` 细化**:索引阶段拆成"扫描/读取/熟读",并把 `stages.rs` 的计时作为可选 `cppls/status` 自定义通知暴露 | 计时已有 | 便于用户报"启动慢"时直接贴数字 |
| P3 | 增量同步(`TextDocumentSyncKind::Incremental`) | 现在是 FULL,合并 `didChange` 依赖它 | 大文件(数万行)才有意义;做了要重写 `UpdateEvent::absorbs` 的合并规则(见其注释),**先不做** |

另:`auto` 类型推断在模板体里拒答(54/60)是已知的;若要提升,建议**只对标准容器/智能指针的常见成员**做特例表(`vector<T>::operator[] → T&`、`map::find → iterator`),这比通用模板实例化便宜几个数量级,且覆盖率高。

---

## 6. 建议的执行顺序

每一项只认一条读数(沿用计划纪律),**先写失败测试/量出基线再改**:

1. **A2 缓存清扫**(0.5 天,零风险):按指纹分目录 + 启动清理;build.rs 剥注释。门禁:改一行注释后,缓存命中数不变;旧指纹目录被删。
2. **A1 依赖作废判据**(0.5–1 天):先写三条失败测试(§2.1),再改成"指令事实前后比较"。门禁:三条测试绿 + 既有 `editing_a_*` 三条不退化。
3. **计划阶段 3 的边界规则**(1–2 天):同一个"指令事实比较"函数复用于 `units.clear()` 的条件。门禁:函数体里打字 `units` 不清空(测试)。
4. **名字倒排 + 整数文件 id**(2–4 天):先做 `macro_defs`、`by_name`,`symbols_matching` 用 trigram/桶。门禁:5 000 文件合成工程上的三个查询的耗时(§3.1),以及探针两张表逐条不变。
5. **括号/作用域栅栏**(2–3 天):解决 §19 的根因,之后再重启单元读数接线。门禁:合成"未闭合名字空间"用例;`errors != 0` 的门可放宽为"按文件隔离"。
6. **阶段 1.5:文件级并行**(2–3 天):walk 串行、`index_one` 并行、合并串行。门禁:冷启动 1.9 s → ≈ 0.5–0.8 s(取决于核数)。
7. **跨文件 references**(随阶段 1 接线一起):单元事实里带引用点,落盘。
8. 拆分 `project.rs`/`session.rs`、加 CI、加 fuzz——可以穿插做,每次一个文件。

阶段 2 的完整"协调者 + worker 池"仍然放在**有一个多 `.cpp` 的真实工程能量出收益之后**(计划里同样这么说);在此之前 1.5 就够了。

---

## 7. 待确认的问题(需要你决定)

1. `docs/architecture.md` 被删除是有意的吗?(见 §1.2(e))
2. 目标用户工程规模:如果主要是"几十到几百个文件"的小工程,§3.1 的倒排优先级可以降到 P2;如果面向大型 CMake/MSVC 工程,应提到 P0。
3. 跨文件 references 是否愿意接受"只在单元读数接线之后提供"——它与阶段 1 的接线绑定,不会更早。
