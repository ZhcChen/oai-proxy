---
title: feat: Rust HTMX 首 token 重试代理
type: feat
status: completed
date: 2026-07-13
origin: docs/brainstorms/first-token-failover-proxy-requirements.md
---

# feat: Rust HTMX 首 token 重试代理

## Overview

本计划定义 `oai-proxy` 的第一阶段技术架构：使用 Rust 构建一个固定端口
`57999` 的轻量 AI HTTP 代理。该服务在同一个端口上同时提供：

- 管理页面：配置单个上游 Base URL、超时、重试策略、请求记录。
- 转发 API：OpenAI-compatible HTTP/SSE 入口。
- 运维端点：健康检查、基础指标或状态查询。

第一阶段只完善 HTTP 模式，不实现 WebSocket 转发。WS 作为后续扩展点预留，
避免当前设计被 WS 状态恢复、连接复用和双向流协议复杂度拖重。

## Problem Frame

需求来自 `docs/brainstorms/first-token-failover-proxy-requirements.md`：部分 AI
上游在高负载时排队，导致响应头或首个语义 token 长时间不可达。代理需要在
用户设定的时间内取消当前上游 attempt，并重新发起请求；只要未向客户端输出
语义响应，重试过程应对客户端无感。

用户补充的架构约束：

- 使用 Rust。
- 使用一个主流 Web 框架。
- 管理页面使用 HTML + htmx。
- 端口固定 `57999`。
- Web 配置页面、请求记录、转发 API 都走同一个服务端口。
- 先支持 HTTP 协议；WS mode 暂不纳入第一阶段。

## Requirements Trace

- R1. 支持 OpenAI-compatible HTTP/SSE 入口：`/v1/chat/completions` 与
  `/v1/responses`。
- R2. 透传请求方法、路径、query、请求体和必要请求头，并支持配置上游。
- R3. 支持流式响应；首 token 超时主要面向 SSE。
- R4. 支持等待响应头超时。
- R5. 支持首 token 超时。
- R6. 首 token 按语义输出识别，不把 created/in_progress/ping/usage-only
  等非语义帧算作首 token。
- R7. 超时后取消当前 attempt、释放资源、发起下一次 attempt。
- R8. 未写出语义响应前允许重试；一旦输出首 token，不再无感切换。
- R9. 支持最大 attempt 配置，耗尽后返回 OpenAI-compatible 504。
- R10. MVP 先支持全局策略 + 单个上游 Base URL 配置，保留模型级覆盖扩展点。
- R11-R13. 重试成功时客户端只看到最终成功 attempt 的响应。
- R14-R16. 记录 request 和 attempt，不做用户计费，不承诺上游取消后不扣费。
- R17-R20. 策略层请求体可重放、客户端断开即取消、attempt 独立清理，默认串行重试。
- R21-R23. 提供结构化日志、请求记录、基础指标，并可关闭自动重试。

## Scope Boundaries

- 不复制 `sub2api` 的账号管理、支付、计费、复杂调度和管理后台体系。
- 第一阶段不做 WebSocket 转发，不做 WS 首 token 超时。
- 第一阶段不做并发抢跑 / hedged request。
- 第一阶段不做多用户后台系统；后台页面不内置登录，部署侧按需保护 `/admin/*`。
- 第一阶段默认不保存完整请求体和响应体，避免隐私与存储膨胀。
- 第一阶段不优先支持图片、音频、视频生成流。

## Context & Research

### Local Repo

- 当前仓库只有项目规范和需求文档，还没有 Rust 项目结构。
- `docs/solutions/` 仅有 `.gitkeep`，没有可复用的历史经验文档。
- 因为空仓库无既有实现模式，本计划以需求文档和外部框架文档为主要依据。

### sub2api Reference

从外部参考仓库分析得到的可借鉴点：

- `sub2api` 具备完整 gateway route 和 OpenAI failover 循环，但体系明显重于本项目。
- 它的关键经验是：只有在未向客户端写出语义响应前，failover 才能无感。
- 它已有 `stream_data_interval_timeout`，但该机制监控任意上游数据间隔，不等价于
  首个语义 token 超时。
- 它对 OpenAI 的响应头等待做了特殊处理，说明排队可能发生在 response header 前。

### External References

- Context7 `Axum` 文档显示 Axum 支持 `Router::with_state` 共享状态、SSE 响应，
  以及将 `reqwest` 的 `bytes_stream()` 包装成 Axum `Body::from_stream()`。
