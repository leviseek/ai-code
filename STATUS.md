# NextIDE 状态

## 当前阶段

Phase 0：技术基线与核心内核起步。

## 本轮完成目标

- 建立 Rust workspace。
- 建立 `nextide-kernel` 与 `nextide-cli` 模块边界。
- 增加 VFS、Capability Registry 和 Event Bus 基线。
- 实现 UTF-8 安全的文档编辑事务。
- 实现事务级 Undo/Redo、快照恢复和 revision 冲突检测。
- 增加 5 个内核单元测试。
- 增加 CLI `smoke` 冒烟流程。
- 通过 `cargo fmt --check`、`cargo test --workspace` 和 CLI 冒烟验证。
- 当前工作区测试：17 passed，0 failed。
- Editor Kernel 已支持 `document.changed` 与 `document.saved` 事件。
- 原子保存成功后可通过 Event Bus 发布保存事件。
- 增加 LocalRpcRouter/Typed RPC 基线。
- 增加 PlatformServices、WindowsPlatform 和终端/进程抽象接口（Windows ConPTY/进程实现待接入）。
- Editor Kernel 已支持通过 VFS 打开、编辑、保存和 save-as。
- 增加 `.gitignore`，排除 Rust target、日志、环境文件和 IDE 临时目录。
- TaskManager 支持异步任务句柄：`spawn_async`/`spawn_async_timeout` 立即返回可轮询的 `AsyncTaskHandle`（线程 + `Arc<Mutex>` 共享输出/状态，`AtomicBool` 取消，`Condvar` 完成通知），现有同步 API 保持不变。
- 修复异步 TaskManager 取消一致性：`TaskManager::cancel(id)` 现在通过共享 cancel flag 同时取消 `spawn_async` 任务的后台进程，最终进程停止且 manager 状态与 `AsyncTaskHandle` 一致收敛为 `Cancelled`。
- 修复异步 worker 完成后的 manager 记录输出丢失：`finish_async` 不再用 `Task::finish`/`timeout` 把记录输出 drain 进被丢弃的 `TaskResult`，改为保留输出的终态转换（`Task::finish_retain_output`/`timeout_retain_output`），manager 记录在 Succeeded/Failed/TimedOut 后仍保留完整输出。
- 修复异步 worker panic 导致句柄永久 Running：worker 主体包裹在 panic 安全运行器中，panic 时句柄与 manager 记录收敛为 `Failed` 并唤醒所有 waiter，然后重新抛出 panic；`clear_finished` 因此也能清理这类任务。
- 新增线程安全事件桥 `AsyncTaskEventSink`/`TaskEventReceiver`（`task_events` 模块，std-only）：`spawn_async_with_sink`/`spawn_async_timeout_with_sink` 让后台 worker 把生命周期/输出事件写入 sink，主线程（UI）轮询 receiver 消费后通过 `publish_into` 回放进 `EventBus`（`task.started`/`task.output`/`task.failed`/`task.finished`/`task.timeout`/`task.cancelled`），worker 不再需要持有 `&mut EventBus`；现有 `spawn_async`/`spawn_async_timeout` 行为与签名不变。
- CapabilityRegistry 增加最小可观测查询 API：`ids`/`iter` 以稳定（字典序）顺序遍历已注册 capability，`summary`/`summaries` 返回 `CapabilitySummary`（id/version/permissions 的值类型投影，可跨 RPC/UI 边界），空注册表查询返回空；新增测试覆盖乱序注册后的稳定排序、元数据投影与空注册表。现有 `register`/`authorize`/`invoke`/`get`/`contains`/`len`/`is_empty` 等 API 与行为不变，向后兼容。

## 下一步

1. 接入 VFS 抽象和文件保存。
2. 增加 Capability Registry、Typed RPC 和 Event Bus 基线。
3. 增加 Windows 平台进程/终端接口。
4. 建立 Desktop Shell 原型。
5. 为 Editor Kernel 增加事件订阅和持久化日志。
