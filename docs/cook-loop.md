# 打开一个文件就烧一个核：永动的 cooking 循环

**报告**：语言服务器在打开文件后反复出现/消失诊断，长时间占满单核；日志里 `index slice` 每 ~250 ms 一条，
持续数分钟，而索引早已 drain。

**结论先行。**

```text
1. 不是 client 的 refresh 请求引起的（`workspace/diagnostic/refresh` 在这个服务里**从未被调用**）；
   单核来自会话内部一个**永不收敛的 cooking 队列**。
2. 实测出来的两个原因，都在同一条链上：
   a. `want_cooked_reading` 每次请求都**清掉"这个文件烤不动"的记录**，然后重新入队 —— 于是一次拉取
      诊断就把"拒绝"撤销一次，pump 再 claim、再拒绝，循环。
   b. 文件的翻译单元在索引尾巴期间被走成 **provisional**（缺文件），而这份 provisional 单元只在
      `translation_unit_of` 里、且队列为空时才会被丢弃；`cooking_materials` 是 `&self`，只读它，
      于是"烤不动"是永久状态。诊断永远只能拿到粗读数，客户端反复重拉。
3. 修复：拒绝只在**文本变化**（`buffer_changed`）时清除；队列排空且第二遍已付清时，丢掉过期的
   provisional 单元，让下一次尝试真正重新走一遍。回归测试钉住这两条。
```

## 日志

```text
12:09:53  analysing e:\EmmyLuaCodeStyle
12:09:58  the workspace is indexed
12:10:38  index drained: 8 rendering(s), 0 unit(s), 1273 summar(ies), 32 cooked file(s)
12:10:45  the pump made no progress in 8 slices with 0 file(s) left: 0 queued file(s) were dropped
12:10:53  …（每 8 秒一条，持续到日志结束）
  中间每 ~250 ms 一条：
  index slice: prepared 0, committed 0, write lock held 0 ms, 4 to cook, 2 left
  index slice: prepared 0, committed 0, write lock held 0 ms, 2 to cook, 0 left
```

`prepared 0, committed 0` + `to cook` 非零 = 队列里有文件、切片什么都没建成。`pending_work()` 因此永远
不为 0，pump 永远被唤醒。

## 复现（`examples/memory_report.rs`，`CPPLS_PULL_LOOP=1`）

```
cooking: E:\EmmyLuaCodeStyle\LuaParser\src\File\LuaSource.cpp yields no materials |
         summary indexed true | unit held provisional true
pull 0: pending 1 -> 1, advanced 0, cooking 1
pull 1: pending 1 -> 1, advanced 0, cooking 1
…
pull 39: pending 1 -> 1, advanced 0, cooking 1
```

这是**会话层**的复现：每次"拉取"（`want_cooked_reading`）把文件放回 cooking 队列，`materials_for` 因为
provisional 单元答 `None`，于是没有进展。

## 修的两处

```text
① `want_cooked_reading` 不再清除拒绝。
   拒绝的意思是"这个文件在当前状态下烤不动"，而一次请求不是"原因变了"的证据。
   清除点移到 `buffer_changed`（文本变了）—— 唯一的真实原因。
   → 一次拒绝之后，拉取不再重新入队；队列会在下一次 pump 切片后真正空掉。

② `cooking_materials`：provisional 单元 + 索引队列为空 + 第二遍已付清 ⇒ 丢掉这份单元。
   下一次尝试会重新走一遍（38 ms），拿到完整环境，烤成功，文件从此有 cooked reading，
   `is_ready_for_a_request_about` 为真，server 不再为它取写锁。
   第二遍没付清时不丢：那时的走查本身就是 provisional，丢了只会走两遍（实测尾巴 +2.5 s）。
```

回归测试：`a_cook_that_cannot_succeed_is_not_queued_again_by_a_request`（`session/tests.rs`）——
拒绝之后拉取不入队；`did_change` 之后重新入队。

## 没有改、但值得下一步做的