- Context7 `Actix Web 4.11` 文档显示 Actix Web 也支持 streaming response 和共享
  app state。
- Context7 `htmx 2.0.4` 文档显示 htmx 适合通过 `hx-boost` 增强表单，通过
  `hx-get` / `hx-trigger` / `hx-swap` 做局部刷新和表格轮询。
- Context7 `HeroUI` 文档显示 React 组件库需要 React 19+ 与 Tailwind CSS v4；
  `@heroui/styles` 提供 framework-agnostic CSS 构建产物，适合本项目本地引用。

## Key Technical Decisions

- **Web 框架选 Axum 0.8 系列。**
  - 理由：Axum 基于 Tokio / Tower / Hyper，适合 HTTP 服务、middleware、
    shared state 和流式 body 组合；对代理类服务更自然。
  - Actix Web 性能和功能都强，也支持 streaming，但 Axum 与 Tower 生态、
    `reqwest` streaming body、middleware 分层更贴合本项目。
  - Rocket 不作为首选，因为本项目重点是代理、SSE、超时和流式控制，不是
    batteries-included 表单应用。

- **运行时使用 Tokio。**
  - Axum、reqwest、SQLx、异步流处理都能自然运行在 Tokio 上。

- **HTTP 客户端使用 reqwest。**
  - 主要能力：上游请求、response header timeout 包装、流式 `bytes_stream()`、
    TLS/rustls、连接池。
  - 首 token 超时不依赖 reqwest 总超时，而在代理流处理层用 attempt context
    和 `tokio::time::timeout` 控制。

- **管理页面使用 Askama 模板 + htmx。**
  - Askama 让 HTML 模板保留为真实 `.html` 文件，适合用户要求的 HTML + htmx。
  - htmx 负责配置表单提交、请求记录列表局部刷新、详情面板加载，不引入 SPA。
  - HeroUI 官方 React 组件库不进入运行时；本项目仅通过 `@heroui/styles`
    本地 vendoring 样式资产，配合手写 HTML class 使用。
  - htmx 与 HeroUI 样式都不走 CDN，统一通过 npm 下载后复制到 `static/vendor/`
    并由 Axum 静态文件路由提供。

- **持久化使用 SQLite + SQLx。**
  - 理由：项目目标是单进程轻量代理，SQLite 足够承载配置和请求记录。
  - 开启 WAL，配置请求记录 retention，避免长期无限增长。
  - 后续如果需要多实例部署，再抽象 repository 层迁移 PostgreSQL。

- **配置以 SQLite 为运行时真源，环境变量只做启动引导。**
  - 固定端口 `57999` 不做端口配置。
  - `OAI_PROXY_BIND` 可选控制绑定地址，默认 `127.0.0.1`。
  - 后台页面不做登录授权；公网部署建议用反向代理限制 `/admin/*`。
  - 客户端 Base URL 根据访问域名自动展示，不作为配置项保存。
  - 代理入口不做 API Key 校验；上游只配置一个全局 Base URL，客户端 Authorization/API Key 原样转发。

- **API 和后台共端口，路径隔离。**
  - `/admin/*`：管理页面和 htmx partial。
  - `/v1/chat/completions`、`/v1/responses`：代理入口。
  - `/healthz`、`/metrics`：运维入口。

- **首 token 重试在代理层做状态机，不做通用透明反向代理。**
  - 代理必须理解 SSE 事件边界，才能判断哪些帧是语义输出。
  - 成功 attempt 首 token 到达前，代理可以缓存该 attempt 的前置非语义帧；
    一旦确认成功，再按原顺序 flush 给客户端。
  - 失败 attempt 的所有前置帧必须丢弃。

## Open Questions

### Resolved During Planning

- 第一阶段协议范围：按用户最新约束，先做 HTTP 模式。OpenAI-compatible
  `/v1/chat/completions` 和 `/v1/responses` 进入 MVP；Anthropic `/v1/messages`
  和 WS 转发暂缓。
- 管理 UI 技术：使用服务端 HTML + htmx，不引入 React/Vue。
- 端口：固定 `57999`，不做运行时端口配置。

### Deferred to Implementation

- OpenAI Responses 与 Chat Completions 的首个语义 token 事件集合需要在测试中
  以 fixture 精确定义。
- SQLite request retention 的默认数量和天数可以先设合理默认，后续根据实际使用调整。
- 是否暴露 Prometheus 格式 `/metrics` 还是先做 JSON stats，可在实现时按依赖成本定夺；
  管理页请求记录是第一优先级。

