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

直接启动后到配置页生成代理 API Key，并在上游页面配置服务提供方地址与上游 Key。
客户端 Base URL 会根据当前访问域名自动展示，不需要手工设置。

启动命令：

```bash
cargo run
```

常用环境变量：

- `OAI_PROXY_BIND`：绑定 host，端口仍固定为 `57999`
- `OAI_PROXY_DATA_DIR`：数据目录，默认 `data`
- `OAI_PROXY_DATABASE_URL`：SQLite URL，默认从数据目录生成
- `OAI_PROXY_UPSTREAM_NAME`：可选，启动时种子的上游名称
- `OAI_PROXY_UPSTREAM_BASE_URL`：可选，启动时种子的上游服务地址；也可以在页面配置
- `OAI_PROXY_UPSTREAM_API_KEY`：可选，启动时种子的上游 API key；也可以在页面配置
- `OAI_PROXY_PROXY_KEY`：可选，启动时种子的代理入口 Bearer key；也可以在配置页生成
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

如果还没有在配置页生成 API Key，代理入口不要求客户端 Bearer key：

```bash
curl http://127.0.0.1:57999/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-example","stream":true,"messages":[{"role":"user","content":"hi"}]}'
```

如果已经在配置页生成 API Key：

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
- 无登录后台、自动 Base URL 展示、页面生成代理 API Key、请求记录 partial、`/metrics`
- 非流式代理转发、header/query/body 透传、Authorization 重写、Cookie 不转发
- OpenAI-compatible 413/503 错误
- SSE 语义 token 分类
- 响应头超时重试、首 token 超时重试、attempt 耗尽 504、关闭自动重试
- 首 token 超时后重试时，前序 attempt 的 SSE 帧不会泄漏给客户端

## 注意事项

- 被取消的上游请求只能做到本代理侧尽力关闭连接，不承诺上游一定不会计费。
- 默认串行重试，不做并发抢跑，避免放大上游成本和限流压力。
- 后台当前不做登录授权；如果暴露公网，建议在反向代理层限制 `/admin/*`。
  根路径 `/` 只会重定向到 `/admin`。
- 上游 API key 当前存储在本地 SQLite；请限制数据库文件权限并避免把 `data/` 纳入备份或提交。
