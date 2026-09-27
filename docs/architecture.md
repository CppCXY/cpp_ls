# cpp_ls 架构

> **一句话**:token 流是唯一的事实,语法树是它的一个视图。
>
> 文件先词法化成 token 流;**前驱**(include、条件、宏表)直接从 token 流上算出来;按编译器的方式把宏展开成
> **熟 token 流**;在熟流上建**真实语法树**,语法与语义分析读它。裸文本上的那棵树只保证**无损 + 结构正确**,
> 它不再承担 C++ 正确性的责任。

---

## 0. 这份文档取代了什么

两天迭代留下了约 880 KB 的设计、路线与缺漏登记文档(`roadmap`、`grammar-gaps`、`index-design`、
`ls-architecture`、`std-library`、`msvc-notes`、`parser-assessment`),**全部删除**。它们记录的是一个**已经被
否定的方向**:让"读裸文本的那棵语法树"同时承担编辑器的宽容与编译器的正确。那个方向没有出路——两种要求互相
拉扯,拉扯出来的就是一堆"这个宏大概站在什么位置"的谓词。

这份文档是**唯一的规范**。旧文档里的 B 编号、§ 编号、轮次编号作为引用已经作废;代码注释里对它们的引用在
同一次清扫中删掉了,留在注释里的是**理由本身**,而不是"去看某份已经不存在的文档"。

被保留下来的结论(各节都有出处,§8 是代价换来的):

- 树必须无损(编辑器要能对文件字节负责);
- `Unknown` 是一等答案,"不知道"不许被当成"不是";
- 规则**不以外来宏的名字为键**(`Dialect::Msvc` 这种"哪个编译器"的判据除外);
- 每一条读数变化都要有形状断言;按文件 diff,不看总数;
- 若干用事故换来的坑,见 §8。

---

## 1. 三个结构,一条流水线

```text
源文件文本
   │  lex
   ▼
[1] token 流 ────────────────────────────────► LSP:语义高亮 / 折叠 / 括号 / 区域
   │  纯 token 分析(不建树)                     · 唯一无损的事实,offset 绝对
   ├────────────► [2] 前驱状态:include 图 / 条件区域 / 宏表 / guard
   │                 可共享;键 =(文件, 配置哈希);**不是树**
   │  expand(按 offset 取宏状态 + 条件求值)
   ▼
[3] 熟 token 流(每个 token 带 origin 链)
   │  parse(真实语法)
   ▼
[4] 真实语法树(index arena,**不是 rowan**)
   │  sema
   ▼
    作用域 / 符号 / 成员表 ──────────────────► 补全 / 跳转 / 悬停 / 重命名

[5] 文件 CST(rowan,**降级**):无损 + 括号 + 指令 + 浅层声明
      供:被排除分支的显示与索引、文档符号兜底、选择范围
```

三条贯穿全局的规则:

1. **谁都不许改写文件文本。** 展开发生在 token 流里,不在文本里(§3.2)。
2. **每个 LSP 功能只允许读一个结构**(§4),否则三个结构会给出三个答案。
3. **不知道就说不知道。** 缺配置、缺宏体、缺证据时,输出"可能"而不是猜一个(§5 的阶梯)。

---

## 2. 每个结构的契约

### 2.1 token 流 —— 唯一无损的事实

- 入口:**`cpp_parser::lex(text, &LexerConfig) -> (Vec<CppTokenData>, Vec<CppParseError>)`**——唯一的词法实现,
  parser 也通过它取 token(`CppParser::with_text`)。解析用的那一份由 `CppSyntaxTree::get_tokens()` 交出来,
  所以"消费者拿到的流"与"parser 读到的流"是同一份数据,不是两次词法碰巧一致。
- 内容:每个 token 有 `kind` 与绝对 `SourceRange`;空白、注释、续行都是 token(trivia 也是事实);没有 `Eof`
  token,"最后一个 token 之后"就是 `text.len()`。
- 不变量:把 token 的文本按 offset 拼回去,等于文件本身;token 索引在一次编辑的版本内稳定。
- **唯一的例外**:header name。`<` 与 `>` 对上下文无关的扫描来说是运算符,只有语法知道那里该是头文件名,
  所以 `#include <vector>` 的折叠由解析器做,`get_tokens()` 里是**一个** `HeaderName` 而 `lex` 里是三个 token。
  这条差异有断言守着(`tests/invariants.rs`),不允许出现第二种改写。
- 谁读它:前驱扫描(§2.2)、LSP 里所有不需要语法的功能(§4)。

### 2.2 前驱状态 —— 可共享,且**不是树**

- 内容:include 图(每条 `#include` 的形式与解析出的路径)、条件区域(`#if` 的分支、哪一支在效力)、宏表
  (名字 → 参数形式 + 替换列表)、guard(include guard / `#pragma once`)。
- 为什么不是树:**同一个头在不同宏状态下包含进来,结果不同**。所以宏环境是**位置化**的——某个 offset 之后
  才生效,`MacroEnvironment` 就是这个形状,不许换成"一个文件一张表"。
- 共享的粒度是 **(文件, 配置哈希)**,不是"文件"。缓存键已经有这套:`SummaryKey::new(content_hash, context_hash)`。
- 它今天已经存在(`crates/cpp_code_analysis/src/preprocess/`、`summary.rs`、`index/`)。**要改的是它的输入**:
  现在从红树读(`index/mod.rs` 的 `preprocess(&root)`),要改成从 token 流读(§6 M1)。

### 2.3 熟 token 流 —— 展开的结果,带 origin

- 展开器已经写好:`crates/cpp_code_analysis/src/preprocess/expand.rs`(hide set、`##` 重词法化、`#`
  字符串化、深度与总量预算,以及 `Origin`/`MacroInvocation` 的**调用链**)。
- 缺口只有一个:**它今天不被 parser 读**。它服务的是"展开视图"(hover)。
- 每个 token 必须能回答两个位置:
  - **spelling**:文本写在哪(可能在被包含的头里,甚至是别的文件);
  - **expansion / 调用点**:读者能看见的那个位置。
  这就是 clang 的 `getSpellingLoc` / `getExpansionLoc`;少了它,"宏里来的 token"的诊断和跳转都会指错地方。
- 熟流不是文本:没有字节,只有 token。所以它**没有**"文件"这个概念,也就不能是无损 CST。

### 2.4 真实语法树 —— index arena

- 在熟流上按**真实语法**解析;这里才是"语法正确性"的责任所在。
- 结构:`Vec<Node>` + `u32` 索引(parent / first_child / next_sibling / kind / token 区间)。两三百行,不需要
  引入 `indextree` 之类的依赖——rowan 内部就是这个形状。
- **不用 rowan**:rowan 的全部价值建立在"一棵不可变、无损、属于**一个文本**、可共享子树"的树上。熟流没有
  单一文本(宏体可能在别的文件)、不需要无损(它就是展开结果)、节点范围不连续、增量复用没有意义。四条都不成立。
- **offset 不是节点的身份**:节点持 **token 区间**,offset 是派生的,而且要区分 spelling 与调用点两个位置
  (§2.3)。否则来自宏体的 token 会把节点范围算到另一个文件去。
- 验收标准是**能删规则**:只为了模拟展开而存在的规则要被删掉,而不是与真实树共存(§6 M3)。

### 2.5 文件 CST —— rowan 留在这里

- `CppParser` 今天产出的无损 rowan 树继续保留,职责**降级**为:无损 + 括号/指令结构正确 + 浅层声明扫描。
- 它仍然是这些东西的家:被排除分支(编辑器要显示、要能跳、要能索引)、文档注释的归属、选择范围、
  以及"没有配置"时(§5 档 0)的兜底。
- 它**不再**需要"把 `_ACRTIMP int __cdecl f(...)` 读成一条函数声明"。

### 2.6 映射层 —— 双向

