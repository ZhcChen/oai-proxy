# oai-proxy

`oai-proxy` 是一个轻量 OpenAI-compatible HTTP/SSE 代理，重点解决上游高负载时
响应头或首个语义 token 长时间排队的问题。

服务固定监听 `57999` 端口，同一端口提供：

- 管理页面：`/admin`
- 转发入口：`/v1/chat/completions`、`/v1/responses`
- 健康检查：`/healthz`
- 基础指标：`/metrics`

第一阶段只完善 HTTP/SSE，不支持 WebSocket。

## 核心行为

代理有两层运行路径：

- **策略旁路的直接转发层**：全局策略层关闭时使用。只选择已配置的单个上游并直接
  流式转发，不做首 token 判断、自动重试或请求体缓存重放；仍保留协议级
  hop-by-hop header 过滤。若请求记录开关开启，会异步记录 request/attempt metadata、
  完整请求/响应正文、响应头耗时、首 token 耗时和完整响应耗时，但不为了记录而改写请求。
  代理客户端不会自动跟随上游 redirect，不启用 reqwest 隐式重试、自动解压或系统代理。
- **策略层**：全局策略层开启时使用。代理会先完整读取并缓存客户端请求体，
  然后按 attempt 串行请求上游：

1. 等待 HTTP response header。
2. 如果上游响应为 SSE，继续等待首个“语义 token”。
3. 若响应头超时或首 token 超时，则取消当前 attempt 并重试下一次 attempt。
4. 只要还没有向客户端输出语义响应，失败 attempt 的 header、SSE 状态帧、ping
   和错误都不会泄漏给客户端。
5. 一旦首个语义 token 已经输出，本次请求就固定在当前 attempt，不再做无感切换。

首 token 不等于任意 SSE 帧。`response.created`、`response.in_progress`、
`ping/comment`、`[DONE]`、仅 role 的 Chat Completions delta 都不会被计为首 token。

## 运行

```bash
cargo run
```

默认配置：

- 监听地址：`127.0.0.1:57999`
- 数据库：`data/oai-proxy.sqlite3`

直接启动后在上游页面配置服务提供方 Base URL。客户端 Base URL 会根据当前访问
域名自动展示，不需要手工设置。请求中的 `Authorization`、自定义 header、query
和 body 会原样转发到上游；上游不再单独配置 API Key。

所有配置保存到 SQLite，服务启动时加载到内存缓存；通过后台页面保存配置或保存
上游 Base URL 后，会立即刷新内存缓存。请求记录也保存到 SQLite，但代理热路径只把
记录事件投递到 bounded channel，由后台 writer 异步落库；队列满或写库失败只记录
warn，不阻塞转发。stdout/stderr 只作为普通运行日志输出，不作为业务请求记录存储。
完整正文按 chunk 追加保存到 `request_payloads` 表，并带有 request/response 是否完整的
标记，便于后续直接用 SQLite 做排查和分析。当前按“完整留存”实现，不截断正文；
长流式响应会增加 SQLite 文件体积，后续应增加 retention/清理策略。

启动命令：

```bash
cargo run
```

常用环境变量：

- `OAI_PROXY_BIND`：绑定 host，端口仍固定为 `57999`
- `OAI_PROXY_DATA_DIR`：数据目录，默认 `data`
- `OAI_PROXY_DATABASE_URL`：SQLite URL，默认从数据目录生成
- `OAI_PROXY_UPSTREAM_BASE_URL`：可选，启动时种子的上游服务地址；也可以在页面配置
- `OAI_PROXY_RESPONSE_HEADER_TIMEOUT_MS`：默认响应头超时
- `OAI_PROXY_FIRST_TOKEN_TIMEOUT_MS`：默认首 token 超时
- `OAI_PROXY_MAX_ATTEMPTS`：默认最大 attempt

## 关键配置

- `policy_enabled`
  - 开启：进入策略层，支持响应头超时、首 token 超时和自动重试。
  - 关闭：进入策略旁路的直接转发层，只做单次透明转发，不读取完整请求体做重放。
- `request_record_enabled`
  - 开启：直接转发层和策略层都会把 request/attempt metadata、完整请求/响应正文、
    响应头耗时、首 token 耗时、完整响应耗时异步写入 SQLite。
  - 关闭：代理仍工作，但不写业务请求记录。
- 上游只保留一个全局 Base URL。未配置时转发入口返回 OpenAI-compatible 503。

## 前端资产

后台页面使用服务端 HTML + Askama + htmx。HeroUI 只 vendoring 本地 CSS，不引入 React
runtime，不使用 CDN。

更新本地资产：

```bash
npm install
npm run vendor:assets
```

生成文件：

- `static/vendor/htmx.min.js`
- `static/vendor/heroui.min.css`

## 转发示例

代理入口不做 API Key 校验；需要发给上游的 `Authorization` 由客户端原样传入：

```bash
curl http://127.0.0.1:57999/v1/chat/completions \
  -H 'authorization: Bearer upstream-api-key' \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-example","stream":true,"messages":[{"role":"user","content":"hi"}]}'
```

## 验证

```bash
cargo fmt
cargo test
```

当前测试覆盖：

- `/healthz` 与 404
- SQLite 默认设置、请求记录与 attempt 记录
- 无登录后台、自动 Base URL 展示、单上游 Base URL 配置、请求记录 partial、`/metrics`
- 非流式代理转发、header/query/body 透传、Authorization/Cookie/Set-Cookie 透传
- 直接透明转发路径的请求记录、响应头耗时、SSE 首 token 耗时和完整响应耗时
- 完整请求/响应正文落库，可通过 `request_payloads` 与 `request_records` 关联分析
- OpenAI-compatible 503/504 错误
- SSE 语义 token 分类
- 响应头超时重试、首 token 超时重试、attempt 耗尽 504、关闭自动重试
- 首 token 超时后重试时，前序 attempt 的 SSE 帧不会泄漏给客户端

## 注意事项

- 被取消的上游请求只能做到本代理侧尽力关闭连接，不承诺上游一定不会计费。
- 默认串行重试，不做并发抢跑，避免放大上游成本和限流压力。
- 后台当前不做登录授权；如果暴露公网，建议在反向代理层限制 `/admin/*`。
  根路径 `/` 只会重定向到 `/admin`。