```text
· pump 每次 drain 会向客户端发最多三个 refresh（diagnostic / inlayHint / semanticTokens）。
  循环修掉之后 drain 变稀有，所以不再是风暴；但它仍是"水平触发"而不是"边沿触发"——
  应该在"答案可能变了"（索引代数变了）时才发，并按时间去抖。
· `advance` 不负责 cooking（pump 才负责）。所以任何 `while pending_work() > 0 { advance(…) }`
  的循环在"只剩 cooking 工作"时会空转。这不是会话的 bug，但它是探针/测试容易踩的坑：
  要么 `advance` 也在排空后烤一片（实测会把尾巴的量算进尾巴：+2.5 s），要么调用方自己去烤。
  本次按后者处理：`advance` 保持只读索引队列，pump 仍然是 cooking 的所有者。
```

## 验收

```text
复现：修复前 40 轮 pending 1 -> 1；修复后 pull 0 即 pending 0 -> 0
测试：cpp_code_analysis 677 passed；cpp_ls 87 + 32 passed（一个已知 flaky 并发用例，单独跑绿）
冷启动：第一遍 2.9–3.7 s，尾巴 63 切片 7.7 s（与修复前同量级；advance 烤片的版本曾把它推到 8.5 s，已回退）
诊断延迟：140–147 ms，未变
```

## 后续：一次编辑必须在下一个请求之前被读到（2026-10-24）

`tests/handshake.rs` 的 `a_completion_sees_a_change_to_an_included_header` 断言的是**同一口气**里
"改头文件 + 立刻补全"，而且答案是改后的成员。这个用例在并行全量里三次里败一次、单独跑必过——
检查下来是**流程**问题，不是测试问题：

```text
① `apply_buffer`（didOpen/didChange）只把文件入队然后 wake pump，summary 在 pump 下一片之前是旧的。
   下一个请求（补全）正好落在这个窗口里，读到的是旧头文件。
   → 现在 didOpen/didChange 在同一把写锁里 `advance(1)`：读的就是刚入队的那个文件（`Priority::Open`
     在队头；`advance(1)` 不会带出 wave，只解析这一个），实测 2–6 ms，在预算内。pump 仍负责它的 include。
② 修①之后暴露了第二个：pull 的快速路径只要求 `summary` 存在。文件"已读但还没 cook"时，
   一次 pull 直接返回 true，`diagnostics` 于是退回**粗读数**，一个未闭合的 class 得到空报告——
   `a_pulling_client_gets_a_report_and_not_null` 与 `an_error_typed_into_an_open_file_is_answered` 因此失败。
   → 快速路径改成要求 **cooked reading**（`cooked_declarations` 存在）：这正是"这个 pull 无事可做"的状态。
```

验收：handshake 全量并行连跑 4 次全绿；`cpp_ls` 87 + 32 + idle + latency 全绿；keystroke 延迟不变
（did_chg 2–5 ms，诊断 21–23 ms，findings 12/13/13）。
## 后续二：空报告不是答案（2026-10-24，实测）

`sticky report` 的第一版只在"没有读数"时重复上一次报告。加了 `tests/diagnostics_stable.rs` 才发现**还有第二种空答案**：

```text
一次编辑 → pull → diagnose_file 返回 Some(0)（真的"干净"报告，连续 7 次）→ 客户端擦掉波浪线
       → pump 结束 → refresh → 客户端再问 → 2 条出现
```

原因在检查层的可见性门：`checks_may_be_trusted` 在索引还有活的时候**扣住 check 结果**，于是报告里只有
parse error（这里没有），成了一个"干净"的答案。修法是把"扣住"当成第三种状态：

```text
settled 的报告          最终答案，替换客户端持有的
未 settled 的报告       与上一次报告合并（新出现的 parse error 立刻显示，未确认的 finding 留在屏幕上）
没有答案                重复上一次报告
```

规则是 `document_diagnostic::the_report`（三个分支有单测），`diagnose_file_with_state` 报告是否 settled。
`tests/diagnostics_stable.rs` 是端到端门禁：一个 `int count = "three"` 的文件，编辑后立刻 pull、
再连拉三次，findings 的数量**不允许从 >0 掉到 0**。修前 12 次跑 2 次失败，修后 12 次全过。