```text
光标 offset → 文件 token → 熟 token → 真实树节点 → 作用域        (补全、悬停、跳转)
真实树节点 → origin 链 → (文件, spelling 范围)                   (诊断、定位、重命名)
```

- 必须**双向**:下行给"光标处是什么",上行给"这个结果写在哪"。
- 没有这一层,补全就只能靠"从文件 offset 反推",而那正是今天所有怪异谓词的来源。

---

## 3. 被否掉的四条路(以及否掉它们的证据)

### 3.1 把展开拼进同一棵树
节点结构来自展开、token 来自调用点,树就既不无损也不对应文本。两棵树各自干净,是更好的答案。

### 3.2 文本级展开 + 重新解析(实测阴性)
把宏体替换进文本再解析:938 → 1177 条消息(**更差**);加到 4 步是 1968。原因是它必然改写文件,破坏无损
不变量,而且没有 per-offset 状态。**这个阴性结果否证的是"文本级展开",不是"展开"**——编译器从不改写文件,
它让 parser 拉熟 token。探针留在 `crates/cpp_code_analysis/examples/macro_expansion.rs`,结论记在那里。

### 3.3 以宏名为键、以形状猜展开的形状规则
"如果这个名字的体是 `{`…"、"如果这个分组后面跟一个类型关键字…"——**替预处理器干活**,而且必须一个宏一个
宏地猜,永远猜不完(两天里最后一批 B 编号全是这一类)。规则只允许建立在**标准的事实**上(保留标识符、
两种 `literal-operator-id` 拼法、说明符在声明符之前),不允许建立在外来库的名字上。

### 3.4 让裸树承担 C++ 正确性
这是 §0 里那个方向的正式名字。它的代价可以量化:同一套语法、同一套规则,只差"宏体知不知道",闭包语料
**454/455 干净**,无闭包语料 **217/255**。差的那部分不是语法缺漏,是缺宏知识。

---

## 4. 功能归属表

| 功能 | 读哪个结构 | 理由 |
|---|---|---|
| 语义高亮、折叠、括号匹配、区域、宏表 | token 流 + 前驱状态 | 只需要词法与区域 |
| 补全、签名帮助、悬停、跳转、重命名、引用 | 真实树 + 作用域 | 需要"光标处属于哪个作用域" |
| 文档符号 / 大纲 | 真实树为主,被排除分支从浅层扫描补 | 编辑器要显示没被编译的代码 |
| 诊断 | 真实树(只报 `Active`) | 在没被编译的分支里报错是假阳性 |
| 格式化 | 文件 CST | 要对文件字节负责 |

`Visibility { Active, Inactive, Unknown }`(`preprocess/guard.rs`)是这条表的公共词汇:**只有 `Active` 才
报诊断**;`Unknown` 按"可能被编译"处理,不隐藏。

---

## 5. 配置阶梯

没有配置就没有展开,"真实树"会退化成今天的裸树。所以阶梯必须显式:

| 档 | 输入 | 得到 |
|---|---|---|
| 0 | 只有文本(缓冲区、没有工具链) | token 流 + 结构 + 宽容语法 = 今天的行为 |
| 1 | 工具链已发现(标准、方言、预定义宏) | 熟 token 流 + 真实树 |
| 2 | 档 1 + include 闭包 | 最完整:位置化宏环境、跨文件语义 |

**测量必须在档 1/2 跑。** 无工具链时:条件全是 `Unknown`,没有区域是 `Inactive`;`$` 被词法器拒绝(那是
方言的事,不是缺漏)。把这些当成语法问题去"修",修的是测量假象。

---

## 6. 迁移计划

每一步都能单独验收;不许出现"两个机制长期共存"的状态。

### M0 —— token 流成为一等产物(**已完成**,见 §10)
`cpp_parser::lex` 是唯一的词法入口,parser 通过它取 token;`CppSyntaxTree::get_tokens()` 把这次解析用的那一份
交出来。词法的配置(方言、`$`、语言级别)跟着 token 流走。
**验收**:同一份文本,`tokenize` 的 token 序列与 parser 内部读到的逐字节一致。

### M1 —— 前驱层改从 token 流读(**已完成**,见 §10)
`preprocess(source, tokens)`:指令由语言自己的行规则从 token 流上认出,include / 条件 / guard / 宏表的
**形状不变**。
**验收**:`FileSummary` 与 `Guards` 在四个语料上的读数**逐文件**不变(总数不算数)。

### M2 —— 位置化展开器 → 熟 token 流(**文件级已完成**,见 §10)
`preprocess/cooked.rs`:`cook(source, tokens)`(档 0)与 `cook_with(source, tokens, initial)`(档 1/2,`initial`
是编译器的内建宏、`-D`、以及闭包在首行已生效的宏)。它丢掉指令、丢掉没被编译的分支(span 报在 `inactive`)、
把宏调用换成展开(`Origin` 链保留),并且**自己维护一张只喂"走到的"定义的宏表**——死分支里的 `#define` 不生效,
这是与 `preprocess` 的分别(`preprocess` 记录每一条定义,因为"这个宏在哪定义"的消费者要看见没编译的那些)。
**验收(实测)**:
- 内建 fixture(对象宏/函数宏/`#`/`##`/条件/死分支里的 `#define`/`#undef`):与 `g++ -E -P` **逐 token 一致**(60/60);
- **13 个真实头文件**(MSVC STL/UCRT,去掉 `#include` 行)**:13/13 一致**(指令行按约定归一化,并把编译器
  `-dM` 的内建宏喂给我们这一侧)。其中 `corecrt.h` 212 对 212 个 token,`sal.h` 4 对 4;`string` 是 0 对 0
  (去掉 include 后剩下的内容全在两边都不取的分支里,属于空一致,不算证据)。
- 这条外部判据立刻抓到一个真缺陷并已修:`##` 结果的 range 用了"左 token 起点 + **拼接后**长度",于是
  `#define P(a,b) a##b` 用在 `P(x, y)` 上时,range 盖住的是 `x,`(参数加逗号)——按 range 切源码的消费者会读到
  另一个 token 的文本,拼得足够长还会越过文件末尾。现在 range 是**左 token 本身**的范围,拼接后的拼写由
  token 的 `text` 携带(和 `#` 产生的字符串字面量一样:它的拼写本来就不在文件里)。

**M2 还没做的**:把整个 TU 拼起来(按 include 顺序把每个文件的熟流缝进去)——那需要 include 图;以及缓存,
它要等到有一个**热的**消费者才有意义(现在没有,先不建)。

### M3 —— 熟流上的真实语法树(**机制已完成**,见 §10)
`CookedStream::render()` 把熟流拼成一段**渲染文本**(token 之间一个空格),并给出 `RenderedSpan` 表:每个 token
在渲染里的位置(`cooked`)、它**写在哪**(`written`)、以及它的 `Origin`。真实树就是**用现有语法解析这段渲染
文本**——parser 一行没改。

**渲染文本不是文件**:它的 offset 不是文件位置,谁把它当文件位置谁就错,而且没有任何东西会拦住他。位置只能
经 `written_at` / `written_span` 问。这也是 rowan 在这里仍然可用的原因:树需要一个文本,而我们**给它一个**,
代价是必须同时带上映射表(§2.4 的"节点 offset 不是身份"就是这条纪律)。

**验收**:
- **一条配置**:`struct S { #if FEATURE int a; #else int b; #endif };` —— 裸树里 `a` 和 `b` **都在**(条件不是
  语法问题,规则无从下手),熟树里只有编译的那一个;把 `FEATURE` 定义为 `1` 再烤一次,换成另一个。这是 M3 买到
  的、任何形状规则都买不到的东西。
- **不得比规则读得差**:B133/B134/B136 那四个注解家族今天在裸树下**已经读通**(规则是有效的),熟读必须与裸读
  **逐条一致**(`get_errors()` 相等),并且用的是不含任何宏名的语法。

