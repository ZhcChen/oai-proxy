---
status: complete
priority: p2
issue_id: "001"
tags: [admin-ui, pagination, navigation]
dependencies: []
---

# 管理端请求记录分页与导航状态

## Problem Statement

管理端请求记录当前固定展示最近请求，缺少分页能力；顶部导航没有当前页面激活状态；概览页重复展示请求记录表，和请求记录页面职责重叠。

这些问题会影响后续排查大量代理请求时的可用性，也让页面层级不够清晰。

## Findings

- `src/admin/handlers.rs` 的请求页和请求 partial 当前固定调用 `records::list_recent_requests(..., 100)`。
- `templates/requests.html` 的 htmx 自动刷新未携带分页参数。
- `templates/layout.html` 的导航链接没有 `active` class 或 `aria-current`。
- `templates/dashboard.html` 底部仍 include `partials/requests_table.html`，导致概览页重复显示最近请求。
- `src/storage/records.rs` 已有 `total_requests`，但请求列表 SQL 暂不支持 `OFFSET`。

## Proposed Solutions

### Option 1: 固定页大小分页

**Approach:** 新增分页查询和分页视图，页大小固定为 25。请求页和 partial 都通过 `?page=` 读取当前页，并渲染上一页/下一页操作。

**Pros:**
- 实现简单，适合当前管理端。
- 不引入额外配置项。
- htmx 自动刷新可以稳定刷新当前页。

**Cons:**
- 暂不支持用户自定义每页条数。

**Effort:** 1-2 小时

**Risk:** Low

### Option 2: 页大小可配置分页

**Approach:** 除 `page` 外支持 `page_size` query，并在页面上提供选择器。

**Pros:**
- 更灵活。

**Cons:**
- 需要更多参数校验和 UI 状态维护；当前需求没有明确需要。

**Effort:** 2-3 小时

**Risk:** Medium

## Recommended Action

采用 Option 1：先实现固定页大小分页、导航激活样式、移除概览页请求表，并补充路由/存储测试。后续如记录量继续增加，再追加筛选、搜索和页大小配置。

## Technical Details

**Affected files:**
- `src/admin/handlers.rs`
- `src/storage/records.rs`
- `templates/layout.html`
- `templates/dashboard.html`
- `templates/requests.html`
- `templates/partials/requests_table.html`
- `static/app.css`
- `tests/admin_routes.rs`
- `tests/storage_records.rs`

**Database changes:**
- No migration needed.

## Acceptance Criteria

- [x] `/admin/requests?page=N` 按页展示请求记录，默认 page=1。
- [x] `/admin/partials/requests?page=N` 返回同一页的表格和分页控件。
- [x] 请求记录页面展示总数、当前范围、上一页/下一页状态。
- [x] 顶部导航在概览、配置、上游、请求记录页面显示当前页面激活样式，并带 `aria-current="page"`。
- [x] `/admin` 概览页不再展示请求记录表。
- [x] 补充或更新相关测试，并通过 `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets -- -D warnings`、`git diff --check`。

## Work Log

### 2026-07-14 - Initial Discovery

**By:** Codex

**Actions:**
- 梳理了现有请求列表、概览页、导航模板和记录查询 SQL。
- 确认当前无迁移需求，分页可以复用现有 `request_records.created_at` 索引。
- 形成固定页大小分页方案。

**Learnings:**
- 当前请求记录完整正文已写入 SQLite，分页是请求页继续扩展筛选/详情查看前的基础能力。

### 2026-07-14 - Implementation Complete

**By:** Codex

**Actions:**
- 新增请求记录分页查询和管理端分页视图。
- 为顶部导航加入当前页面激活样式和 `aria-current="page"`。
- 移除概览页请求记录表，避免和请求记录页重复。
- 补充路由和存储层测试。
- 通过格式、测试、clippy、diff 检查，并完成页面截图验证。

**Learnings:**
- 端口 `127.0.0.1:57999` 已有用户进程运行旧版本，视觉验证使用临时 IPv6 loopback 实例 `[::1]:57999`，未影响现有服务。

## Notes

- 用户明确要求后续在 `main` 分支开发并推送。
