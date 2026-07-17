# Conduit v2 — 待办清单（Tasks）

> 维护人：Senku / Luffy
> 更新时间：2026-07-17
> 基线：P1 A4/A5/A6/O1 完成；以 workspace test/clippy 结果为准

本文档列出 conduit-v2 当前**未完成 / 部分完成 / 有缺陷**的工作项，按优先级 P0 → P3 排序，作为后续持续跟进的单一信息源。每完成一项请在勾选框内打 `x` 并附上 commit / PR 链接。

---

## P0 — 阻塞主链路，不修无法跑通的 demo

### G1. 网关主入口 `POST /v1/chat/completions` 当前是空 stub

- **位置**：`crates/conduitd/src/routes.rs`
- **现状**：返回 200 空 body，没有跑 L1→L7 完整流水线
- **需要做**：
  - [x] 串起 ingress → router → codec(encode_request) → upstream → codec(decode_response) → egress → sink
  - [x] 非流式路径联通（OpenAI、Anthropic 两个 provider）
  - [x] 流式路径联通（SSE 转发 + chunk 级 codec + 末帧 usage 汇总）
  - [x] 错误路径写入 `TraceEventKind::Error`，HTTP 状态码按 IR error 映射
  - [x] LossReport 落盘到 trace
- **验收**：`curl localhost:xxx/v1/chat/completions` 对接 OpenAI / Anthropic 真实接口，trace 日志中能查到完整事件链

### G2. `conduit-router` 单测编译失败

- **位置**：`crates/conduit-router/` 测试 fixture
- **现状**：~~`RouteTarget` 初始化缺 `base_url` 字段，2 处编译错误~~ **已修复**
- **需要做**：
  - [x] 修复 fixture，让 `cargo test -p conduit-router` 绿（26/26 passed）
  - [x] CI 加 `cargo nextest run --workspace` 卡门禁（`.github/workflows/ci.yml` test job）
- **验收**：workspace 全量测试通过 ✅

### G3. `conduitctl` 五个核心子命令只有 CLI 骨架，没有 handler

- **位置**：`crates/conduitctl/src/cmd/{provider,route,key,status}.rs`
- **现状**：~~`clap` 定义齐全，`run()` 未实现~~ **已实现**
- **需要做**（每条都对应 admin API，HTTP 复用 trace/budget 已有的 client 模板）：
  - [x] `conduitctl status` —— 调用 `GET /health` + 汇总 daemon 关键指标
  - [x] `conduitctl provider list/add/remove/health`
  - [x] `conduitctl route list/get/remove`
  - [x] `conduitctl key list/create/revoke`
- **验收**：README Quick Start 里所有命令实际可跑

### A1. 完整审计链路（Complete Audit Trail）断链 ✅ 已修复

- **位置**：`crates/conduit-pipeline/src/handle.rs`、`crates/conduit-pipeline/src/stream_probe.rs`
- **现状（修复前）**：`run_non_stream` 返回 `events` 但 `routes.rs` 用 `_events` 丢弃；`egress::finalize` 没有 caller；流式路径没有任何 trace 收尾事件；错误路径不写 `Error` 事件
- **修复内容**：
  - [x] `run_non_stream`：merge_usage → compute_cost → push UpstreamResponse → egress::finalize → flush all events to sink
  - [x] `run_stream`：pre-stream routing events flush + `InstrumentedStream` wrapper（TTFB、usage 累加、stream end 时 FinalUsage + UpstreamResponse + quota.record）
  - [x] 错误路径：push `TraceEventKind::Error` 并 flush 到 sink
- **验收**：一次请求的 trace log 包含 RequestReceived → RoutingDecided → UpstreamResponse → FinalUsage 完整链

### A2. TraceSink 改 event bus / quota.record 从未调用 ✅ 已修复（P0 范围）

