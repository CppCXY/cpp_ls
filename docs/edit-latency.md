# 编辑延迟：一次按键的成本，和检查层的 43 倍

**结论先行。编辑路径上"重建 + 索引"已经是 10 ms 级；真正贵的不是索引，是诊断（`sema::check`）。**

```text
一次按键（LuaSource.cpp，6.5 KB）
  did_change                       0.1 ms     换 buffer、丢摘要、入队
  pump（重建 + 重新索引）          10.2 ms    cook materials 3.5 + render 4.8 + 第二遍 0.6 + parse 1.1
  补全请求                         15.3 ms    view 1.0 + 名字/成员列举 14.3
  诊断（客户端每次编辑都会拉）      340 ms     之前是 19 704 ms   ← 改前改后的全部差别在这里

四条真实工程上的诊断成本（每文件一次）
  LuaSyntaxTree.cpp    283 326 ms →  6 248 ms
  LuaSyntaxNode.cpp      1 314 ms →    319 ms
  四个文件合计         284 648 ms →  6 575 ms      （43×，findings 18 → 18，逐条相同）
```

本文只写**测出来的东西**和**改了什么**。所有数字都来自下面的两个探针，命令见 §五。

---

## 一、一次按键到底走了哪些步骤

服务器处理一次 `didChange` + 一次补全 + 一次诊断拉取，等于：

```text
did_change          Session::buffer_changed    换文本、算指令签名、丢摘要、入队、标记要 cook
pump 一个 slice     prepare_a_wave             解析（或从磁盘摘要命中）
                    commit_a_wave              插入摘要、把 include 入队
                    materials_for / render_them  cook = lex + 宏展开 + 解析渲染 + 建事实
                    prepare_the_drain            第二遍（被包含的头已入索引后重读文件）
                    commit_the_drain / commit_them  两次插入
request             view + completions         补全要走可见集和成员表
diagnostics         checks_about               22 条规则，读同一份 view + 索引
```

`examples/edit_loop.rs` 逐步计时，在 `EmmyLuaCodeStyle` 的 `LuaParser/src/File/LuaSource.cpp` 上：

```text
  did_change                0.1 ms
  pump                     10.2 ms
    parse（wave prepare）     1.1
    wave commit               0.1
    cook materials            3.5
    cook render               4.8     ← lex + 渲染 + 解析渲染 + 191 条 fact
    second pass（drain）      0.6
    drain commit              0.0
    cook commit               0.1
  write hold                0.2 ms   ← 查询要等的那个锁，一直是这个量级
  request                  15.3 ms   （view 1.0 + 补全 14.3）
  diagnostics             340.0 ms
```

也就是说：**重建 + 索引这一半已经只花 10 ms**，并且写锁只被持有 0.2 ms。用户在打字时感到的"卡"，
如果还剩下，来自诊断（和补全），不来自索引。

## 二、检查层：19.7 s → 0.34 s

`CPPLS_TIME_CHECKS=1` 是仓库自带的按规则计时（`sema/check/mod.rs::run_check`）。同一个文件，改前改后：

```text
规则                        改前      改后
assign-type-check         15 371 ms    135 ms
param-type-check           2 228 ms    121 ms
implicit-bool-conversion   1 115 ms     59 ms
access-invisible             375 ms     15 ms
argument-count               365 ms     10 ms
concept-not-satisfied          1 ms      1 ms
undefined-field                1 ms      1 ms
----------------------------------------------
diagnostics（整次拉取）   19 704 ms    340 ms      58×
```

四条根因，全部是"对每个成员/每个表达式重复做同一件昂贵的事"。每一条都先量、后改、再量。

### ① 每个成员重建一次 `DeclarationShapes` —— 927 ms 一个成员表

`fact_from_binding` 把一条绑定读成一条 fact，它的每个字段都要 `DeclarationShapes` 表
（`declared_type_of` / `declared_returns_of` / `declared_bases_of` / `parameter_list_at`），
而 `DeclarationShapes::of(root)` 是**一次整棵树遍历**。`direct_members` 对每个成员调用它一次：