**档 2 的熟流普查(实测)**,`std_probe <list> --seeds --closure [--cooked]`,同一命令只差一个开关:

| 语料 | 档 | 干净 | 消息 | 消息种类 |
|---|---|---|---|---|
| 255 文件 SDK+STL | 2 裸 | 217 | 253 | 12 |
| 255 文件 SDK+STL | 2 **熟** | **242** / 255 | **99** | **3** |
| 109 文件 STL+UCRT | 2 裸 | 84 | 208 | 11 |
| 109 文件 STL+UCRT | 2 **熟** | **100** / 109 | **74** | **2** |

按文件分布(不是总数):255 的 `217\|9\|16\|13` → `242\|3\|4\|6`;109 的 `84\|4\|10\|11` → `100\|2\|2\|5` ——
文件只从差的一侧往外走,没有文件变差。

**整族消失的**:`expected a parameter list or an initializer`(12 条,**就是 B136 那一族**)、
`expected a declarator name`、`unexpected token`、`expected }`、`expected a template argument`、
`expected ), but get \|`,以及中间那一版查出来的 `expected ), but get ::`、`expected ;, but get (`。
剩下的只有三族:`expected primary expression`(42)、`expected ; after expression`(32)、`expected ;`(25)。

### 那 9 条新失败查清了:其中 7 条是**我们自己**的缺陷

初版的熟读在 109 语料上把 89 文件读干净、156 条消息,同时多出三族新失败。把**渲染文本**打出来(为此给
`std_probe --cooked` 接了映射表:错误在渲染里的 offset 经 `written_at` 换回文件的行列,并把渲染片段一起
打印——这是映射表的第一个真实消费者),一眼就看出来了:

```text
concepts:5:17  expected `;` after expression | RENDERED …NE__ ) "): " MESSAGE ) ( "warning " # NUMBER ": " MESSAGE )…
exception:4:6  expected primary expression    | RENDERED …tern "C++" void __cdecl __ExceptionPtrCreate ( ( SAL_name , …
```

**`#`、`NUMBER`、`MESSAGE`、`SAL_name` 是宏体被原样贴进流里**——`_STL_MSG(...)` 这类**函数宏**的体
(带 `#` 和参数名)被当成对象宏贴了出去。原因在 `configuration_from_environment`:in-force 通道只带
**文本体**、不带"是不是函数宏",于是我初版把它当对象宏用了。

一个体不是定义。修法是把那条通道**只计数、不使用**(`Configuration::in_force_without_a_parameter_list`):
通道当初是为**读**一个体而建的(`_STD_BEGIN` 告诉规则"这里开了个命名空间"),而"能代入参数地展开它"是
一个**更强**的断言,证据不够就不该下。改掉之后:109 → **100 干净 / 74 条 / 2 族**,255 → **242 干净 /
99 条 / 3 族**。

### 剩下三族的逐条分类(255 语料 13 个失败文件的首错,都带渲染片段)

**真语法缺口**(裸读时被宏名挡住了,熟读把它露出来):

| 渲染里读不动的形状 | 文件 | 缺口 |
|---|---|---|
| `int ( __cdecl * _onexit_t ) ( void ) ;` | `ucrt/stdlib.h` | **括号声明符里的调用约定**;`new` 里的 `void ( __cdecl * ) ( )` 同族 |
| `new ( :: std :: addressof ( _Obj ) ) _Ty ( … )` | `xutility` | placement new 里的**限定名表达式** |
| `_Tuple_conditional_explicit_v0<tuple_size_v<_Dest> == sizeof...(_Srcs), …>` | `tuple` | 模板实参是**表达式**,且带**包展开** |
| ~~`template <class _Ty> constexpr _Ty ( max ) ( … )`~~ | `utility` | ~~B135 那一族~~ **已修**(见下) |

**不是语法问题,是熟读还读不动:函数宏的体没有参数名,所以没展开**(这是证据的缺口,不是语法的):

| 渲染里留下的名字 | 文件 |
|---|---|
| `_ACRTIMP`、`_SAL2_Source_`、`_Check_return_` | `ucrt/corecrt_wio.h`、`ucrt/corecrt_wstring.h` |
| `_Success_ ( return != 0 )`、`_Ret_writes_z_ ( 26 )` | `ucrt/corecrt_wtime.h` |
| `_Struct_size_bytes_ ( Size )` | `um/ncrypt.h`、`um/winioctl.h`、`shared/ktmtypes.h` |
| `_Post_equal_to_`、`__inline` | `arm_neon.h`、`arm64_neon.h` |

两件事由此变得清楚,而且都有数字支撑:

1. **`_ACRTIMP` 是对象宏,却被跳过了** —— 它的 `#define` 在一个条件区域里,所以只从 **in-force 通道**到达,而
   那条通道**不带"是不是函数宏"**。给通道补上这个标志,`_ACRTIMP` 这种就能回来,`_STL_MSG` 那种继续跳过:
   这是下一步里最便宜、收益最直接的一条。
2. **SAL 那一批必须等参数表**:`_Success_(return != 0)` 要按参数替换,证据里没有参数名就没法做。要动
   `MacroFact` 的编码 → `CODEC_VERSION` bump。

### 函数宏的**参数表**已进证据(不需要 `CODEC_VERSION` bump)

原先以为这一步要动 `MacroFact` 的编码(加一个字段 → 版本 bump)。**不用**:事实里已经有**体**的范围
(`body_range`),而参数表就是**紧挨着体、在它之前的那一组括号**。于是遍历时从 `body_range.start_offset`
**向前做一次配平扫描**就够了——这**不是**搜索,而是精确的:标准里宏的参数表只允许标识符、逗号、`...` 和空白
(没有字符串、没有注释、参数里也不会嵌套括号),所以配平扫描就是全部规则。

- `IncludedMacro.parameters` / `MacroEnvironment::parameters_of` / `parameters_in_force`:证据带着它走;
- `InForceBody` 多了 `parameters`,并新增一条 4 元组 `From`(旧的三条 `From` 一律给 `None`,即"没人说过");
- 熟读的判据变成:**对象宏可用,或者函数宏且参数表在手**(`definition_text` 拼出 `NAME(params) body`,再交给
  指令层同一个 `parse_define`);
- 于是"函数宏没参数名"这个计数从**一整类**缩成"证据里确实没有的少数"。

**实测**(同一命令,只差开关):

| | 默认(开关关) | `--in-force-bodies` |
|---|---|---|
| 255 文件 | 242 干净 / 99 条 / 3 族 | 241 / **83** / 7 |
| 109 文件 | 100 / 74 / 2 | **99** / **69** / 6 |

默认路径**一个数没动**(这正是要的:证据变丰富不该动没打开开关的读数)。开关打开后,109 语料**多干净一个文件、
少七条消息**;255 语料干净数不变、消息数反而从 71 涨到 83——因为 SAL 那一批现在**真的展开**了,而展开之后
露出来的正是那三条语法缺口。**证据侧到这里基本做完,剩下的是语法。**
### 剩下三条的**最小复现与边界**(下一轮从这里开始)

把它们缩到最小之后,边界比原来的描述干净得多,而且两条**不是**我原先猜的形状:

```text
typedef void (*)(void);            干净
typedef void (__cdecl * p)(void);  干净      ← 组里有名字就没事
typedef void (__cdecl *)(void);    失败 "expected ;" 落在那个 `(`
```

**已修**。原因不是"抽象声明符里读不了调用约定",而是组里**没有名字**:那个判定认得的三条形状
(`(*f)`、`(WINAPI PM_OPEN_PROC)`、`(*STDAPICALLTYPE LPFN…)`)**全都要一个名字**,于是 `( __cdecl * )` 落到了
抽象声明符那条路上,读了 `(` 就在宏名上停住,把 `( __cdecl * )` 留成裸 token。加的是第四种形状
`( 宏 运算符 )`:靠**宏的拼法**(`written_like_a_macro`)与"后面跟着参数表"两条守住,普通名字的 `(x *)` 不受影响;
读取端相应地允许组里**没有名字**(抽象拼法)。

**实测**:裸读 255 `253 → 251` 条、109 `208 → 206` 条、128 文件不变;**熟读多一个干净文件**
(255 `242 → 243`、109 `100 → 101`)。这是第一条让**默认**熟读读数动起来的语法修复。

(顺带记下上一次的失败:我先前把分支加在 `parse_abstract_declarator` 的循环里,**三份普查一个数都没动**,
因为它压根没被执行到——于是撤了。修对了地方才看得见读数,这条经验比补丁本身有用。)

**placement new 的实参以 `::` 开头 —— 缩到了"只在 `new` 里"**:

```text
void g() { T(::x); }             干净      ← 语句里的构造/调用,没问题
void g() { auto q = T(::x); }    干净
void g() { T t(::x); }           干净
void g() { new (p) T(x); }       干净      ← 不带 `::` 的 placement new,没问题
void g() { new (p) T(::x); }     失败 "expected ), but get ::" 落在 `::`
void g() { new T(::x); }         失败      ← 连 placement 都不需要
```

所以与 placement 无关(`new T(::x)` 一样失败),而是 **`new` 的类型那一侧把 `(::x)` 当成了参数表**:`new` 的类型由一个
type-id 读(`parse_type_id_for_an_allocation`),它的抽象声明符会把类型后面跟着的 `(` 读成**函数类型的参数表**
(`T(::x)` = 返回 `T` 的函数),`::x` 在那里读不动,错误就冒出来了——而**紧接着** `parse_new_initializer` 的存在
正说明那个 `(` 本该是**初始化器**。

**已修**。做法正是当时写的那个:给 type-id 加一个开关
(`parse_abstract_declarator_with(p, name_possible, read_a_parameter_list_as_the_type)`),分配的入口
(`parse_type_id_for_an_allocation`)传 `false`,于是**类型读完就停**——那后面的 `(` 是初始化器。

一处细节值得记下来:`(` 那一块有**两个**分支,而两个都属于"类型"。第二个("参数表就是类型",即 `void (int)`)是
我第一版只关掉的那个,**y1 一样失败**;把整块都关掉之后 `new T(*q)`、`new T(::x)` 才通。以列位置为线索逐层缩小
(`cpp_dump --tree` 里 type-id 把 `( :: x ) ;` 整个吞进了一个 `Declarator`)才看清是第一个分支
(`a_parenthesised_abstract_declarator_follows`)认领了 `(*q)`——而**这个原因早就写在测试里**:

> `an allocation initialiser is not a parameter list`: "`a_parenthesised_abstract_declarator_follows` claims the
> group first — a `*` right after a `(` is a parenthesised declarator, `void (*)(int)` — so `(*q)` is read as a
> declarator and the `q` inside it has nowhere to go"

那条测试还带**自检**:构造被读通之后它会说 "these constructs parse now, so move them into the list of what is
read"。这次它就是这么报的——于是把它移进"读得通"的清单,并补了一条形状断言(`new T(*q)` 里那个组是初始化器,
不是 `FunctionType`)。

**实测**:

| | 默认(开关关) | `--in-force-bodies` |
|---|---|---|
| 255 文件 | 243 干净 / 97 条 / 3 族 | **242** / **78** / 6 |
| 109 文件 | 101 / 72 / 2 | **100** / **64** / 5 |

默认路径**一个数没动**(这个形状在裸文本与默认熟读里没出现);开关打开后**两边各多一个干净文件、各少五条消息**,
而开关模式的差距从"241 对 242"缩到"242 对 243"。

### 最后一条:未知名字后面的 `<` 是模板实参还是小于号

缩到头之后,这条与 `tuple` 本身无关,而是**名字 + `<` 的两种读法**:

```text
F<g<D> == 1>            失败 "expected primary expression" 落在 `==`
F<g<D>::value == 1>     失败
F<sizeof(D) == 1>       干净      ← 只有因为它作为比较链恰好合法
F<(g<D>::value == 1)>   干净      ← 括号把读法定死了
template <class D, class... S>
  constexpr bool v = F<tuple_size_v<D> == sizeof...(S), D, S...>;   失败   ← tuple:33 的真实形状
```

原因不是实参规则(`parse_template_argument` 先试类型、再退回表达式,两者都没问题),而是**更外层的决定**:
`F` 是一个未知的名字,于是 `<` 按 C++ 的规矩读作**小于号**,整串成了比较链
`((F < g) < D) > (== 1)`——而 `==` 没有左操作数,于是报 "expected primary expression"。
`sizeof(D)` 那一条"干净"是**巧合**:比较链 `F < sizeof(D) == 1 >` 恰好是合法表达式。

**已修**。规则落在**实参**这一层而不是名字那一层:`parse_template_argument` 先试类型、失败才退回表达式,而
`g<D> == 1` 的类型读**成功**了(停在 `==`),于是那个停点被当成了实参的结尾。加的是
`continues_an_expression(kind)`:类型读完之后如果跟的是一个**根本不可能出现在类型里**的运算符,这个实参就是
表达式,交给下面那条路。

**这个谓词的第一个版本把 `>` 和 `<` 也列了进去,代价立刻显现**:裸读 libstdc++ 128 文件语料从
**128 干净 / 0 条**变成 **74 干净 / 865 条**(其中 430 条 `expected a template argument`)——因为 `>` 是**列表的
结束符**、`<` 开的是嵌套列表,列进去等于拒绝了**每一个**类型实参。`*`/`&`/`&&` 同样不能列:它们修饰类型和做乘法
一样自然(`S<int*>`、`S<int&>`、`S<int&&>`),`>=` 也不能——收尾的 `>` 后面跟 `=` 由 `split_closing_angle` 先切开。

**实测**(每一档都无退步):

| 语料 | 修前 | 修后 |
|---|---|---|
| 裸 255 | 217 干净 / 251 条 | 217 / **244** |
| 裸 109 | 84 / 206 | 84 / **199** |
| 裸 128 | 128 / 0 | 128 / 0 |
| 熟 255(默认) | 243 / 97 | 243 / 97 |
| 熟 109(默认) | 101 / 72 | 101 / 72 |
| 开关 255 | 242 / 78 | 242 / **73** |
| 开关 109 | 100 / 64 | 100 / **59** |

**影响面**(先说清楚,免得高估):`tuple` 在**默认**熟读里本来就是干净的(失败的是开关打开后的那一档),
所以这条修好之后受益最大的是 `--in-force-bodies` 那一档;但它在**裸读**上也减掉了 14 条消息(两个语料各 7 条),
这是它与前两条不同的地方。
### B135 的另一半:模板声明里的括号函数名(已修)

`template <class _Ty> constexpr _Ty (max)(_Ty a);` 读不动,原因与 B135 当初那个一样,但门槛在另一边:
那条分支有两半 —— "**在光标处看到 `(` 且还没有类型名**"和"**整形状是 `运算符* ( 声明符 ) (`**"。模板声明里
说明符序列已经读到了一个**真的类型名**(`_Ty`),前半关了;`(max)` 前面又没有运算符,后半也关了。现在后半的
条件放宽成"**没运算符时,组里必须是一个光名字**":`T (max)(T a)` 正是 `T max(T a)` 的另一种拼法,而同形状的
`Widget w(1, 2)`、`T (a, b)`(直接初始化)、`int main(argc, argv)` + 声明(K&R)组里都装不下"一个光名字"。