- **位置**：`crates/conduit-pipeline/src/handle.rs`、`crates/conduit-pipeline/src/stream_probe.rs`
- **现状（修复前）**：`quota.record()` 从未被调用，预算永远不扣减
- **修复内容**：
  - [x] 非流式：`handle.rs` 在 finalize 后显式调用 `quota.record()`
  - [x] 流式：`InstrumentedStream` 在 stream 消费完毕后 `tokio::spawn` 调用 `quota.record()`
- **遗留（P2）**：完整 fan-out event bus（目前 quota.record 是 pipeline 的直接调用；将来新增 metrics、UI push 等订阅者应改造为 sink 的 subscribers）

### A3. 重试循环缺失 ✅ 已修复

- **位置**：`crates/conduit-pipeline/src/handle.rs`
- **现状（修复前）**：`stage::should_retry` 实现完整，但 `handle.rs` 主循环里一次调用失败即直接 502，不走第二个 fallback target
- **修复内容**：
  - [x] `run_non_stream`：`loop { ... if should_retry { route_request; continue } else { flush error + return Err } }`
  - [x] `run_stream`：同上，连接建立失败可 retry（TTFB 前）；连接成功后由 stream 内部处理
  - [x] 重路由时重新从 secret backend 取最新 secret
- **验收**：配置两个 provider 的 Fallback 路由，关掉第一个，请求自动走第二个

---

## P1 — 核心能力残缺，影响"可审计 + 可观测"卖点

### A4. Provider dispatch 是 stringly-typed 硬编码（P1） ✅

- **位置**：`crates/conduit-pipeline/src/provider.rs`（原 `handle.rs` match）
- **现状**：~~string match~~ **已实现** `ProviderKind` + `dispatch_non_stream` / `dispatch_stream`；handle 只调用 dispatch
- **需要做**：
  - [x] 抽出 `enum ProviderKind { OpenAi, Anthropic }` + 严格 `FromStr` / `parse`
  - [x] pipeline 调用点只 dispatch（新 kind 改 `provider.rs`，不改 handle 编排）
- **验收**：加第三个 provider 不需要改 handle.rs ✅

### A5. 路由表/pipeline deps 每请求重建（P1） ✅

- **位置**：`crates/conduitd/src/{routes,state,server}.rs`、`conduit-pipeline` `PipelineDeps`
- **现状**：~~每请求 clone + new handle~~ **已实现** `ArcSwap` + 启动时共享 `PipelineHandle`
- **需要做**：
  - [x] routing_table 改为 `Arc<ArcSwap<RoutingTable>>`，admin 改表 `store(new)`，读路径无锁
  - [x] `PipelineHandle` 在 daemon 启动时构一次存 `DaemonState`，handler 直接复用
- **验收**：热路径 `load_full()` 无写锁；admin reload 后后续请求可见新表 ✅（单测覆盖；未跑 cargo bench）

### A6. Append-only log 缺 fsync 保证（P1） ✅（写路径 fsync；CRC trailer 仍 P2）

- **位置**：`crates/conduit-trace/src/log.rs`（`LogWriter::append`）
- **现状**：~~仅 flush~~ **已实现** `DurabilityMode::{Fsync, BestEffort}`，默认 Fsync 在 append batch 末尾 `sync_data`
- **需要做**：
  - [x] 每 batch 末尾加 `file.sync_data().await`（Fsync 模式）
  - [x] 暴露 `DurabilityMode::{ Fsync, BestEffort }`（GroupCommit 未做）
  - [ ] segment 加 trailer/CRC32C，启动时检测截断 frame 并恢复（deferred）
- **验收**：Fsync 模式 write-path 计数 + re-open 可读；kill -9 混沌测试未跑

### O1. `conduitctl trace tail` / `trace replay` 未实现 ✅（dry-run）

- **位置**：`crates/conduitctl/src/cmd/trace.rs`、`conduitd` admin traces API
- **现状**：~~mock / bail~~ **已实现** SSE tail + dry-run replay
- **需要做**：
  - [x] `trace tail`：admin `GET /admin/traces/stream` + CLI SSE 订阅
  - [x] `trace replay <trace-id>`：默认 dry-run（`POST /admin/traces/{id}/replay?dry_run=true`），打印 intended provider/target + request summary，不调上游、不计费
  - [ ] live replay（`--execute` / `dry_run=false`）仍未实现
