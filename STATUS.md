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
- 当前工作区测试：208 passed，0 failed（内核 156 + shell 51 + doc-test 1）。
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
- VFS 新增类型化、稳定排序的目录枚举 API：`FileSystem::read_dir(path) -> io::Result<Vec<FileEntry>>`，`FileEntry` 含 `name`/`path`/`kind`/`len`（`path` 为相对请求目录的完整条目路径），条目按名称字典序稳定排序；缺失路径明确返回 `NotFound`、非目录路径明确返回 `NotADirectory`（含清晰错误消息）、越界路径继续返回 `PermissionDenied`，rooted 文件系统根安全约束不变；trait 默认实现返回 `Unsupported`，现有 `FileSystem` 实现（LocalFileSystem/MemoryFs/TreeFs/NoopFs）无需改动、编译与行为兼容，`LocalFileSystem::list` 改为委托 `read_dir`（排序语义与返回内容保持一致），新增测试覆盖类型字段与长度、稳定排序顺序、缺失/非目录/越界错误、空目录与默认 Unsupported 兼容性。
- 新增纯 std 的 workspace 全局子串搜索模块（`search` 模块，`SearchQuery`/`SearchOptions`/`SearchMatch`，入口 `search::search(fs, &query)` 与 `query.run(fs)`）：基于任意 rooted `FileSystem` 递归 `read_dir`，返回每个命中的相对路径/1 基行号/字节列偏移/行文本；跳过二进制（非 UTF-8 或含 NUL）与不可读/被拒文件及子目录（静默继续），仅搜索根目录错误向上传播；结果按 `(path, line, column)` 稳定排序并截断到 `max_results`；`case_sensitive`（默认开）、`max_depth`（默认 64，限制含符号链接环的递归）、`"."` 根归一化保证路径不逃逸工作区根。新增 12 个测试覆盖文本命中、大小写、结果上限、稳定排序、二进制/NUL 跳过、不可读文件与目录跳过、符号链接越界与环、空 needle、根错误传播、多字节列偏移与默认选项。未引入新依赖，现有 API 完全兼容。
- search 模块新增可复用的匹配抽象 `SearchMatcher`（`Literal` 字面模式与 `Glob` 通配符模式，`find_columns`/`is_match` 可脱离文件系统对任意文本复用）：`SearchQuery` 新增 `matcher` 字段并提供 `SearchQuery::glob` 构造器，`SearchQuery::new` 仍默认为字面子串搜索（行为不变），现有 API 完全兼容；Glob 为简单通配符匹配——`*` 匹配任意字符序列（含空）、`?` 恰好一个字符、按子串匹配且首部 `*` 只扩大搜索窗口（`*error*` 等价于"包含 error"并报告 error 的列偏移），明确是 glob 而非正则：字符类/分组/锚点/转义等一律按字面匹配；实现为基于字符的动态规划（按行 O(pattern×line) 时间、O(line) 内存），纯 std 无新依赖。新增 11 个测试覆盖字面/通配符列偏移、空模式、纯 `*` 模式、多列命中、大小写、多字节字符与正则元字符按字面匹配，以及 `SearchQuery::glob` 的端到端查询。
- 新增纯 std 的 Git 状态能力（`git` 模块：`GitStatusEntry`/`GitFileState`/`GitStatusCode`/`GitStatusSnapshot`，入口 `git::status(root) -> io::Result<GitStatusSnapshot>`）：通过 `std::process::Command` 执行 `git -C <root> status --porcelain=v1`（不经过 shell，root 作为 `-C` 的单个参数传入，不会被解释为 shell 语法），解析全部 XY 状态码（`M`/`T`/`A`/`D`/`R`/`C`/`U`/`?`/`!`，未知码保留为 `Unknown`），重命名/复制条目保留 `path`（当前路径）+ `original`（来源路径）两个字段，解码 git 的 C 风格引号路径（`"with space \344\270\255\346\226\207.txt"` 解码为 `with space 中文.txt`），条目按路径稳定排序；root 安全约束：root 必须存在（缺失返回 `NotFound`）且为目录（非目录返回 `NotADirectory`），git 可执行文件缺失返回 `NotFound`，git 非零退出返回 `Other` 并携带 git 的 stderr（如 "not a git repository"），任何绝对路径或含 `..` 组件的解析路径一律以 `InvalidData` 拒绝，快照不可能指向仓库之外的路径；按 porcelain v1 语义路径相对仓库顶层报告，当 `root` 即仓库顶层时（工作区根的正常情形）所有路径即相对 `root`。新增 18 个测试覆盖全部状态码解析、重命名/引号/八进制转义/含空格路径、稳定排序（含同键稳定性）、staged/unstaged/untracked 分类、root 校验与非 git 错误、路径安全与真实临时仓库集成。未引入新依赖，现有 API 完全兼容。

## 下一步

1. 为 Editor Kernel 增加订阅式事件接口与持久化编辑日志（`document.changed`/`document.saved` 等事件已可经 `EventBus` 轮询/排空，订阅式 API 与持久化日志尚未实现）。
2. 接入 Windows ConPTY 终端后端（`PlatformServices`/`TerminalSession` 接口与 `WindowsPlatform` 进程 spawn/terminate/状态查询已就绪，ConPTY 实现待接入）。
3. 增加当前文件搜索与正则表达式搜索（全局子串搜索已实现；通配符搜索已通过 `SearchMatcher::Glob` 支持，完整正则需引入依赖或自研引擎，暂不在纯 std 模块内实现）。
4. 增加 Git Diff 与提交服务（Git 状态已实现：`git::status` 解析 `--porcelain=v1` 快照，Diff、暂存与提交操作尚未实现）。
5. 建立 Windows 安装工程与 CI（编译、测试、打包）流水线。