**实测**:五份默认普查**一个数都没动**(裸 255 `217/253`、裸 109 `84/208`、熟 255 `242/99`、熟 109 `100/74`、
裸 128 `128 干净`),因为这个形状只在**用了 in-force 的体**之后才出现——开关关着的时候,那些 STL 头本来就
读得通。把开关打开再测(新增 `std_probe --in-force-bodies`):**241 干净 / 81 → 71 条 / 6 族**,即这条修复在
那个模式下减掉 10 条消息,但干净数仍是 241(关着是 242)。所以默认照旧关闭,而四个真缺口的清单现在是**三条**。
### 删形状规则还差什么:`cooked_index` 量出来的那道分界

开关定了之后本可以开始删规则,但先量了一个更基本的问题——**熟读能不能直接当分析层的解析结果用**?
(`examples/cooked_index.rs`:同一个文件,一次用**自己的文本**建摘要,一次用**熟流的渲染文本**建,再比声明名)

```text
files 128 | same declaration names 42
names only in the raw reading 2595 | only in the cooked reading 1516
cooked fact ranges mapped back into the file 9847 | not mapped 0
```

**管道没问题**:渲染建出来的摘要,9847 条事实的 range **全部**能经 `written_span` 映射回文件(0 条落空)。
这是 M5 要的那件事,第一次对**每一条事实**而不是一条诊断验证过。

**但两边的读数本来就不同,而且是设计使然**:裸读索引的是文件**写了什么**(每个 `#if` 的两支、宏体里的声明),
熟读索引的是编译器**看到什么**(一支、宏已展开、指令消失)。所以渲染建出来的摘要**不能顶替**文件建出来的摘要——
上面这两个数字就是证据。这正是架构自己的分工(§2.5 与 §4 的功能归属表),现在是**量出来的,不是假设的**。

**于是删规则的前提变得清楚了**:形状规则服务的是**裸读**,而索引今天建立在裸读之上。要删它们,得先把
"索引需要什么"和"读法需要什么"分开——索引要的是"文件写了什么"(含未编译分支,靠 `Visibility` 标注),
读法要的是"编译器看到什么"(熟流)。这两件事分别由**两个结构**回答,而不是由一棵树同时回答。

### 结论:开关**已改为默认**,理由是"仪器修好之后的读数"

修好上下文之后的同一批普查(255 文件,档 2):

| | 保守读法(开关关) | **默认(开关开)** |
|---|---|---|
| 干净 / 消息 / 族 | 244 / 95 / 3 | **245 / 67 / 4** |
| 109 文件 | 101 / 72 / 2 | **103 / 50 / 3** |

干净文件**多了**,消息**少了 28 条**(255)和 22 条(109)。这正是当初给这个开关定的判据("开关在干净文件数上不为负"),
于是 `configuration_from_environment` 现在传 `true`;想要保守读法的调用者用
`configuration_from_environment_with(environment, false)` 明确地问。断言也跟着改:两条路都有测试
(`an_object_like_body_in_force_is_used_by_default_and_can_be_turned_off`)。

**这不是开关变了,是测量变了**——两件事同时发生:语法缺口被修掉(展开不再撞墙),以及**上下文**从"字母序第一个
包含者"换成"种子 TU 真正走的那条链"(下一节)。
### 那"六换六"里的六个,单独跑**全部干净** —— 代价是**上下文**的,不是文件的

把 `cstdint`、`utility`、`tuple`、`new`、`type_traits` **各自单独**放进清单再跑开关模式:

```text
utility        files 1 | clean 1 | messages 0     (859 行)
tuple          files 1 | clean 1 | messages 0     (941 行)
new            files 1 | clean 1 | messages 0     (122 行)
type_traits    files 1 | clean 1 | messages 0     (2360 行)
cstdint        files 1 | clean 1 | messages 0     (97 行)
```

**五个全干净。** 所以"开开关会弄坏六个文件"这个说法**不准确**:它说的是"这六个文件在**某个上下文里**读不动"。

`std_probe` 的上下文是这么来的(`examples/std_probe.rs:203`):

```rust
let mut includers: HashMap<PathBuf, (PathBuf, usize)> = HashMap::new();
// 按**排序后**的路径遍历,`or_insert_with` ⇒ 第一个包含它的文件胜出
```

也就是说:**每个头文件的"谁包含了它"取的是全语料里按路径排序的第一个**,再把那个包含者在 `#include` 之前
的宏(偏移 0)喂进来——`macros_in_force_before_the_include`。这个选择**确定**(所以读数一直稳定,那段注释里
还记着一次"数字自己会动"的教训),但**任意**:真实的翻译单元里,包含者是另一个文件。

**这件事改变了两件事**:

1. **"六换六"这张表要按新说法读**:换成"六个文件在**语料挑的那个上下文**里读不动,单独读没问题"。开关的代价
   因此**比表上小**,而收益(六个 UCRT/SDK 文件读对)是**文件的属性**。
2. **仪器本身要修**:正确的上下文是**真实翻译单元**(这条语料本来就是从一个包含 `<windows.h>` 的 TU 生成的,
   清单第一行就是它),而不是"按字母序第一个包含者"。下一步要么把种子 TU 当唯一上下文,要么**同时报两档**
   (有上下文 / 无上下文),让"是文件的错还是上下文的错"一眼可分。
### 用 `using` 写同一个类型:抽象声明符那一侧也认调用约定(已修)

上一轮修的是**声明符**那一侧(`typedef void (__cdecl *)(void);`),而 `new:111` 是**别名**:

```cpp
using new_handler = void (__cdecl *)();      // 走 type-id,那里的分组是**抽象**声明符
```

那条谓词(`a_parenthesised_abstract_declarator_follows`)只认 `(` 后面直接跟 `*`/`&`/`&&`/`::` 或
成员指针,于是 `( __cdecl * )` 落空、报 ``expected a type specifier``。加的是同一个形状
`( 宏 运算符 )`,**但这次要求后面跟着参数表**——这个要求是被数据逼出来的:

```text
(_Rng&&)    是**参数表**:一个参数,名字恰好写成实现宏的样子
```

少了它,`template<borrowed_range _Rng> subrange(_Rng&&)` 这类被认领成声明符,libstdc++ 128 文件语料
从 **128 干净 / 0 条**掉到 **125 / 3 条**(``expected ), but get identifier``)。调用约定是**修饰**某样东西的,
所以它所在的分组后面一定是被修饰者的参数表——这条不是审美,是 3 个文件。

**实测**(每档都无退步):

| 语料 | 修前 | 修后 |
|---|---|---|
| 裸 255 | 217 干净 / 244 条 | **218 / 242** |
| 裸 109 | 84 / 199 | **85 / 197** |
| 裸 128 | 128 / 0 | 128 / 0 |
| 熟 255(默认) | 243 / 97 | 243 / 97 |
| 熟 109(默认) | 101 / 72 | 101 / 72 |
| 开关 255 | 242 / 73 / 6 族 | **243 / 72 / 5** |
| 开关 109 | 100 / 59 / 5 | **101 / 58 / 4** |

**开关模式的干净文件数现在与默认持平(都是 243),而消息数少 25 条(72 对 97)。** 这正是上一轮给"要不要开开关"
定的判据;剩下的差别只有族的个数(5 对 3),所以下一次可以正式做这个决定。### 开开关这一步:先摊开代价(**还没开**)

三条语法缺口修完之后,打开开关的代价与收益可以**逐文件**列出来了(255 文件 SDK+STL,档 2,只差
`--in-force-bodies`,两档各有 11 个失败文件):

| 开关**修好**的 | 开关**弄坏**的 |
|---|---|
| `corecrt_wstring.h`、`corecrt_wio.h`、`corecrt_wtime.h`、`stat.h`、`ctype.h`、`winnt.h` | `cstdint`、`new`、`type_traits`、`utility`、`tuple`、`processthreadsapi.h` |

消息数 97 → 73、干净文件 243 → 242。**六换六,今天不划算**,所以默认照旧关闭。

但两边的性质完全不同,这才是要不要开的关键:

- **被修好的六个**:失败的原因是"宏体没人知道",开关给了答案 → 它们从此**读得对**;
- **被弄坏的六个**:失败的原因是"展开**之后**露出来的语法/配置问题"——也就是说,开关把一个"因为读不懂宏名所以没报错"的文件,变成了一个**说了实话**的文件。

第二轮定位把"弄坏的六个"分成了两类(逐条缩,而不是猜):

1. **语法缺口**(展开把一段合法 C++ 拼了出来,而语法读不动);
2. **我们取错了体**:`cstdint:54-55` 是
   ```cpp
   #if _HAS_TR1_NAMESPACE
   namespace _DEPRECATE_TR1_NAMESPACE tr1 {
   ```
   而 `yvals_core.h` 里 `_DEPRECATE_TR1_NAMESPACE` **有两条定义**(906 行是空的,908 行是另一条,由配置决定哪条在效力)。
   开关打开后渲染出来的是 `namespace :: tr1 {` —— 而 `namespace ::tr1 { }` **不是合法 C++**(命名空间定义的
   名字必须是标识符,probe 过),所以错的是**我们挑了哪条定义**,不是语法。这一类是**证据侧的缺陷**,可以修。

所以顺序是:**先把"弄坏的六个"分成上面两类**(已有工具:`std_probe --cooked --in-force-bodies` 给出渲染片段,
逐个形状缩到最小),**把第二类修掉**,再看第一类剩几条 —— 那时开开关的账才算清。开关默认关闭,直到那笔账
在**干净文件数**上不再是负的。

**旁证**:另外两条也缩过,但**在孤立的小例子里是干净的**——`tuple:853` 的
`struct _Tuple_cat2<_Ty, index_sequence<_Kx...>, index_sequence<_Ix...>, _Ix_next, index_sequence<_Kx_next...>, _Rest...>`
单独读没问题,`Is_memfunptr<_Ret(__cdecl _Arg0::*)(_Types...) &>` 也没问题。**"到文件里却失败"说明触发点不在
这段文字里**——而熟读的错误位置是**渲染**里的 offset(经 `written_at` 换回文件行列时可能被夹到文件末尾),
所以下一次不要按行列去找,要按**渲染片段**去找。
### in-force 通道的 `function_like`:机制已就位,默认**关闭**(测出来的)

`InForceBody` 把三样东西一起交给通道:**名字、`Option<bool>` 的 function_like、体**。`Option` 而不是 `bool`,
因为两个调用者知道的东西不同:索引知道(`MacroFact::function_like`),只带文本的探针不知道——而 **`None` 不是
"对象宏"**,它是"没人说过",于是按不可用处理。`Configuration::in_force_without_a_parameter_list` 数出来的
就是"留下了多少"。

打开它(`configuration_from_environment_with(environment, true)`),同一批普查:

```text
255 文件:干净 242 → 241 | 消息 99 → 81 | 族 3 → 6
109 文件:干净 100 →  98 | 消息 74 → 76 | 族 2 → 6
```

它**修好了七个 `_ACRTIMP` 开头的文件**(`corecrt_wstring.h`、`corecrt_wio.h`、`corecrt_wtime.h`、`stat.h`、
`stdlib.h`、`ctype.h`、`winnt.h`),同时**弄坏了另外几个**(`xstring` 在内)——展开一个体,就把体后面那个
语法缺口露出来了,与前面四个真缺口是同一个故事。

**干净文件数下降不是这一层有权自己做的交易**,所以开关默认关闭:代码在、两条路都有断言,等那四个真缺口修完
再打开。这就是"先量后改"在这条线上的样子——机制就位,决定留给数字。

**删规则的位置也更清楚了**:等上面两条做完,剩下要删的是"注解站在说明符/声明符位置"那一批形状规则
(B133/B134/B136),而**真语法缺口那四条**(调用约定、placement new、模板实参表达式、`_Ty(max)(…)`)与展开无关,
它们该由语法自己修——这正是 M4 之后 parser 那一侧的清单。

**一条纪律**:熟流的树**没有无损性**(指令没了、每个条件只留一支),所以普查在 `--cooked` 下必须跳过
`to_source_text() == source` 这条断言——无损性是**原始 token 流**的属性(`CppSyntaxTree::get_tokens`),
不是渲染文本的。`std_probe` 里现在就是这么写的。

### 一轮五改:$ 与宏名、名字空间属性、指针操作数里的宏、函数宏的定义、以及一个拼写谓词的假阳

这一轮的账,五条**读数**改动 + 若干**仪器**修理。读数从 255 熟读 **245 干净 / 67 消息 / 4 族** 走到
**252 / 25 / 2 族**,109 熟读 **103 / 50** 走到 **107 / 23**,455 熟读 **446 / 43** 走到 **450 / 8**;裸读
也一起动了(255:**218 / 242 → 223 / 223**,109:**85 / 197 → 89 / 179**,因为其中两条本来就是**静默误读**)。

1. **`$` 是名字字符(词法默认改了)**。`LexerConfig::dollar_in_identifier` 原本默认关,理由是"严格的解析器
   该报它"。这跟本项目的取向冲突,而且有语料:Windows SDK 自己的 SAL 头 `specstrings_strict.h:1130` 把宏
   **名字**写成 `__$allowed_on_return` 这种拼法,`#define __$allowed_on_return /* empty */`。拒绝 `$` 不是报
   一个字符错,而是把 `__$` / `allowed_on_return` 拆成三个 token——**整个定义连同它的每一次使用一起丢掉**,
   熟读里表现成 `_Always_ ( $ allowed_on_global_or_field )` 这种残留。GCC/Clang/MSVC 默认都接受,所以"常见
   读法"就是接受:默认**开**,标准那一侧留一个 `with_dollar_in_identifier(false)`。实测:455 熟读 43 → **12**
   条消息,`processthreadsapi.h`、`winerror.h` 直接干净,没有文件变差。
2. **名字空间可以带属性**:`namespace attribute-specifier-seq(opt) identifier`(C++17)。MSVC STL 有 **12 个**
   头文件用 `namespace _DEPRECATE_TR1_NAMESPACE tr1 {`,而这个宏就是 `[[deprecated("warning STL4002: …")]]`
   (`yvals_core.h:908`)。**裸读看不出来**:没展开时 `_DEPRECATE_TR1_NAMESPACE` 只是个标识符,被当成名字空间
   名读了,`tr1` 又被"名字与 `{` 之间的宏"那条规则吃掉——歪打正着。展开之后是
   `namespace [[deprecated(…)]] tr1 {`,`[` 不是名字,整段报 `expected primary expression`(`cstdint`、`tuple`
   就是死在自己的 TR1 块上)。修法是把 `parse_attribute_specifiers` 接在 `namespace` 之后,`at_an_attribute`
   保证普通名字不受影响。
3. **指针操作数里没展开的宏,两个方向都要认**:`*RESTRICTED_POINTER PRKCRM_MARSHAL_HEADER`
   (`shared/ktmtypes.h:188`,同一个头写了 11 次)和 `, NEAR * NPMMTIME , FAR * LPMMTIME`
   (`um/mmsyscom.h:138`,还有 `mciapi.h`/`mmiscapi.h`/`shared/rpcdcep.h`)。`RESTRICTED_POINTER` 是
   `winnt.h:110` 的 `__restrict`(另一个目标上是**空**定义),`NEAR`/`FAR` 是 `minwindef.h:150` 的
   `near`/`far`,而 `near`/`far` 自己又是两条定义更上面的空宏——编译器读到的是 `* NPMMTIME`。
   两条臂都在抽象声明符的指针循环里,**各自要求一个跟随者**(`* MACRO name` 要第二个名字,`MACRO *` 要那个
   `*`):`*MACRO name` 的臂还必须**已经读过一个操作数**(`container.is_some()`)——否则它会先一步把
   `(WINAPI PM_OPEN_PROC)` 里的第一个名字吃掉,那条调用约定的断言当场就红了。
   **`ktmtypes.h` 的裸读本来就是"干净"的**,只是干净得不对:声明符把自己命名为 `RESTRICTED_POINTER`,typedef
   没等到它的 `;`,于是 `PRKCRM_MARSHAL_HEADER ;` 变成一条**独立声明**——树良构、无损、零诊断,两个名字全错。
   熟读里同样的 token 报 ``expected `;` ``,因为那里没有那个凭空出现的 `;`。这就是"只问能不能解析"的盲区,
   所以断言写的是**哪些 token 成了名字**。