- **验收**：tail 订阅真实 SSE 事件；replay dry-run 输出 provider + summary 且 `billed=false`

### O2. Codec 流式实现完整性确认

- **位置**：`crates/conduit-codec/src/openai/stream.rs`、`crates/conduit-codec/src/anthropic/stream/`
- **现状**：文件齐，`encode_chunk` / `decode_chunk` 实现深度未审
- **需要做**：
  - [ ] 用真实 SSE 录制建立 `insta` 快照（OpenAI: tool_call、function_call、finish_reason=length / stop / tool_calls；Anthropic: `message_start` → `content_block_delta` → `message_delta` → `message_stop`、`input_json_delta`、错误事件）
  - [ ] proptest：IR chunk 流 → wire → IR 流 roundtrip 等价
  - [ ] 标记并写入 LossReport 的降级点（Anthropic 不支持的 OpenAI 字段，反之亦然）
- **验收**：两个 provider 全部流式 finish_reason / tool_call / error 路径都有快照覆盖

### O3. Codec 测试三件套门禁未跑齐

- **现状**：`insta` / `proptest` / `wiremock` 依赖配齐，但实际样本和覆盖率未量化
- **需要做**：
  - [ ] 给每个 provider 至少 1 个非流式 + 2 个流式 wiremock 端到端用例
  - [ ] codec roundtrip proptest 跑过 1k 用例 0 失败
  - [ ] insta 快照纳入 PR 必过检查
- **验收**：CI 输出 codec 覆盖矩阵

### O4. Secret backend 审计事件未写 trace

- **位置**：`crates/conduit-secret/src/audit.rs` 与 `conduit-trace`
- **现状**：`audit.rs` 自己记录，但未汇入统一 trace 事件流
- **需要做**：
  - [ ] secret put/get/delete/downgrade 触发 `TraceEventKind::SecretAccessed`（IR 已有事件枚举位置需要扩展）
  - [ ] S1 → S2 降级在 trace 中标记为 `severity = warn`
- **验收**：UI 里能在 trace 时间轴看到 secret 操作

---

## P1 — OAuth 订阅账号（Claude / Codex / Grok）

### OAuth1. Provider OAuth 支持 ✅

- **位置**：`crates/conduit-oauth/`、`conduit-upstream`、`conduit-pipeline`、`conduitd` oauth admin、`conduitctl oauth`、UI Providers
- **完成内容**：
  - [x] `conduit-oauth`：PKCE、Claude/Codex 授权码、Grok Device Code、credential JSON、refresh singleflight
  - [x] 热路径 `CredentialResolver`：读 secret → 近过期刷新 → Bearer + 扩展头
  - [x] `ProviderKind`：`claude-oauth` / `codex-oauth` / `grok-oauth`
  - [x] Codex 最小 Responses codec（`/responses`）
  - [x] Admin：`/admin/oauth/{kind}/start`、sessions、cancel、refresh + 本机 callback
  - [x] CLI：`conduitctl oauth start|status|cancel|refresh|list`
  - [x] UI：Providers 页「OAuth 登录」
  - [x] Claude OAuth relay 全量对齐 CLIProxyAPI：Chrome TLS（wreq）、Firefox token TLS、cloak/cch/tool-remap/signature/cache、headers（`conduit-upstream/src/claude_oauth/`）
- **验收**：`cargo test -p conduit-oauth` + codec/pipeline/conduitd 相关单测通过；真实账号需本机跑 OAuth 登录

---

## P2 — 工程质量与可维护性

### Q1. 迁移目录 `migrations/` 是空的，schema 嵌在 Rust 代码里

- **现状**：`conduit-store/src/schema.rs` 用 `CREATE TABLE IF NOT EXISTS` 启动时执行
- **风险**：演进期一旦改字段，幂等 DDL 无法承担列变更 / 数据迁移
- **需要做**：
  - [ ] 引入 `sqlx::migrate!()`，将 schema 拆为 `migrations/0001_init.sql` 等版本化文件
  - [ ] `conduit-store` 启动时跑迁移，移除运行时 `CREATE TABLE`
  - [ ] 写迁移指引（如何新增 / 如何 rollback）
