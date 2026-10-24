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
