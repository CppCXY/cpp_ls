# 一次「语义对了，性能掉了 2.6 倍」的解剖：索引把模板参数当成了文件作用域的声明

**报告**：给 operator 补上返回类型（`v[0]`、`-v`、`a + b` 因此有答案）之后，`LuaSyntaxTree.cpp` 的诊断从
**~210 ms 涨到 ~550 ms**。语义是对的，所以问题不在这个特性，而在**我们问问题的方式**。下面是把它钉死的过程。

## 一、先证明"不是问得更多"

`CPPLS_TRACE_TIME=1` 会打印整个 pull 的推理总量。两次运行（同一文件、同一次按键）：

```text
                       未打 operator 补丁        打了补丁
questions              452                      452
computed               247                      247
reused                 256                      256
总时长                 319 ms                   986 ms      ← 同样的 452 个问题，贵了 3 倍
```

**问题的数量一模一样**。所以不是"答案变多 → 检查问得更多"这种解释；是同样的问题里有某个变贵了。
这种方法上的判断（数量 vs 单价）应该在每次"语义修好但变慢"时先做一遍，它把搜索空间从"检查的算法"
缩到"某一个问题的成本"。

## 二、找出是哪一个

`[question]` 探针（`sema/model.rs`，≥0.5 ms 的问题按耗时打印）：

```text
补丁前：  93 个问题 ≥0.5 ms，合计 170 ms，最大 4.6 ms
补丁后：  99 个问题 ≥0.5 ms，合计 495 ms，其中一条 182 ms —— IsNode(child)
```

再把这个问题的邻居打出来（探针按发生顺序写到同一个 stderr，所以前后几行就是它的因果链）：

```text
[lookup] 167.1 ms  _Tp  (2288 answer(s))
[type_of] 176 ms  depth 1  "n.FirstChild"
[question] 177.6 ms  IsNode(child)
```

`n` 的声明类型是 `_Tp`——一个**模板参数**，引擎没法替换它，于是把它当类名去查成员
（`n.FirstChild` → `members_of("_Tp")` → `definitions("_Tp")`）。查这个名字要 **167 ms**。

## 三、为什么一个名字要 167 ms：索引里 2288 个 `_Tp`

新加的 `CPPLS_PRINT_LOOKUP` 探针（`examples/edit_loop.rs`）直接回答"这个查询到底拿到了什么"：

```text
lookup `_Tp` in 187.6ms
  2288 answer(s), by kind {"Type": 2288}
  top files: [("type_traits", 492), ("cmath", 152), ("stl_function.h", 111), ("shared_ptr.h", 71), …]
  e.g. "_Tp" kind Type scope None type None in …/include/c++/array
```

答案全是 `Type`、`scope: None`、没有任何类型——也就是每个头文件里 `template <class _Tp>` 的那个参数，
被当成**文件作用域的类型声明**存进了索引。根因在 `sema/declarations.rs::DeclarationFacts::build`：
它遍历**每个作用域**的每个 binding，而 `qualification_prefix_of` 对
`ScopeKind::TemplateParameters` 回答"没有前缀"，于是这些参数被记成了文件作用域的 `Type`。
索引的模型没有"只在某个声明内部可见"这种作用域，所以它们本来就不该进去。

## 四、修法：一个作用域不产生声明

```rust
if scope.kind == ScopeKind::TemplateParameters {
    continue;   // 模板参数不是文件作用域的声明；名字在 DeclFact::parameters 上
}
```

名字没有丢：类模板/函数模板的参数在**事实自己**的 `parameters` 字段上，替换步骤读的就是它
（`docs/status.md` 的替换链依赖的是这个字段，不是索引里的同名事实）。

## 五、数字

```text
                                 修前            修后
lookup `_Tp`                     187 ms          14 ms（2288 → 5 条答案，且都不是类）
索引事实数                        176 816         152 576      ← 少 24 240 条（13.7%）
cooked declarations              13 325          9 062        ← 少 4 263 条
LuaSyntaxTree.cpp 诊断            225 ms          188 ms
ImplicitBoolConversion           12 ms           12 ms
UndefinedField                   99 ms           99 ms
```

而**真正的问题——那个 2.6 倍——在补丁之上也消失了**：

```text
                          补丁 + 未修      补丁 + 本修复
ImplicitBoolConversion    199 ms          14 ms
UndefinedField            234 ms          80–110 ms
诊断                      551 ms          199 ms
推理总量                  986 ms          268 ms
```

也就是说 operator 的语义扩展本身**几乎没有成本**；之前量到的 2.6 倍是"模板参数被当成类名查询"的账单，
只是那一个问题的落点恰好换到了热路径上。修完之后诊断比打补丁前**更快**（199 vs 225），因为那些
`_Tp` 查询在修补丁前也在发生，只是在别的位置。

## 六、留下了什么

```text
tests/semantic.rs::a_template_parameter_is_not_a_declaration_at_file_scope
    修前失败（事实是 ["_Tp", "A", "value"]），修后通过；同时钉住"参数在 DeclFact::parameters 上"
CPPLS_PRINT_LOOKUP=<name>   examples/edit_loop.rs：一次名字查询的条数、种类、来源文件
[question] / [lookup] / [type_of]   已有的探针，这次是靠它们把因果链读出来的
```

## 七、这条经验的一般化

"语义修好之后变慢"几乎总是同一类错误：**某个查询能问出一个以前问不出的名字，而这个名字的查询在索引里没有
边界**。下次的顺序应该是固定的：

```text
1. 数量还是单价？   CPPLS_TRACE_TIME 的 questions/computed/reused —— 数量不变就不要再读检查的代码
2. 哪个问题？       [question] 的耗时列表 + 它前后的 [lookup]/[type_of]
3. 那个查询拿到了什么？ CPPLS_PRINT_LOOKUP —— 条数、种类、来源文件
4. 它该拿到这些吗？ —— 多数时候答案是"索引里存了本来不该存的东西"
```