```text
std::vector（libstdc++ <bits/stl_vector.h>，151 个成员）
  own 927 ms        ← 151 次建表
  bases 0 ms
```

`declarations.rs` 里 `_with` 形式的读取函数（`declared_type_of_with` 等）本来就是为这种调用准备的，
注释写着"每一个有不止一个问题要问的调用者都该用它"。改法：`direct_members` 建一次表，
`fact_from_binding_with(&shapes, …)` 走完整张表。

```text
std::vector 成员表   own 927 ms → 4 ms
```

### ② 为找一个 operator 建整张成员表 —— 340 ms 里 13 次相同的问题

`operator_returns_of`（给 `v[0]`、`a + b` 这类表达式求类型）原来的做法是：拿整个类的**成员表**，
再在表里找 `operator[]`。`LuaSource.cpp` 有 14 个下标表达式，每个都建一遍：

```text
members_of(std::vector)            24 ms
× 14 个下标表达式                 340 ms   ← assign-type-check 全部成本
```

一个"某个成员是否存在"的问题，索引里有**单成员**的窄查询（`member_fact`，按名字/scope 的倒排）。
改法：先问窄查询；`Known::Yes` 就直接用它（成员表按同样顺序遍历成员——本类优先、再逐层基类——
所以表里 `find` 到的必然就是这一条，答案相同）。只有窄查询答不出时才回落到成员表。

```text
operator_returns_of                  20 ms → 0 ms（窄查询命中时）
assign-type-check                   135 ms（其余是表达式类型推导本身）
```

顺带记一次**做过又撤回**的改动：`declared_returns_of_with` 只接受 `BindingKind::Function`，
所以 operator 的 fact 从来没有返回类型。放开它看起来是明显的修好，实测是**错误**：
`std::vector<Token>::operator[]` 的返回类型写作 `reference`，而 `reference` 是类内的 typedef，
未替换的名字让 `tokens[i]` 的类型变成"一个叫 `reference` 的类"，
`undefined-field` 因此报出**四个文件 60 条误报**。这条路径要保持"答不出就不答"，
记录在 `declared_returns_of_with` 的注释里，免得下一个人再试。

### ③ 模板参数被当成基类 —— 355 ms 的成员表里 96% 是它

成员表的基类遍历从一个**写法**里取"可以据此查找类的名字"：
`conditional_t<A, B>` 的真实基类确实是括号里的某个名字，所以 `lookup_names` 会取出里面的名字。
但 `_Vector_base<_Tp, _Alloc>` 也照此取出 `_Tp` 和 `_Alloc`：

```text
members_of("std::vector")（当前文件的 scopes，走索引）
  own                    1 ms      282 个成员
  base std::_Bvector_base 1 ms
  base _Alloc             13 ms → 0 个成员
  base std::_Vector_base  1 ms
  base _Tp               251 ms → 0 个成员     ← "谁声明了 _Tp"：整个闭包里上百条声明
  base _Tp 的 bases_of    88 ms
  level 1                355 ms
```

规则本身不是启发式：C++ 里模板参数会遮蔽同名，**一个类自己的模板参数不可能是它的基类**。
`members_of` 现在先取本类的模板参数（`class_template_parameters`，已有函数），基类遍历跳过它们。

```text
members_of("std::vector")   level 1  355 ms → 3 ms
access-invisible            375 ms → 15 ms
argument-count              365 ms → 10 ms
```

## 三、等价性：findings 一条没变

性能改动最容易悄悄改变答案，所以每次都用同一份输入对比**全部 finding**（`examples/diagnostics_audit.rs`，
输出 `path:line:col code message`，排序后 diff）：

```text
四个 LuaParser 文件
  改前  284 648 ms  18 findings
  改后    6 575 ms  18 findings
  diff                                          ← 空
```

测试：`cargo test --release --workspace`。唯一失败的
`an_unresolved_include_does_not_erase_the_files_own_facts` 在 HEAD 上也失败（用 `git stash` 验证过），
不是这次改动引入的；`handshake` / `latency` 两条在整套并发跑时会红、单独跑会绿，
是 `architecture-review.md` P1-1 / P3-8 记的墙钟与顺序依赖。