4. **函数宏的定义现在能用了(前提:参数表在证据里)**。原来凡是 `function_like` 的定义一律跳过,注释写着
   "证据里没有参数名"——那是参数表进证据**之前**的话;`definition_text` 现在能把
   `#define NAME(params) body` 原样写出来,`parse_define` 再读回去,按参数替换就是它本来的意思。仍然跳过的
   只有"函数宏但参数表不在证据里"那一种(写成对象宏会把 `#` 和参数名贴进流,§6 记着那次事故)。
   **这一改的价值远超它看起来的样子**,而且差点被数字骗过去:同一份语料两侧是 249 干净 / 49 消息(旧)对
   248 / 70(新)——但 A/B 一渲染就看见,**旧侧把 255 个文件里的 158 个渲染成了空**,新侧 103 个:那 55 个
   文件的正文(含 `windef.h`、`winbase.h`、`wingdi.h` 一整片 SDK)从头到尾没被读过,它们"干净"是因为**空的**。
   原因是 `#if WINAPI_FAMILY_PARTITION(...)` 这类条件要用函数宏才算得出来,算不出来时整块正文不是活的。
   于是"干净"这个数必须和"渲染里还剩多少内容"一起看——这一行现在是普查输出的一部分。
5. **一个拼写谓词的假阳:`(C& r)` 被判成了声明符组**。`a_parenthesised_declarator_with_a_name_follows` 有一条
   臂管 `(MACRO & name)`(调用约定/注解套在声明符操作数上的那种写法),守卫是"第一个名字写得像宏"——而这个
   判据是**拼写**:`C`、`T`、`M`、`X` 单个大写字母就满足,`_Container_base12`、`_Ty` 因为下划线开头也满足。
   于是最普通不过的参数表 `(C& r)` 被整组当成声明符,`C` 成了宏、`& r` 成了它修饰的东西,整个定义掉回
   表达式语句,报在它自己的 `void` 上——所以原因看起来跟参数毫无关系。**修法是跟随者**:声明符组后面跟的是它
   修饰之物的后缀(`(WINAPI PM_OPEN_PROC)(LPWSTR)`),参数表后面跟的是 `)`、`,`、限定符或函数体。
   这一条是 MSVC STL 里最后一个**非缺口**的失败(`xmemory` 的
   `_Container_base12::_Swap_proxy_and_iterators_unlocked(_Container_base12& _Right)`)。

**仪器侧的修理**(同样的钱,但省的是下一轮):

- `--render-to <dir>` 把**每个**文件的渲染写出来:读数变化的问题是比较式的("这个文件的展开变成了什么"),
  被修好的文件和被弄坏的文件一样是答案的一部分(第一版只写失败文件,答不了这个问题);
- 熟流**只 cook 一次**:失败分支原来为了取错误窗口又 cook 了一遍,既让失败文件贵一倍,又留下了**两份配方**,
  改一处就会让打印出来的窗口描述一个解析器没见过的流;
- 行号索引拒绝的位置**不再静默跳过**:原来 `continue` 会把文件从"首错清单"里丢掉却留在失败计数里(查这个
  疑点时发现 255 语料其实 37 个失败全部打印了,是我自己的 grep 不吃四位行号——修理照样留着,因为在报告里
  沉默永远是错的答案);
- `table:` 四个计数进普查输出:证据里有、却进不了宏表的定义各有多少条——"宏没展开"的三种不同原因。

**这一档剩下的三个文件**(255 熟读 25 条消息 / 2 族):

| 文件 | 首错 | 形状 |
|---|---|---|
| `codeanalysis/sourceannotations.h:69` | `expected primary expression` | `[source_annotation_attribute(SA_All)] struct …` —— **单方括号**的 MIDL/SAL 注解,不是 `[[…]]`;先要查清这些行在真实编译里是否活的 |
| `tuple:855` | ``expected `;` `` | `_Tuple_cat2<…, index_sequence<_Ix...>, _Ix_next, index_sequence<_Kx_next...>, _Rest...>` 偏特化里的包展开 |
| `shared/rpcdcep.h:183` | ``expected `;` `` | `typedef void RPC_ENTRY RPC_ADDRESS_CHANGE_FN(IN void* arg);` 后面的 `typedef void …`(待切成最小复现) |

### M4 —— 文件 CST 降级
裸树只保留:无损、括号/指令结构、浅层声明扫描。此时它那侧的门禁放宽到"结构正确",不再要求 C++ 正确。
**验收**:宽容语法里针对展开的谓词清零。

### M5 —— 双向映射 + 作用域入口
补全/悬停/跳转改走 §2.6 的路径;文件 offset 的直达路径只留给不需要语法的功能。

---

## 7. 度量与门禁

每次改动后必须全绿(测试基线:**1193 个测试**,实测;`cargo clippy --workspace --all-targets`
零警告,`cargo doc` 零警告,`cpp_dump` 零错误):

```bash
cargo test --workspace
cargo clippy --workspace --all-targets        # 零警告
cargo doc --no-deps -p cpp_code_analysis      # 零警告
cargo run -q -p cpp_parser --bin cpp_dump -- crates/cpp_parser/tests/real_world.cpp   # 零错误
```

四个语料 + 两个端到端。**每个语料必须记下它是哪一档跑出来的**(§5):同一个 255 文件语料,档 0(只有文本)
是 206 干净 / 995 消息,档 2(闭包)是 217 干净 / 253 消息——这正是"缺宏知识"值多少的度量,把两档混着比
就是拿两个问题互相回答。命令形状:

```bash
# 档 0:只有文本
std_probe <list>
# 档 2:闭包(种子 TU + --closure)
std_probe <list> --seeds --closure
```

| 语料 | 档 | 方式 | 现状基线 |
|---|---|---|---|
| libstdc++ 128 文件 | 0 | 裸读 | **128 干净 / 0 消息** |
| libstdc++ 455 文件(闭包清单) | 0 | 裸读 | **454 干净**(1 个文件 4 条) |
| libstdc++ 455 文件 | 2 | 熟读 | **450 干净 / 8 消息 / 3 族** |
| Windows SDK + MSVC STL 255 文件 | 2 | 裸读 | **223 干净 / 223 消息** |
| Windows SDK + MSVC STL 255 文件 | 2 | 熟读(默认=level 2) | **252 干净 / 25 消息 / 2 族** |
| MSVC STL + UCRT 109 文件 | 2 | 裸读 | **89 干净 / 179 消息** |
| MSVC STL + UCRT 109 文件 | 2 | 熟读(默认=level 2) | **107 干净 / 23 消息 / 2 族** |
| `std_query` 两个方向 | 2 | — | 9/9 |
| driver 声明查询 | 2 | — | 9/9 |

熟读那一档的"保守读法"(不采信 in-force 通道的体)用
`configuration_from_environment_with(environment, false)` 问,数字见 §6 M3 的开关一节。

**熟读的读数必须和"渲染里还剩多少内容"一起读。** 一个文件的整个 body 落在未取分支里时,渲染是**空的**,
而空文件没有错误——于是只数"干净"的普查会把**没读过的文件算成读过了**。这是量出来的:一次 A/B 的两侧
是 249 干净 / 49 消息 与 248 干净 / 70 消息,而**好看的那一侧把 255 个文件里的 158 个渲染成了空**
(另一侧 103 个),也就是说那 55 个文件的正文从头到尾没被看过。`std_probe --cooked` 现在把这一行印出来
(`rendering: N of M files rendered to nothing`),同一行还有 `table:` 四个计数——证据里有、却进不了宏表的
定义各有多少条,那是"宏没展开"的三种不同原因。

