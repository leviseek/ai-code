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
- 当前工作区测试：161 passed，0 failed。
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
- nextide-shell 命令注册表增加稳定只读查询 API：`command_ids` 以稳定（声明）顺序枚举命令 id，`get_command` 按 id 解析静态描述符，`command_summary`/`command_summaries` 返回 `CommandSummary`（id/title/keywords 的值类型投影，可跨 command palette/UI/插件边界）；新增测试覆盖稳定排序、唯一性、元数据投影与 `command_from_id` 往返。现有 `search`/`COMMANDS`/`command_id`/`command_from_id` 行为与签名不变。
- nextide-shell 命令执行增加最小类型化观测模型：`CommandExecutionResult`（稳定 command id + `CommandOutcome` 三态 applied/noop/failed + 结构化 `CommandError`），新增 `DesktopShell::execute_observed` 返回该模型并发布 `shell.command.executed` 事件（稳定载荷 `id=...,outcome=...[,error=<kind>,message=...]`），失败仍发布 `shell.command.failed` 与错误通知；`CommandErrorKind` 提供 io/not-found/invalid-state/unknown 分类与消息，可经 `std::io::Error` 无损往返（`Io` 保留原始 kind）。`execute`/`execute_checked`/`execute_selected`/`CommandResult` 签名不变，checked/selected 路径改经观测路径（每次执行均发出 `shell.command.executed`）；注册表新增 `command_title`/`is_registered`/`describe_execution` 查询，将执行结果映射回已注册命令元数据。
- VFS 增加只读、类型化的文件元数据查询：`FileSystem::metadata` 返回 `FileMetadata`（`kind`/`len`/`readonly`，`FileKind` 区分 File/Directory/Other，附 `is_file`/`is_dir` 便捷方法），`is_file`/`is_dir` 对缺失路径返回 `Ok(false)`、对访问失败返回 `Err`；rooted 文件系统保持 workspace root 安全约束，`..` 组件、绝对路径越界与嵌套父目录逃逸一律返回 `PermissionDenied`，缺失条目 `metadata` 返回 `NotFound`。新增测试覆盖文件/目录判定、缺失路径与三类路径逃逸；新方法带默认实现，现有 `FileSystem` 实现（LocalFileSystem/MemoryFs/TreeFs/NoopFs）无需改动，API 向后兼容，未引入新依赖。
- `FileSystem::exists` 升级为类型化只读查询：签名从 `exists(path) -> bool` 变为 `exists(path) -> io::Result<bool>`，默认实现基于 `metadata` 给出 `Ok(true)`（工作区内存在）/`Ok(false)`（缺失）/`Err`（路径逃逸或权限错误，`PermissionDenied`），不再把越界路径静默当作 `false`；`LocalFileSystem` 改用默认实现（rooted 安全约束不变），测试伪实现 MemoryFs/TreeFs/NoopFs 与全部调用点同步更新为 `io::Result<bool>`，新增测试覆盖存在/缺失/相对逃逸/绝对越界四类语义。

## 下一步

1. 接入 VFS 抽象和文件保存。
2. 增加 Capability Registry、Typed RPC 和 Event Bus 基线。
3. 增加 Windows 平台进程/终端接口。
4. 建立 Desktop Shell 原型。
5. 为 Editor Kernel 增加事件订阅和持久化日志。
