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

## 下一步

1. 接入 VFS 抽象和文件保存。
2. 增加 Capability Registry、Typed RPC 和 Event Bus 基线。
3. 增加 Windows 平台进程/终端接口。
4. 建立 Desktop Shell 原型。
5. 为 Editor Kernel 增加事件订阅和持久化日志。