- **验收**：`sqlx migrate info` 显示版本链

### Q2. Clippy / 未使用导入告警

- **现状**：~~全 workspace 有 16+ warnings~~ **已全部清零**（2026-07-17 复核：OAuth relay 等新增代码引入的回归警告已清理）
- **需要做**：
  - [x] 清空所有 warning（`cargo clippy --all-targets` 零警告）
  - [x] CI `cargo clippy --all-targets -- -D warnings` 已落实（`.github/workflows/ci.yml` clippy job）
- **验收**：CI 红绿能反映 lint 状态 ✅

### Q3. 集成测试目录全空

- **现状**：所有测试都嵌在各 crate `lib.rs` 中，`tests/` 目录为 0
- **需要做**：
  - [ ] 顶层 `tests/end_to_end.rs`：启动 daemon + wiremock provider + 一次完整 chat completion，验证 trace 落盘 + budget 扣减
  - [ ] 顶层 `tests/admin_api.rs`：覆盖 admin CRUD（provider / route / key / budget）
- **验收**：`cargo nextest run` 能跑 E2E

### Q4. CI / 发布管线未建立（CI 已有，release 未建）

- **现状**：`.github/workflows/ci.yml` 已存在：fmt + clippy(`-D warnings`)+ nextest(Linux/macOS/Windows)+ cargo deny + cargo audit + llvm-cov 覆盖率。缺 release 管线
- **需要做**：
  - [x] `ci.yml`：fmt + clippy + nextest + cargo deny + cargo audit
  - [ ] `release.yml`：tag 触发，产出 macOS arm64 / x86_64、Linux x86_64、Windows x86_64 的单文件二进制 + 校验签名
  - [ ] Tauri UI bundle 独立产物
- **验收**：tag push 后 GitHub Release 自动出现

### Q5. Tauri 后端绑定未审计 ✅

- **位置**：`conduit-ui/src-tauri/`、`ARCHITECTURE.md`
- **现状**：~~未定形态~~ **已选定 A**：Tauri = 窗口壳；Svelte 经 loopback HTTP 调 admin（默认 `127.0.0.1:4001`）。无 admin IPC。
- **需要做**：
  - [x] 决定形态 A 并写入 ARCHITECTURE.md
  - [x] 前端 admin client 与 daemon 契约对齐（`api_key` / `key` / budget envelope / traces）
  - [x] Traces 视图：list / detail / SSE tail / dry-run replay
- **验收**：文档与实现一致；`npm test` + `npm run build` 通过

### Q6. Pipeline stage 单测待补

- **位置**：`crates/conduit-pipeline/src/`
- **现状**：`handle.rs` + `stream_probe.rs` 重写后逻辑更完整，但 pipeline crate 单测仍为 0
- **需要做**：
  - [ ] 补 `run_non_stream` 单测（mock upstream + mock quota + 验证 trace 事件链）
  - [ ] 补 `InstrumentedStream` 单测（验证 TTFB、usage 累加、finalize 时序）
  - [ ] 补 retry loop 单测（第一个 provider 超时，第二个成功）
- **验收**：pipeline crate 单测覆盖率 ≥ 70%

### Q7. TraceSink 改完整 fan-out event bus（A2 遗留）

- **位置**：`crates/conduit-trace/src/sink.rs`
- **现状**：A2 P0 修复后 quota.record 由 pipeline 显式调用；理想架构是 FinalUsage 事件驱动所有副作用
- **需要做**：
  - [ ] TraceSink 改为持有 `Vec<Arc<dyn EventSubscriber + Send + Sync>>`
  - [ ] 注册 `QuotaSubscriber`（on FinalUsage → record）、`UiBroadcast`（SSE push）等
  - [ ] pipeline 只 `sink.send(event)`，所有副作用自动触发
