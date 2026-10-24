# 编辑时的钱花在哪里：一次诊断 ~150 ms 的拆分

**结论。**

```text
一次 keystroke 的诊断（LuaSource.cpp，20 KB）：140–155 ms
  其中 view（解析）          0.5–1 ms
       module notes          0 ms
       checks                ~130–150 ms
            ParamTypeCheck    57 ms
            AssignTypeCheck   49 ms
      型推断              170 ms / 153 个问题   ← 和 checks 的行有重叠：checks 的时间就是推断
用 CPPLS_NO_INFERENCE=1 关掉推断：诊断 6.6 ms
```

所以**编辑器里那 150 ms 基本全是类型推断**，其余检查加起来几个毫秒。剩下的问题是：为什么一个"这个表达式的类型"
要 ~1 ms。

## 已经排除的（都量过）

```text
索引查询（definition/definitions）     CPPLS_NO_INDEX_LOOKUP（临时开关）→ 诊断 138 ms，没变
跨文件解析（resolver）                 一次 pull 里 [resolver] 行数为 0 —— 从未被调用
成员表缓存未命中                       三次 keystroke：10 次命中 / 8 次未命中（ProjectIndex::member_lists）
项目闭包大小                           两文件工程 8 个问题 0.5 ms（0.06 ms/问）；EmmyLuaCodeStyle 1.1–1.8 ms/问
```

最后一行是关键：**同样的引擎、同样的代码，两文件工程是 0.06 ms/问，真实工程是 1 ms+/问**。所以不是"引擎每次都有固定开销"，
而是**和索引里有什么有关**——只是不在 `definition` 那条路上（关掉它数字不动）。

## 最慢的问题（新探针：`CPPLS_TRACE_TIME=1`，只打印 ≥0.5 ms 的）

```text
  8.3 ms  _lineOffsetVec.empty()
  8.3 ms  _source.data() + lineStartOffset
  8.3 ms  _source.size() - i
  5.5 ms  _source.data() + i
  3.4 ms  bytesLength                     ← 一个局部变量的类型
  2.8 ms  _lineOffsetVec[line]
  …
  20 个 ≥0.5 ms 的问题 = 84.9 ms；剩下 133 个问题占另外 ~85 ms（每个 ~0.6 ms）
```

`_lineOffsetVec`/`_source` 都是**文件自己类里的 std 容器成员**（`std::vector<size_t>`、`std::string`），
`bytesLength` 只是一个局部变量——一个局部变量的类型也要 3.4 ms，说明开销在**每个问题都要做的那件事**上，
而不是在某一个重问题上。

## 下一步（按顺序，各自都有可验证的判据）

```text
1. 在 type_of_expression_memoed 里按分支加计时：memo 命中 / 作用域查找 / 成员访问 / 类型构造 / 转换判断。
   现有探针只给总数；需要知道 1 ms 落在哪个分支。（这是纯测量，改一行代码的门槛。）
2. 若落在"成员访问"：members_of 的命中路径要把 MemberList 以 Arc 借出，而不是 clone 一份
   （307 个成员的列表每个问题 clone 一次 ≈ 100k 次分配/请求）。
3. 若落在"转换判断"：convertible_to 对 std 类型会去问构造函数，那是另一次成员查找；
   应按 (from,to) 在 TypeMemo 里记一份（目前没有）。
4. 若落在"作用域查找"：ScopeTree::scope_at 的二分是否退化成线性（一次请求 153 次 × O(scopes)）。
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
crates/cpp_code_analysis/src/sema/model.rs
                                    [question] 探针：≥0.5 ms 的推断问题连同表达式文本。
```

## 被量掉的候选（没有留在代码里）

```text
ordered_visibility：把可见性走查的答案预先转成 file id（每次查询 O(1) 而不是重新展开 617 个路径）。
  量到：推断 169 → 151 ms，checks 63/55 → 57/49 ms。end-to-end 诊断在噪声内。
  按本仓的规矩（数字不动就是没修），回滚；结论留下。
```

## 复现命令

```text
cargo run --release -p cpp_code_analysis --example edit_loop -- E:\EmmyLuaCodeStyle <file> 3
CPPLS_TIME_CHECKS=1     每条规则一行
CPPLS_TRACE_TIME=1      [checks] inference 总数 + [question] 慢问题 + [unit]/[resolver]
CPPLS_NO_INFERENCE=1    所有推断答 Unknown（A/B）
cargo test --release -p cpp_ls --test idle
```
