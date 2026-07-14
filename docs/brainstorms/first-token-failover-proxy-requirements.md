---
date: 2026-07-13
topic: first-token-failover-proxy
---

# 首 token 超时自动重试代理

## Problem Frame

本项目要做一层轻量 AI API 代理转发服务。核心问题不是完整的账号管理、
计费或管理后台，而是在某些上游 AI 服务负载过高时，请求进入服务方队列，
客户端长时间等不到首个 token。代理需要在用户设定的超时窗口内自动放弃
慢请求，并重新发起一次或多次上游请求；只要尚未向客户端输出任何语义响应，
这个过程对客户端应尽量无感。

参考分析了 `sub2api`。它是完整 AI 网关平台，覆盖账号、分组、调度、
计费、限流、管理后台与多协议兼容；其中已有账号 failover 与流数据间隔
超时机制，但我们的需求应收敛为更小、更专注的“转发 + 首 token 超时重试”
能力。

外部参考仓库 `sub2api` 中的关键观察：

- `backend/internal/server/routes/gateway.go` 暴露 `/v1/messages`、
  `/v1/responses`、`/v1/chat/completions` 等 AI 网关入口。
- `backend/internal/handler/openai_gateway_handler.go` 已有 OpenAI 请求
  账号选择与 failover 循环，只有在未写出客户端语义响应时才允许切换。
- `backend/internal/service/openai_gateway_response_handling.go` 已有
  `stream_data_interval_timeout`，但它监控的是“上游任意数据间隔”，不是
  “首个语义 token 等待时间”。
- 同文件已有 `firstTokenMs` 与 `startsClientOutput` 判断，说明“首 token”
  应按语义输出事件识别，而不是按上游任何 SSE 帧识别。
- `backend/internal/repository/http_upstream.go` 对 OpenAI profile 默认关闭
  response header timeout，说明上游排队可能发生在响应头前，也可能发生在
  SSE 已建立但迟迟无语义 token 的阶段。

## Request Flow

```text
客户端请求
  │
  ▼
代理读取并缓存请求体
  │
  ▼
读取已配置的单个上游 Base URL / endpoint
  │
  ▼
发起第 N 次上游请求
  │
  ├─ 等响应头超时 ───────► 取消本次请求，重试下一次
  │
  ├─ 已有响应头但首 token 超时 ─► 若未向客户端写语义输出，取消并重试
  │
  ├─ 首 token 正常到达 ───► 开始向客户端转发；后续不再无感切换
  │
  └─ 上游明确错误 ───────► 按可重试策略决定重试或返回错误
```

## Requirements

**代理入口与协议**

- R1. 代理服务应优先支持 OpenAI-compatible HTTP/SSE 入口，包括
  `/v1/chat/completions` 与 `/v1/responses`。
- R2. 代理应透传请求方法、路径、query、请求体与必要请求头，并允许配置
  单个上游 base URL 和超时策略；客户端请求中的 Authorization/API Key 原样转发，
  不在代理入口做 API Key 校验。
- R3. 代理应支持流式响应；非流式响应可以复用响应头超时和总请求超时，
  但首 token 超时主要面向 SSE 流。

**超时与重试**

- R4. 应提供“等待响应头超时”配置，用于处理上游在 HTTP response header
  前排队过久的情况。
- R5. 应提供“首 token 超时”配置，用于处理 HTTP/SSE 已建立但迟迟没有
  首个语义输出 token 的情况。
- R6. 首 token 应定义为“客户端可感知的语义输出”，而不是任何上游 SSE 帧。
  例如 OpenAI Responses 的 created/in_progress、ping、keepalive、usage-only
  事件不应算首 token。
- R7. 在响应头超时或首 token 超时时，代理必须取消当前上游请求、关闭响应体、
  释放本次 attempt 占用资源，然后重新发起新 attempt。
- R8. 自动重试只允许发生在代理尚未向客户端写出语义响应之前；一旦开始向
  客户端输出 token，就不能把两个不同上游请求的流拼接到同一个下游响应里。
- R9. 应提供最大 attempt 次数、最大重试次数或最大切换次数配置。全部耗尽后，
  代理返回 OpenAI-compatible 错误，推荐 HTTP 504。
- R10. MVP 先做全局策略配置和单个上游 Base URL；模型级、endpoint 级覆写可作为
  后续增强。

**客户端无感行为**

- R11. 若重试成功，客户端只应看到最终成功 attempt 的响应流，不应看到前序
  attempt 的任何 header、SSE 帧、错误事件或 keepalive。
- R12. 代理需要延迟提交下游响应头，直到确定要输出首个语义事件，或已经决定
  返回最终错误。
- R13. 对流式响应，代理可在内部缓存首 token 前的非语义 SSE 事件，但在发生
  failover 时必须丢弃；不能把旧 attempt 的状态事件透传给客户端。

**计费与副作用**

- R14. 代理自身不做用户计费；但必须记录每个 attempt 的结果，至少包含
  upstream、model、timeout_reason、duration、是否已向客户端输出。
- R15. 对被取消的上游请求，代理只能尽力 cancel；不承诺上游一定不扣费。
  需求上只保证客户端侧无感和本代理不重复记录成功用量。
- R16. 如果未来接入本地 usage 统计，只有最终成功且实际输出的 attempt 应计入
  成功请求；被放弃 attempt 作为 abandoned/timeout 事件记录。

**稳定性与资源控制**