- **验收**：新增一个订阅者无需改 pipeline crate

---

## P3 — 可观测性与文档完善

### D1. OpenTelemetry / Prometheus 指标 ~~尚未串通~~ 已决定不做

- **结论（2026-07-17）**：local-first 个人网关不需要 OTLP/Prometheus 导出。`tracing-opentelemetry`、`opentelemetry{,_sdk,-otlp}`、`metrics`、`metrics-exporter-prometheus` 依赖已从未使用状态中移除（此前代码零引用，纯依赖负担）。可观测性由 trace 日志 + SQLite 索引 + admin API 承担。如未来确有需要再单独评估引入。

### D2. ARCHITECTURE.md 未写部分补齐

- **现状**：覆盖处理流程 + 数据模型 + 安全模型，缺：
- **需要做**：
  - [ ] 错误分类与传播（IR error → HTTP status 映射表）
  - [ ] 配置文件 schema（`conduit.toml` 完整字段说明）
  - [ ] 部署形态（standalone / sidecar / kubernetes ConfigMap 注入策略）
  - [ ] 升级 / 回滚指南
- **验收**：新开发者可在 30 分钟内读完跑起来

### D3. 开源项目基础物料

- **现状**：仓库已有 README / ARCHITECTURE / deny.toml / rustfmt.toml
- **需要做**：
  - [ ] `LICENSE`（建议 Apache-2.0 或 MIT）
  - [ ] `CONTRIBUTING.md`（开发流程、PR 模板、commit 规范）
  - [ ] `CODE_OF_CONDUCT.md`
  - [ ] `SECURITY.md`（披露渠道、PGP key、SLA）
  - [ ] `CHANGELOG.md`（采用 Keep a Changelog 格式）
  - [ ] PR / Issue 模板（`.github/ISSUE_TEMPLATE/`）
- **验收**：满足 OpenSSF Scorecard 至少 7 分

### D4. 性能基准缺失

- **需要做**：
  - [ ] `criterion` benchmark：codec encode/decode、router lookup、quota check、trace append
  - [ ] 基线数字写入 ARCHITECTURE.md，回归 > 10% 在 CI 拦截
- **验收**：`cargo bench` 跑出报告并归档

---

## 当前完成度小结（2026-05-18 更新）

| 模块 | 完成度 | 说明 |
|---|---|---|
| conduit-ir | ✅ 100% | 类型齐全 |
| conduit-codec | 🟡 ~70% | 流式 / loss 覆盖待补 |
| conduit-secret | 🟢 ~90% | 缺与 trace 联动 |
| conduit-store | ✅ ~95% | 缺正式 migrations |
| conduit-trace | ✅ ~95% | 写路径 Fsync 已有；缺 CRC trailer |
| conduit-quota | ✅ ~90% | 测试可加厚 |
| conduit-router | ✅ ~95% | 单测全绿（26/26） |
| conduit-upstream | 🟢 ~85% | warning 已清零 |
| conduit-pipeline | 🟢 ~90% | A4 typed dispatch 已完成；单测仍可加厚 |
| conduitd | 🟢 ~85% | ArcSwap 热路径 + traces admin SSE/replay |
| conduitctl | 🟢 ~90% | trace tail/replay dry-run 已实现 |
| conduit-ui | ✅ 已重写（2026-07-17） | Svelte 5 operator console：Live Monitor（SSE rollup + lagged 状态）、trace 四 pane 审计、route multi-target wizard、统一 Confirm/Palette；零在线资源；49 tests + svelte-check 0 警告（见 `docs/design/conduit-ui-rewrite.md`） |
| 测试 / CI / 发布 | 🟡 ~45% | 单元测试+CI 有；E2E/release 未建 |

**整体可上线度：~78%**（P1 A4/A5/A6/O1 已完成；CRC trailer / live replay / O2–O4 仍待做）。

---

> 维护规范：完成任意一项后请勾选并附 PR 链接；新发现的 TODO 一律录入对应优先级段落，禁止散落在 commit message 里。