## High-Level Technical Design

> This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.

```text
┌──────────────────────────────┐
│ Axum server :57999           │
├──────────────────────────────┤
│ /admin/*                     │
│  HTML + htmx + Askama         │
│  config / upstreams / logs    │
├──────────────────────────────┤
│ /v1/chat/completions          │
│ /v1/responses                 │
│  proxy ingress                │
├──────────────────────────────┤
│ Proxy engine                  │
│  request body replay          │
│  upstream selection           │
│  attempt loop                 │
│  header timeout               │
│  first-token timeout          │
│  SSE classifier               │
├──────────────────────────────┤
│ SQLite                        │
│  settings / upstreams         │
│  request records              │
│  attempt records              │
└──────────────────────────────┘
```

Attempt 状态机：

```text
read request body
  │
  ▼
attempt loop
  │
  ├─ send upstream request
  │    ├─ response header timeout -> record timeout -> next attempt
  │    └─ got response
  │
  ├─ if non-stream -> proxy response / error
  │
  └─ if stream
       ├─ parse SSE until first semantic output
       ├─ first-token timeout -> drop buffered prefix -> next attempt
       ├─ first semantic output -> commit downstream response
       └─ stream rest without invisible failover
```

## Implementation Units

- [x] **Unit 1: Rust Axum 项目骨架**

**Goal:** 建立 Rust 服务基础结构，固定监听 `57999`，包含基础路由、日志和配置加载。

**Requirements:** R1, R2, R23