## 四、还没做的，和各自量到的大小

### 4.1 冷启动的"第二遍"是白做的 —— 1205 个文件、27 s，全部记在打字上

`index_everything()` 在索引队列空时返回，但**第二遍**（被包含的头入索引后重读文件）只由一个 slice 的尾部
搬运、一次 4 个文件。冷摘要缓存下一次实测：

```text
index_everything 返回时第二遍欠账      1 205 个文件
清完这笔账                            302 个 slice，27.3 s
每个 slice 的 trace                    "nothing bodied"  ← 27 s 里一个摘要都没重建
```

`nothing bodied` 的意思是：这个工程里没有任何宏体改变读法，所以第二遍的每一次单元走查都是白走。
而队列空之后，这笔账由**下一次按键**的 slice 还：4 个无关文件的重读 + 4 次单元走查，
实测每次按键从 10 ms 涨到 65 ms（最长 152 ms），持续约 300 次按键。

两个方向，都还没动：

```text
· 调度：slice 的重读量与被解析量成比例，欠账不累积（总量不变，但不再从打字里扣）
· 算法：把"哪条宏体可能改变读法"的判定提到单元走查之前（plain shape 先判，只有 Other 才需要环境），
  这个工程上直接省掉 27 s —— 需要重排 prepare_the_re_read 与 units_for_the_pass
```

### 4.2 大文件仍然是秒级：LuaSyntaxTree.cpp 6.2 s

```text
param-type-check       2 745 ms
assign-type-check      1 808 ms
access-invisible       1 055 ms
implicit-bool-conv       531 ms
```

三条规则都在做同一件事：对每个表达式问"这个类型的成员"。第一反应是模型的成员表 memo 按**写法**做键
（`std::vector<LuaSyntaxNode*>` / `std::vector<Token>` / `std::vector<size_t>` 各建一次表），
所以试过把它拆成"裸类名 memo + 每次替换实参"：**实测没有用** —— 同一个文件 6 248 ms → 6 161 ms，
在噪声里。也就是说这个文件的成本不是"同一个类被建了多次表"，而是**每个表达式各自的一次推导**
（`type_of_expression`：名字解析、`files_declaring`、`direct_member`、别名解析、scope 走查）。
那次改动已撤回，记录在这里，免得下一个人再拆一遍 memo。

下一步要对这个文件按表达式再量一次（`assign_type_check` 里的 `[expression]` 计时是一次尝试，
已撤回；需要的是一个常驻的 `CPPLS_TIME_EXPRESSIONS` 探针），先拿到"5 ms 花在哪个子查询"，
再决定是缓存哪一个。补全（15 ms）也会跟着受益。

### 4.3 每按键仍然重建一次 cook

`cook render` 4.8 ms = lex（6.5 KB）+ 宏展开 + 解析渲染 + 191 条 fact。这一项与按键文本长度成正比，
在 6.5 KB 的文件上是 5 ms；`docs/prefix-reuse.md` 记的前缀复用（共享 green tree）如果做出来，
能省的也是这一段的解析部分，但起点已经不是 17 ms 而是 5 ms。

## 五、怎么复现

```text
# 一次按键的逐阶段成本（会自动清掉第二遍欠账，先给你稳态数字）
cargo run --release -p cpp_code_analysis --example edit_loop -- \
    E:\EmmyLuaCodeStyle E:\EmmyLuaCodeStyle\LuaParser\src\File\LuaSource.cpp 8

# 按规则计时
$env:CPPLS_TIME_CHECKS="1"; …同上…

# 全项目 findings 基线 + 每文件耗时（--match 选一个子树，--time 打最慢的文件）
cargo run --release -p cpp_code_analysis --example diagnostics_audit -- \
    E:\EmmyLuaCodeStyle --match luaparser/src --files 4 --time
```

`Session::re_read_backlog()` 是这一轮新增的仪器：第二遍的欠账数。
`pending()`（索引队列）和 `pending_cooking()`（cook 队列）都不包含它，
所以一个"没有工作"的会话可以有 1205 个文件待重读——这正是 4.1 的入口。
