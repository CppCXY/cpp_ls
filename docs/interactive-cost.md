# 编辑时的钱花在哪里：一次诊断 ~150 ms → ~20 ms

**结论（先看这个）。**

```text
                                修复前              修复后
LuaSource.cpp（20 KB）           140–155 ms          21–23 ms      （findings 12/13/13）
LuaSyntaxTree.cpp（40 KB）       639–758 ms          177–214 ms    （findings 17/19/19，逐条相同）
型推断（一次 pull，153 个问题）   174 ms              13 ms
慢的声明查询（≥0.2 ms）          99 次               3 次
```

**根因不是引擎，是索引查询的重复**：一次 pull 里 99 次慢查询有 **95 次是同一个名字 `std`**，每次 0.6–1.3 ms；
`std` 是文件里每个限定名的前缀，而每次查询都要重新收一遍候选（MSVC 在约一百个头文件里写 `namespace std`）、
对候选做**平方级**的"同一文件内同一声明"去重，再把声明成组 clone 出来。

两处修复：

```text
① ProjectIndex::definitions 按 (拼写, 查询所在文件) 记忆答案（definition_answers）。
   索引内容变了就整表清掉（insert_at / insert_cooked / forget / forget_cooked）——
   和 visibility 记忆同一条规则，答案只可能在"索引里的东西变了"时变。
   一次 pull 的 95 次 std 查询变成 1 次。实测：型推断 174 → 13 ms。

② 命名空间的答案先折叠再判重。命名空间不管有几个声明、在几个文件里，答案永远是"一个"，
   所以先 truncate(1)，跳过平方级的去重：一个 std 查询 1.3 → 0.6 ms，且答案不变
   （去重保序，`certain[0]` 前后相同）。
```

两边一起：一次 pull 的型推断 174 → 13 ms，真实文件的诊断 150 → 21 ms（6×）/ 758 → 214 ms（3.5×）。

## 怎么找到的（这条链要留着）

```text
1. CPPLS_NO_INFERENCE=1：诊断 150 → 6.6 ms。所以 95% 在型推断，其余检查总和几毫秒。
2. CPPLS_NO_INDEX_LOOKUP=1（新开关）：诊断 174 → 4 ms 的推断时间。
   ← 这一步第一次做时 PowerShell 的 .Replace 静默失败，得到"没变"的错误结论；
     用 edit 工具重做才看到 174 → 4。**结论和工具都要复验。**
3. [lookup] 探针（≥0.2 ms 的声明查询，打印名字与答案数）：99 次，其中 95 次是 `std`。
4. [question] 探针（≥0.5 ms 的推断问题，打印表达式）：
   `_lineOffsetVec.empty()` 8.3 ms、`_source.data() + lineStartOffset` 8.3 ms、`bytesLength` 3.4 ms。
   一个局部变量也要 3.4 ms —— 说明开销不在"某个复杂表达式"上，而在"每个问题都要做的那个查询"上。
```

## 已经排除的（都量过，别再走一遍）

```text
跨文件解析（resolver）        一次 pull 里 [resolver] 行数为 0 —— 从未被调用
成员表缓存                   三次 keystroke：10 次命中 / 8 次未命中（ProjectIndex::member_lists）
项目闭包大小                 两文件工程 0.06 ms/问；真实工程 1 ms+/问 —— 所以是"索引里有什么"，
                             不是"闭包多大"（同名的 std 在后者的索引里有几百条候选）
view（解析）                 0.5–1 ms
module notes                 0 ms
```

## 被量掉、没有留在代码里的

```text
ordered_visibility：把可见性走查的答案预先转成 file id，每次声明查询 O(1) 而不是重新展开 617 个路径。
  量到：型推断 169 → 151 ms，checks 63/55 → 57/49 ms。end-to-end 在噪声内。
  按本仓的规矩（数字不动就是没修）回滚；结论留下 —— 真正的成本是"同名查询的重复"，不是"路径展开"。
```

## 本轮的落地

```text
crates/cpp_ls/tests/idle.rs         真实服务器 + 假客户端的端到端门禁：
                                    空闲 5 秒内不向客户端发任何 refresh/publishDiagnostics；
                                    一次编辑 ≤4 条 refresh。（跑 7.5 s）
crates/cpp_ls/src/server/mod.rs      initialize 参数解析失败不再 panic：回 -32602 InvalidParams。
                                    用一个真实客户端形状（semanticTokens 缺 requests）复现的崩溃。
crates/cpp_ls/src/context/analysis_state.rs
                                    pull 的预检不再额外解析一次文件（原来要 view 才认为"ready"）。
crates/cpp_code_analysis/src/index/project.rs
                                    definition_answers 记忆 + 命名空间先折叠；[lookup] 探针。
crates/cpp_code_analysis/src/sema/model.rs
                                    [question] 探针：≥0.5 ms 的推断问题连同表达式文本。
crates/cpp_code_analysis/src/probes.rs
                                    CPPLS_NO_INDEX_LOOKUP（A/B 用的开关，和 CPPLS_NO_INFERENCE 同族）。
```

## 下一步（都有可验证的判据）

```text
1. `undefined-field` 仍是 LuaSyntaxTree.cpp 里最贵的一条（92 ms）——它问的是成员存在性，
   每个成员访问一次 members_of；看它是否也该走"按 (class, member) 记忆"的路径。
2. 剩下 3 次慢查询里 `size_t` 有 132 个答案：一个 typedef 在标准库的每个头里都声明一次。
   命名空间的折叠规则能不能推广到 typedef/别名（同名的多个声明如果"指向同一个实体"……
   —— 这条要小心，不能猜）。
3. 冷启动尾巴仍然 63 切片 / 7.7 s，和编辑延迟是两笔账；见 docs/memory-audit.md 与 docs/cook-loop.md。
```

## 复现命令

```text
cargo run --release -p cpp_code_analysis --example edit_loop -- E:\EmmyLuaCodeStyle <file> 3
CPPLS_TIME_CHECKS=1     每条规则一行
CPPLS_TRACE_TIME=1      [checks] inference 总数 + [question] 慢问题 + [lookup] 慢查询 + [unit]/[resolver]
CPPLS_NO_INFERENCE=1    所有推断答 Unknown（A/B）
CPPLS_NO_INDEX_LOOKUP=1 所有声明查询答 NotDeclaredHere（A/B）
cargo test --release -p cpp_ls --test idle
```
