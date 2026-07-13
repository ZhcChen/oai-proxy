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

代理会先完整读取并缓存客户端请求体，然后按 attempt 串行请求上游：

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
- 管理员 token：`admin`

如果把 `OAI_PROXY_BIND` 设置为非 loopback 地址，必须显式设置非默认
`OAI_PROXY_ADMIN_TOKEN`，否则服务会拒绝启动。

建议启动时显式设置：

```bash
OAI_PROXY_ADMIN_TOKEN='change-me' \
OAI_PROXY_UPSTREAM_BASE_URL='https://api.example.com' \
OAI_PROXY_UPSTREAM_API_KEY='sk-example' \
cargo run
```

常用环境变量：

- `OAI_PROXY_BIND`：绑定 host，端口仍固定为 `57999`
- `OAI_PROXY_DATA_DIR`：数据目录，默认 `data`
- `OAI_PROXY_DATABASE_URL`：SQLite URL，默认从数据目录生成
- `OAI_PROXY_ADMIN_TOKEN`：后台登录 token
- `OAI_PROXY_UPSTREAM_NAME`：启动时种子的上游名称
- `OAI_PROXY_UPSTREAM_BASE_URL`：启动时种子的上游 base URL
- `OAI_PROXY_UPSTREAM_API_KEY`：启动时种子的上游 API key
- `OAI_PROXY_PROXY_KEY`：可选代理入口 Bearer key；未配置任何 proxy key 时代理入口不鉴权
- `OAI_PROXY_RESPONSE_HEADER_TIMEOUT_MS`：默认响应头超时
- `OAI_PROXY_FIRST_TOKEN_TIMEOUT_MS`：默认首 token 超时
- `OAI_PROXY_MAX_ATTEMPTS`：默认最大 attempt
- `OAI_PROXY_MAX_BODY_BYTES`：默认请求体上限

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

如果没有配置 `OAI_PROXY_PROXY_KEY`，代理入口不要求客户端 Bearer key：

```bash
curl http://127.0.0.1:57999/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-example","stream":true,"messages":[{"role":"user","content":"hi"}]}'
```

如果配置了 `OAI_PROXY_PROXY_KEY`：

```bash
curl http://127.0.0.1:57999/v1/chat/completions \
  -H 'authorization: Bearer your-proxy-key' \
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
- 管理登录、请求记录 partial、`/metrics`
- 非流式代理转发、header/query/body 透传、Authorization 重写、Cookie 不转发
- OpenAI-compatible 413/503 错误
- SSE 语义 token 分类
- 响应头超时重试、首 token 超时重试、attempt 耗尽 504、关闭自动重试
- 首 token 超时后重试时，前序 attempt 的 SSE 帧不会泄漏给客户端

## 注意事项

- 被取消的上游请求只能做到本代理侧尽力关闭连接，不承诺上游一定不会计费。
- 默认串行重试，不做并发抢跑，避免放大上游成本和限流压力。
- 后台是单管理员 token 模式；如果暴露公网，建议在反向代理层额外限制 `/admin/*`。
- 上游 API key 当前存储在本地 SQLite；请限制数据库文件权限并避免把 `data/` 纳入备份或提交。