**Dependencies:** None

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/app.rs`
- Create: `src/config.rs`
- Create: `src/error.rs`
- Test: `tests/health_routes.rs`

**Approach:**
- 使用 Rust edition 2024。
- 使用 Axum + Tokio 搭建 HTTP server。
- 端口固定为 `57999`；只允许配置 bind host。
- 引入 `tracing`，所有请求具备 request id。
- 建立 `AppState`，集中持有配置、数据库池、HTTP client、代理服务。

**Patterns to follow:**
- Axum 文档的 `Router::with_state` shared state 模式。
- Tower middleware 分层思路。

**Test scenarios:**
- Happy path: 启动测试 router，GET `/healthz` 返回 200。
- Edge case: 未匹配路径返回明确 404。
- Error path: 配置文件缺失时使用默认配置和环境变量启动。

**Verification:**
- 服务可在 `57999` 暴露基础路由。
- 所有 handler 能访问共享 state。

- [x] **Unit 2: SQLite 数据模型与配置存储**

**Goal:** 持久化单上游 Base URL、全局超时设置、请求记录和 attempt 记录。

**Requirements:** R2, R10, R14, R17, R21

**Dependencies:** Unit 1

**Files:**
- Create: `migrations/0001_initial.sql`
- Create: `src/storage/mod.rs`
- Create: `src/storage/settings.rs`
- Create: `src/storage/upstreams.rs`
- Create: `src/storage/records.rs`
- Test: `tests/storage_settings.rs`
- Test: `tests/storage_records.rs`

**Approach:**
- 使用 SQLx + SQLite。
- 表建议包含：
  - `settings`
  - `upstreams`
  - `request_records`
  - `attempt_records`
- 代理入口不存储 API Key；客户端 Authorization/API Key 只作为请求 header 透传。
- 请求记录默认只保存 metadata，不保存完整 prompt/body。

**Patterns to follow:**
- repository 层隔离 SQL，handler 不直接写 SQL。

**Test scenarios:**
- Happy path: 创建 upstream 后可读取并用于配置页面。
- Happy path: 写入 request record 和多个 attempt records 后可按时间倒序查询。
- Edge case: 空配置库启动时插入默认 settings。
- Happy path: 重复保存上游 Base URL 会覆盖同一条全局上游配置。
- Error path: 无效 Base URL 返回可展示错误。

**Verification:**
- 配置和请求记录重启后不丢失。
- 数据库文件可独立放在 `data/oai-proxy.sqlite3`。

- [x] **Unit 3: 管理页面 HTML + htmx**

**Goal:** 提供可用的后台页面，用于配置参数、管理上游、查看请求记录。

**Requirements:** R10, R14, R21, R23

**Dependencies:** Unit 1, Unit 2

**Files:**
- Create: `src/admin/mod.rs`
- Create: `src/admin/handlers.rs`
- Create: `package.json`
- Create: `package-lock.json`
- Create: `static/vendor/htmx.min.js`
- Create: `static/vendor/heroui.min.css`
- Create: `static/app.css`
- Create: `templates/layout.html`
- Create: `templates/dashboard.html`
- Create: `templates/settings.html`
- Create: `templates/upstreams.html`
- Create: `templates/requests.html`
- Create: `templates/partials/*.html`
- Test: `tests/admin_routes.rs`

**Approach:**
- 使用 Askama 渲染 HTML。
- htmx 用于配置表单提交、局部刷新请求记录、加载详情。
- 使用 npm 本地下载 `htmx.org` 与 `@heroui/styles`，把发布产物复制到
  `static/vendor/`；模板只引用本地路径。
- HeroUI React 组件不进入运行时，避免破坏 HTML + htmx 的服务端渲染架构。
- 管理页面不提供登录页，不使用后台授权 token。
- 配置页展示按当前域名推导的客户端 Base URL；上游页只配置单个 Base URL。
- `/admin/requests` 支持按状态、模型、上游、时间窗口过滤。

**Patterns to follow:**
- htmx 文档中的 `hx-boost` 表单增强和 `hx-trigger="every ..."` 轮询模式。
- 后台 partial endpoint 与整页 endpoint 分离，便于 htmx swap。
- HeroUI styles 包的本地 CSS 产物，不使用 CDN。

**Test scenarios:**
- Happy path: 无登录访问 `/admin` 返回 dashboard HTML。
- Happy path: 配置页按当前 Host 展示客户端 Base URL。
- Happy path: 上游页保存单个 Base URL 后刷新运行时缓存。
- Happy path: 修改 first token timeout 后，页面 partial 显示更新后的值。
- Happy path: 请求记录列表 htmx endpoint 返回表格 fragment。
- Error path: 无效配置值返回表单级错误，不写入数据库。

**Verification:**
- 不写自定义前端框架也能完成配置和查看记录。
- 页面不生成、不保存代理侧密钥；客户端鉴权头由请求原样透传到上游。

- [x] **Unit 4: HTTP 代理入口与上游转发**

**Goal:** 支持 OpenAI-compatible HTTP endpoint 的基础转发，不包含首 token 重试细节。

**Requirements:** R1, R2, R3, R17, R18, R19

**Dependencies:** Unit 1, Unit 2

**Files:**
- Create: `src/proxy/mod.rs`
- Create: `src/proxy/routes.rs`
- Create: `src/proxy/upstream.rs`
- Create: `src/proxy/headers.rs`
- Create: `src/proxy/body.rs`
- Test: `tests/proxy_forwarding.rs`

**Approach:**
- 暴露 `/v1/chat/completions` 和 `/v1/responses`。
- 策略层请求体读取为 bytes，供 attempt 重放；请求体大小不再作为策略项限制。
- 使用 reqwest 构造上游请求，透传请求头、query 和 body，仅过滤协议级 hop-by-hop header。
- 响应头按 allowlist 透传，避免 hop-by-hop headers 泄漏。
- 客户端断开时取消当前 attempt。

**Patterns to follow:**
- Axum 文档中 `Body::from_stream(reqwest_response.bytes_stream())` 的流式 body 思路。
- sub2api 的经验：响应写出后不能再无感 failover。

**Test scenarios:**
- Happy path: POST `/v1/chat/completions` 被转发到配置的 mock upstream。
- Happy path: query string 和必要 headers 被转发。
- Edge case: 无上游配置时返回 OpenAI-compatible 503。
- Error path: 无可用 upstream 返回 OpenAI-compatible 503。
- Error path: 上游连接失败返回可记录的 502/504，并写 attempt record。
- Integration: 一次代理请求产生一个 request record 和至少一个 attempt record。

**Verification:**
- 非流式请求可完整代理。
- 流式请求在无重试逻辑时可直接转发。

- [x] **Unit 5: 首 token 超时与串行 attempt loop**

**Goal:** 实现响应头超时、首 token 超时、取消当前 attempt、重新发起下一次 attempt。

**Requirements:** R4, R5, R6, R7, R8, R9, R11, R12, R13, R18, R19, R20

**Dependencies:** Unit 4

**Files:**
- Create: `src/proxy/attempt.rs`
- Create: `src/proxy/sse.rs`
- Create: `src/proxy/semantic_token.rs`
- Modify: `src/proxy/routes.rs`
- Modify: `src/proxy/upstream.rs`
- Test: `tests/first_token_failover.rs`
- Test: `tests/sse_semantic_token.rs`

**Approach:**
- `request.send()` 外层使用 response header timeout。
- 对 SSE 响应，读取并解析 SSE event，在首个语义输出前暂存该 attempt 的前置帧。
- 首 token timeout 到期且未输出语义响应时：
  - drop 当前 response body；
  - cancel attempt context；
  - 记录 attempt timeout；
  - 对已配置的单个上游发起下一次 attempt。
- 首个语义输出到达时：
  - 提交 downstream response header；
  - flush 当前成功 attempt 的前置帧和首个语义帧；
  - 后续进入普通 streaming。
- 一旦 downstream 已提交语义输出，后续错误只作为当前流错误处理，不再无感重试。

**Technical design:** Directional state machine, not implementation specification:

```text
AttemptStarted
  ├─ HeaderTimeout -> RetryableFailure
  ├─ UpstreamHTTPError -> RetryPolicyDecision
  └─ SSEOpen
       ├─ NonSemanticFrame -> BufferPrefix
       ├─ FirstTokenTimeout -> RetryableFailure
       └─ SemanticFrame -> CommitAndStream
```

**Patterns to follow:**
- sub2api 的“写出前可 failover，写出后不可 failover”边界。
- htmx/admin 的请求记录依赖 attempt record，而不是临时内存状态。

**Test scenarios:**
- Happy path: 第一个 upstream 首 token 超时，第二个 upstream 正常，客户端只收到第二个流。
- Happy path: 第一个 attempt response header 超时，第二个 attempt 成功。
- Happy path: 成功 attempt 的 `response.created` 前置帧在首 token 到达后按顺序下发。
- Edge case: upstream 先发 ping/comment/created 但无语义 token，仍触发 first token timeout。
- Edge case: 所有 attempts 耗尽，客户端收到 504 OpenAI-compatible error。
- Error path: 首 token 已输出后上游断流，不再尝试第二个 upstream。
- Error path: 客户端断开时当前 attempt 被取消，后续 attempt 不再启动。
- Integration: request record 最终状态显示 `retried_success` 或 `exhausted_timeout`。

**Verification:**
- 测试能证明前序失败 attempt 没有任何 SSE 帧泄漏到客户端。
- 日志能区分 `response_header_timeout` 和 `first_token_timeout`。

- [x] **Unit 6: 请求记录、指标与运维端点**

**Goal:** 让管理员能观察重试行为、定位慢上游，并能关闭自动重试回退普通代理。

**Requirements:** R14, R21, R22, R23

**Dependencies:** Unit 2, Unit 5

**Files:**
- Create: `src/observability/mod.rs`
- Create: `src/observability/metrics.rs`
- Modify: `src/admin/handlers.rs`
- Modify: `templates/requests.html`
- Test: `tests/admin_routes.rs`
- Test: `tests/metrics_routes.rs`

**Approach:**
- request record 聚合展示：
  - 请求状态；
  - upstream；
  - model；
  - attempts；
  - retry_count；
  - final_status。
- 基础 `/metrics` 可先暴露 Prometheus 文本或 JSON，具体实现以依赖成本决定。
- 配置项支持一键禁用自动重试，回退为单 attempt 转发。

**Patterns to follow:**
- 结构化日志字段与数据库记录字段保持同名，减少排障认知成本。

**Test scenarios:**
- Happy path: 请求记录 partial 可通过 htmx endpoint 渲染。
- Happy path: `/metrics` 展示 first token timeout 计数。
- Error path: 指标写入失败不影响代理请求主路径。

**Verification:**
- 管理页面能快速看出当前上游是否经常首 token 超时。
- 关闭自动重试后，首 token 超时不再发起新 attempt。

### Implementation Completion Notes

本轮实现按第一阶段 MVP 收敛完成，和计划初稿相比有以下落地调整：

- 默认绑定地址为 `127.0.0.1`；后台页面不提供登录页，不使用后台授权 token。
- 配置页根据当前访问域名自动展示客户端 Base URL；不再生成代理入口密钥。
- 代理转发不再改写 `Authorization`，`Cookie`/`Set-Cookie` 也按普通端到端 header 透传。
- SQLite 不再保留旧入口密钥表；上游也不再单独存储密钥。
- SSE 首 token 提交后，request/attempt 记录延迟到流结束、流错误或客户端断开时更新，
  避免首 token 后断流被误记为完整成功。
- `/metrics` 第一阶段采用 JSON 计数，不引入 Prometheus 依赖；TTFT 直方图和记录
  retention 清理作为后续运维增强。

### Follow-up Adjustment: 双层代理与运行时缓存

用户在 MVP 后补充了新的运行边界，本计划按以下方向继续演进：

- 新增全局 `policy_enabled`：
  - 关闭时走策略旁路的直接转发层，只做单次上游转发，不读取完整请求体做重放，不做
    响应头/首 token 策略判断；仅过滤协议级 hop-by-hop header。若请求记录开关开启，
    仍记录 request/attempt metadata、完整请求/响应正文与响应头、首 token、完整响应耗时。
  - 开启时走策略层，保留当前响应头超时、首 token 超时、串行重试和后续过滤逻辑。
- 新增 `request_record_enabled`，对直接转发层和策略层均生效：
  - 开启时继续把 request/attempt metadata、完整请求/响应正文、响应头耗时、首 token 耗时、完整响应耗时写入 SQLite。
  - 关闭时不写请求记录；stdout/stderr 不作为业务请求记录存储。
- 配置真源仍是 SQLite，代理请求主路径读取启动时加载的内存快照；管理页面保存配置
  或保存上游 Base URL 后同步刷新内存快照。
- 上游配置收敛为单个全局 Base URL，不提供逐项启用/停用操作。
- 请求记录页面继续读取 SQLite。代理热路径只向 bounded channel 投递记录事件，后台
  writer 异步落库；队列满或写入失败只记录 warn，不影响转发主路径。
- 完整请求/响应正文按 chunk 追加到 `request_payloads`，并记录完整性标记；当前不做
  正文截断，后续需要 retention/清理策略控制 SQLite 体积。

## System-Wide Impact

- **Interaction graph:** 管理页面写配置，代理请求实时读取配置或使用短 TTL cache；
  代理主路径写 request/attempt records；运维页面读取聚合结果。
- **Error propagation:** 上游连接失败、响应头超时、首 token 超时均归一成 attempt
  结果；最终对客户端返回 OpenAI-compatible 错误。
- **State lifecycle risks:** attempt context、response body、stream parser、数据库记录必须
  在取消路径中清理；客户端断开不能继续重试。
- **API surface parity:** `/v1/chat/completions` 与 `/v1/responses` 共享 attempt loop，
  但 semantic token classifier 分开实现。
- **Integration coverage:** 需要 mock upstream 测试完整流转，单元测试不足以证明
  “前序 attempt 帧不泄漏”。
- **Unchanged invariants:** 端口固定 `57999`；HTTP first，WS deferred。

## Risks & Dependencies

- **首 token 语义分类不准。**
  - Mitigation: 用 endpoint-specific fixture 覆盖常见 OpenAI Responses 和 Chat
    Completions 事件；先保守，不把状态帧算 token。

- **取消上游请求后仍可能被上游计费。**
  - Mitigation: 在文档和 UI 中明确 abandoned attempt 仅表示本代理取消，不承诺上游不扣费。

- **缓存首 token 前帧导致内存膨胀。**
  - Mitigation: 对 prefix buffer 设置大小上限；超过上限视为 attempt 失败或直接提交当前流。

- **后台配置暴露敏感信息。**
  - Mitigation: 后台不保存代理侧或上游侧密钥；公网部署应由反向代理保护 `/admin/*`。

- **SQLite 写入影响代理延迟。**
  - Mitigation: request/attempt 记录通过 bounded channel 投递到后台 writer 异步落库，
    失败时不阻塞转发主路径。

## Documentation / Operational Notes

- `README.md` 需要说明：
  - 固定端口 `57999`；
  - 后台无登录、客户端 Base URL 自动推导；
  - 单个上游 Base URL 配置；
  - HeroUI/htmx 静态资产来自本地 `static/vendor/`，不依赖 CDN；
  - response header timeout 与 first token timeout 区别；
  - WS 暂不支持。
- 部署示例建议提供：
  - 本地运行；
  - Docker；
  - systemd 或 launchd 可选。
- 默认安全建议：
  - 建议反向代理只开放 `/v1/*` 给客户端，后台路径按需加额外访问控制。

## Sources & References

- Origin document: `docs/brainstorms/first-token-failover-proxy-requirements.md`
- External reference repo: `Wei-Shaw/sub2api`
- Context7 Axum docs: `/tokio-rs/axum`
- Context7 htmx docs: `/bigskysoftware/htmx/v2.0.4`
- Context7 Actix Web docs: `/websites/rs_actix-web_4_11_0`
- Context7 HeroUI docs: `/heroui-inc/heroui`