- R17. 策略层请求体应在代理层读取并缓存一次，供多次 attempt 重放；不再提供
  请求体大小限制策略。
- R18. 客户端断开连接时，代理应取消当前上游请求，并停止后续重试。
- R19. 每次 attempt 应有独立 context、独立上游连接和明确清理逻辑，避免连接、
  goroutine 或响应体泄漏。
- R20. 重试应默认串行执行。并发抢跑/hedged request 可以作为后续增强，但不进入
  MVP，避免放大上游成本和并发压力。

**可观测性**

- R21. 日志应记录 request_id、attempt_id、upstream、model、endpoint、
  response_header_ms、first_token_ms、timeout_reason、retry_count 和最终结果。
- R22. 指标应至少包含请求总数、成功数、超时重试数、首 token 超时数、响应头
  超时数、重试后成功数、全部重试耗尽数和 TTFT 分布。
- R23. 应能通过配置快速关闭自动重试，回退为普通代理转发。

## Success Criteria

- 人工构造“响应头前延迟超过阈值”的上游时，代理会取消该 attempt 并重试。
- 人工构造“已返回 SSE response.created 但迟迟无输出 token”的上游时，代理会
  在首 token 超时后取消该 attempt，并把最终成功 attempt 的流返回给客户端。
- 重试成功时，客户端响应中没有前序 attempt 的残留 SSE 帧。
- 所有 attempt 都超时时，客户端收到清晰的 OpenAI-compatible 504 错误。
- 客户端断开时，上游请求被取消，不再继续重试。
- 日志和指标能够区分 response header timeout 与 first token timeout。

## Scope Boundaries

- 不复制 `sub2api` 的完整账号管理、支付、计费、后台、复杂调度和多平台生态。
- 不在已经输出首 token 后做无感 failover。
- 不承诺阻止上游对已取消请求计费，只做本代理侧资源清理和结果记录。
- 不在 MVP 中实现并发抢跑、预测性调度或自动质量评分。
- 不优先支持图片、音频、视频等非文本流式生成场景。

## Alternatives Considered

**方案 A：轻量独立代理，串行 failover（推荐）**

只实现转发、超时、取消、重试、观测。复杂度低，最贴合当前项目目标。
缺点是如果所有上游都慢，最终仍会等待多次超时后失败。

**方案 B：直接 fork/裁剪 sub2api**

可以复用账号池、调度、监控等能力，但引入大量非目标复杂度。对本项目来说，
维护成本和理解成本偏高，不建议作为第一阶段。

**方案 C：并发抢跑/hedged request**

同时向多个上游发请求，谁先出首 token 就用谁，其他取消。TTFT 最优，但会放大
上游成本、限流压力和潜在扣费，适合后续在高价值流量上按需启用。

## Key Decisions

- 第一阶段做轻量代理，不做完整 AI 服务管理平台。
- 首 token 超时必须独立于“流数据间隔超时”，判断对象是语义输出，不是任意 SSE 帧。
- 重试必须在下游响应提交前完成；首个语义输出一旦写给客户端，本次请求就固定在
  当前上游 attempt 上。
- MVP 采用串行重试，避免并发抢跑带来的成本和副作用。
- 配置先覆盖全局策略和单个上游 Base URL；模型级、endpoint 级覆写作为自然扩展点保留。
- 代理拆成两层运行路径：
  - 策略旁路的直接转发层：全局策略关闭时使用，只选择已配置的单个上游并直接流式转发，
    不做首 token 判断、自动重试或请求体缓存重放；仅过滤协议级 hop-by-hop header。
    若请求记录开关开启，仍异步记录 request/attempt metadata、完整请求/响应正文、
    响应头耗时、首 token 耗时和完整响应耗时。
  - 策略层：全局策略开启时使用，执行响应头超时、首 token 超时、串行重试和后续过滤逻辑。
- 所有运行配置保存到 SQLite，服务启动时加载到内存缓存；管理页面修改配置或保存
  上游 Base URL 后刷新内存缓存。
- 请求记录以 SQLite 为主存储，stdout/stderr 只保留普通运行日志；新增记录开关控制
  是否写入 request/attempt records 和 request/response payloads。payload 按 chunk
  追加落库并记录完整性标记，不做截断；写入通过 bounded channel + 后台 writer
  异步完成，队列满或写入失败不阻塞代理转发。
- 上游配置收敛为单个全局 Base URL，不再提供代理侧密钥、上游侧密钥或逐项启停。

## Dependencies / Assumptions

- 初始目标客户端使用 OpenAI-compatible API。
- 初始目标上游支持 HTTP/SSE；WebSocket 可以后续单独设计。
- 用户能提供至少一个可重试的上游配置；如果只有单个上游，也允许同上游重新发起
  新 attempt。
- 请求体主要为 JSON，可被代理完整缓存后重放。

## Outstanding Questions

### Resolve Before Planning

- [Affects R1][User decision] 第一阶段只做 OpenAI `/v1/chat/completions` 和
  `/v1/responses`，还是还要兼容 Anthropic `/v1/messages`？

### Deferred to Planning

- [Affects R6][Technical] 每种协议的“首个语义 token”事件集合需要在代码层精确定义。
- [Affects R10][Technical] 配置格式采用 YAML、环境变量，还是二者都支持。
- [Affects R21][Technical] 指标出口用 Prometheus endpoint、JSON stats endpoint，
  还是先只做结构化日志。

## Next Steps

先确认第一阶段协议范围，然后进入 `ce:plan` 做结构化技术方案与文件级实施计划。