列表文件由 `std_probe --seeds --closure` 或编译器的 `-M` 输出生成,**不进仓库**。

**迭代成本**(同一台机器,release):255 文件 SDK 语料**一次读数约 4 分钟**(构建位置化宏证据 2.5M seeds
≈ 140 s,熟流解析 ≈ 150 s),455 文件闭包 ≈ 40 s,109 文件 STL ≈ **12 s**。所以改动先用 109 文件那一档
迭代、再用 255/455 复核:一轮从 20 分钟降到 20 秒。想让 255 那一档也快,只有两条路——把普查按文件并行,
或者按 `READING_FINGERPRINT` 把每文件的环境落盘缓存;两者都是**普查**的事,不是语法的事。

纪律:

- **每一条读数变化都要有形状断言**(`crates/cpp_parser/tests/`),只报数字的改动不接受;
- **按文件 diff**,不看总数:一个文件变好、另一个变差,总数会互相掩盖;
- **`cargo fmt` 不是门禁**(这个仓库是手写的约 110 列格式);
- 源码改动**不 bump `FORMAT_VERSION`**:`READING_FINGERPRINT`(build.rs 对 `cpp_parser/src` +
  `cpp_code_analysis/src` 取哈希)会让旧缓存自动不可达;`CODEC_VERSION`(现在 **14**)只在缓存**编码布局**
  改变时才 bump。

---

## 8. 已知的坑(代价换来的)

1. **同一套空白谓词必须被派发和扫描共用。** 把 `\u{c}`/`\u{b}` 加进词的**派发**却没加进 `eat_while`,会产出
   零长 `Whitespace` token、offset 不前进——**一次 48 GB 分配**。同理,词法的每一个"什么算空白"只能有一个谓词。
2. **两条宏证据通道不可互换。**
   - 位置通道:`macro_body_kinds_at(name, offset)`(这个 offset 之后生效的体);
   - 定义通道:`macro_evidence` / `IncludedMacro::defined_with_body`(某个头定义了什么)。
   两次事故(`_EXPORT_STD`、`_ACRTIMP`)都是因为拿一条通道的答案去回答另一条通道的问题。
3. **`peek_*_at` 的 offset 是相对游标的。** 用绝对索引传进去,谓词会静默失效——读数一动不动,看起来像
   "这条规则没用"。
4. **报告里要打完整路径。** `winnt.h` 在 `um\` 和 `shared\` 各有一份,按文件名看会得到错的行号。
5. **无工具链的普查有一部分是环境假象**:条件全 `Unknown`、`$` 被拒、宏体为 0 条。修之前先确认它是不是缺漏
   (§5)。
6. **展开行为的 ground truth 是编译器**:`clang -E`(或 `cl /E`)。我们的熟流要和它对齐,而不是和我们的
   直觉对齐。

---

## 9. 本机环境(只影响测量,不影响代码)

- 未固定时自动发现 MSVC `cl.exe` **14.35.32215**;
- MinGW 的 `g++` 需要用 `$env:CXX` 固定;
- 语料清单与普查脚本在临时目录,不进仓库(§7)。

---

## 10. 现状

**已有**(可跑、有测试):

- **M0 已完成**:`cpp_parser::lex` 是唯一的词法入口(parser 通过它拿 token),`CppSyntaxTree::get_tokens()`
  把这次解析用的 token 流交出来——token 流是一等产物,tree 是它的一个视图。唯一的例外写在断言里:
  `#include <vector>` 的 `<…>` 只有语法知道那里该是头文件名,所以 **header name 的折叠是解析器做的**,
  token 流因此只在这一点上与 `lex` 不同(`tests/invariants.rs` 的 `the_parser_reads_the_stream_the_lexer_produces`
  逐 token 走两遍,只允许这一种差异)。
- **M1 已完成**:预处理层从 **token 流**读指令(`scan_directives(source, tokens)`),规则是语言自己的:
  `#` 是它所在**逻辑行**的第一个 token(`LineContinuation` 不算换行,所以 `#define F(a) \` 折行后的 `#a`
  不是新指令)。指令的 span 与树给的完全一致,包括尾随 trivia。
  **实测**(`examples/directive_scan_audit.rs`):128 文件 libstdc++ 与 455 文件闭包 **node only 0 /
  scan only 0**——语法读得通的地方两条规则是同一个规则;255 文件 SDK 语料 **node only 0 / scan only 6950**
  (49 个文件解析失败),也就是解析失败的地方旧路径把剩下的指令**藏起来了**。预处理层不再继承语法的失败。
- 词法器与 token 流:`CppTokenKind`、trivia、续行、原始字符串;
- 无损 rowan CST + 宽容语法(`cpp_parser`),含针对真实头文件的缺漏断言集;
- 分析层:预处理(include / 条件 / guard / 宏环境)、每文件摘要与缓存、项目索引、会话;
- **影子展开器**,带 `Origin` 调用链,目前只服务展开视图;
- LSP 壳:`cpp_ls`,诊断(push + pull)、definition、hover,真客户端握手测试。

**M0/M1 之后的读数**:四个语料 + `std_query` 9/9 + driver 9/9 **全部与迁移前一致**(§7),测试 1192 → 1193。

- **M2(文件级)已完成**:`cook` / `cook_with` 产出**熟 token 流**,外部判据是与编译器自己的预处理逐 token 一致
  (`examples/cook_vs_compiler.rs`,实测见 §6 M2)。
- 副产品:那个判据抓到并修掉了一个 `##` 的 range 缺陷(§6 M2 最后一段),以及 `Branch::holds` 现在被两个
  消费者共用(条件判断只有一处实现)。

- **M3 的机制已完成**:`render()` + `RenderedSpan` 映射表,真实树由现有语法在渲染文本上建出来(parser 未改)。
- **档 2 的宏表已完成**:`configuration_from_environment` 把 include 闭包给出的定义变成 `MacroDef`,**对象宏与
  "参数表在证据里的函数宏"都进表**;只有"函数宏但参数表不在证据里"那一种计数跳过(§6 本轮第 4 条有 A/B 与
  空渲染的教训),`std_probe --cooked` 因此能跑熟流普查。
- **熟流普查有了数字**:255 文件 **252 干净 / 25 消息 / 2 族**、109 文件 **107 / 23**、455 文件 **450 / 8**
  (裸读 223 / 89 / 454),本轮五条读数改动与仪器修理见 §6 末尾那一节;剩下的失败逐条在表里。
- **映射表的第一个真实消费者**:`std_probe --cooked` 用 `written_at` 把渲染里的错误位置换回文件行列。
- **熟读的读数必须和"渲染里还剩多少内容"一起看**:空渲染没有错误,只数"干净"会把没读过的文件算成读过了
  (实测差 55 个文件),`std_probe --cooked` 现在把这一行和宏表的四个计数一起印出来(§7)。

**下一步(按依赖排序)**(更新到本轮之后):
1. **熟读那三个文件**(§6 本轮末尾的表):`sourceannotations.h` 的单方括号注解先查"真实编译里是否是活的",
   `tuple` 的包展开偏特化切成最小复现,`rpcdcep.h` 同样;
2. **TU 级拼接**:按 include 顺序缝熟流;
3. **删形状规则的位置更清楚了**:注解站在说明符/声明符位置那一批(B133/B134/B136)现在多半已经无用,
   但删之前要先把索引的两半分开(§6 `cooked_index`),因为形状规则服务的是**裸读**,而裸读仍是索引的输入;
4. 缓存(等有热的消费者);把 255 那一档的普查按文件并行或落盘缓存(§7 的迭代成本);
5. M4/M5:裸树降级、双向映射接进补全/悬停/跳转。
