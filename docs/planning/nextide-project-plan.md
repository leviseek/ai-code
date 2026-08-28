# NextIDE 下一代编码 IDE 项目总规划

> 版本：0.1-draft  
> 目标平台：Windows 10/11 优先；macOS 后续支持  
> 产品定位：本地优先、Rust 核心、协议兼容、AI 原生、能力安全、协作可演进

## 1. 产品目标与边界

### 1.1 产品目标

NextIDE 融合 VSCode 的生态兼容能力与 Zed 的原生性能、低延迟和协作思路，形成以下产品闭环：

```text
开发者输入
  -> 桌面交互壳
  -> Rust 编辑器内核
  -> 工作区索引与语言工具
  -> 任务 / 测试 / 调试
  -> AI Agent 规划、执行、Patch、Review
  -> 可选云端模型与多人协作
```

### 1.2 不可妥协的原则

- **Local-first**：编辑、保存、跳转、Git、终端和基础索引不依赖网络。
- **Editor-kernel-centered**：用户、Agent、扩展、协作操作统一进入编辑事务流。
- **Protocol-compatible**：优先兼容 LSP、DAP、Git、终端和标准测试/格式化工具。
- **Capability-secured**：文件、终端、网络、密钥和模型调用均采用最小权限。
- **AI-provider-neutral**：模型通过 Provider Adapter 接入，不绑定单一厂商。
- **Cross-platform by design**：核心层不依赖 Win32，平台差异收敛到适配层。

### 1.3 V1 范围

#### 必须交付

- Windows 10/11 x64 安装包、便携包和自动更新。
- 文件夹/多根工作区、文件树、标签页、布局和设置。
- 高性能文本编辑、多光标、Undo/Redo、代码折叠和大文件处理。
- 当前文件、全局、正则搜索与替换。
- Git 状态、Diff、Stage、Commit、Branch 和冲突提示。
- LSP：补全、跳转、引用、Rename、Diagnostics、Code Action、Format。
- 任务、终端、构建、测试和基础 DAP 调试。
- AI Chat、选区理解、代码生成、多文件 Patch 和 Diff Review。
- 日志、崩溃恢复、工作区恢复、设置迁移和基础诊断。

#### V1 不纳入核心承诺

- 100% VSCode 扩展兼容。
- 自建完整插件市场。
- 多人实时协作。
- 完整远程开发和容器编排。
- 所有语言的深度调试能力。
- 内置数据库、Docker、Kubernetes 和云资源管理。
- 企业级组织、计费和团队管理。

## 2. 目标架构

### 2.1 逻辑分层

| 平面 | 模块 | 责任 | V1 优先级 |
|---|---|---|---|
| 交互层 | Desktop Shell | GPU UI、窗口、布局、命令面板、面板 | P0 |
| 核心层 | Editor Kernel | 文档状态、编辑事务、多光标、Undo/Redo | P0 |
| 数据层 | VFS / Workspace Store | 文件、SQLite、Git、缓存、密钥引用 | P0 |
| 智能层 | Workspace Graph | AST、Symbol、依赖、语义上下文 | P1 |
| 工具层 | Language Tool Bus | LSP、DAP、Formatter、Test Adapter | P1 |
| 执行层 | Task / Terminal Runtime | PTY、Build、Test、Debug、进程隔离 | P1 |
| AI 层 | Agent Orchestrator | Plan、Tool、Patch、Review、Verify | P2 |
| 安全层 | Trust Center | Capability、审批、脱敏、审计、密钥 | P2 |
| 扩展层 | Extension Sandbox | WASM 扩展和隔离兼容宿主 | P3 |
| 云端层 | Model / Remote Gateway | 多模型、远程计算、策略控制 | P2/P4 |
| 协作层 | CRDT Relay | Presence、离线合并、多人会话 | P4 |

### 2.2 核心数据流

#### 编辑路径

```text
用户输入 -> Desktop Shell -> Editor Transaction
                                  ├-> VFS / SQLite / Git
                                  ├-> Workspace Graph
                                  ├-> LSP / DAP
                                  └-> 协作事件
```

#### AI 路径

```text
Editor Kernel + Workspace Graph
              -> Agent Orchestrator
              -> Provider Adapter / Model Gateway
              -> Tool Contract
              -> Trust Center
              -> Patch / Task / Test
              -> User Review
              -> Editor Transaction
```

#### 协作路径

```text
本地编辑事务 -> CRDT 编码 -> Relay（可选）
             <- 远端操作 <-
             -> 冲突检测、离线合并、Presence
```

### 2.3 技术约束

- 核心语言：Rust。
- UI：高频编辑路径使用原生/GPU 渲染；React/WebView 仅用于低频扩展界面或兼容面板。
- 图形抽象：wgpu 或同等跨平台 GPU 抽象，Windows 使用 DirectX 优先并保留 fallback。
- 文本模型：Rope、Piece Tree 或混合模型，必须支持增量编辑和大文件。
- 解析：Tree-sitter 或同类增量解析器。
- 存储：SQLite + 本地全文/语义索引。
- 协议：LSP、DAP、JSON-RPC；内部接口采用版本化 Typed RPC。
- 扩展：WASM/WASI-first，Node/VSCode 兼容扩展运行在隔离进程。
- Windows 终端：ConPTY、PowerShell、CMD、Git Bash、WSL 适配。
- macOS 适配：PTY、Metal/wgpu、Keychain、App Bundle、Notarization。

## 3. 可执行工作分解（WBS）

### A. 产品与架构治理

- [ ] A01 冻结 V1 用户画像和核心场景。
- [ ] A02 输出 PRD、功能优先级和明确非目标。
- [ ] A03 建立 ADR 模板和架构决策记录。
- [ ] A04 定义模块边界、依赖规则和 API 所有权。
- [ ] A05 建立版本号、发布分支和变更管理规范。
- [ ] A06 定义数据收集、隐私和云端默认策略。

### B. 桌面壳与 UI

- [ ] B01 创建 Rust Workspace 和 Desktop Shell。
- [ ] B02 实现 Windows 窗口、多窗口和高 DPI。
- [ ] B03 实现 GPU 渲染抽象和主题系统。
- [ ] B04 实现布局、Dock、面板、标签页和状态栏。
- [ ] B05 实现命令注册、快捷键和 Command Palette。
- [ ] B06 实现设置、键位映射和配置迁移。
- [ ] B07 实现托盘、通知、文件关联和右键打开。
- [ ] B08 设计 macOS Platform Adapter，不调用 Windows API 进入核心层。

### C. 编辑器内核

- [ ] C01 评估 Rope / Piece Tree / 混合文本模型。
- [ ] C02 定义 Document、Buffer、Selection、Cursor 数据模型。
- [ ] C03 实现 Edit Transaction 和版本号。
- [ ] C04 实现 Undo/Redo、批量修改和事务回滚。
- [ ] C05 实现多光标、选择、替换、折叠和增量更新。
- [ ] C06 实现大文件加载、分页和内存回收。
- [ ] C07 实现文件变化、冲突和恢复机制。
- [ ] C08 暴露稳定的 Editor Kernel API。
- [ ] C09 建立编辑延迟、内存和大文件基准测试。

### D. 工作区与数据

- [ ] D01 实现 VFS：磁盘、内存、临时文件和虚拟资源。
- [ ] D02 实现 SQLite 工作区元数据。
- [ ] D03 实现 Git 状态、Diff 和提交服务。
- [ ] D04 实现 Gitignore、隐藏目录和文件过滤。
- [ ] D05 实现工作区缓存、索引缓存和清理策略。
- [ ] D06 对接 Windows Credential Manager。
- [ ] D07 设计 macOS Keychain 适配。
- [ ] D08 实现崩溃后的工作区和文档恢复。
- [ ] D09 实现版本升级和数据迁移。

### E. 语言和开发工具

- [ ] E01 实现 LSP Client 和 Server 生命周期管理。
- [ ] E02 实现补全、Hover、跳转、引用、Rename、Diagnostics。
- [ ] E03 实现 Code Action、Format 和代码修复。
- [ ] E04 实现 DAP Client、断点、单步、调用栈和变量。
- [ ] E05 实现 Formatter、Test Adapter 和结果模型。
- [ ] E06 实现 Server 超时、崩溃重启和日志诊断。
- [ ] E07 实现语言 Server 下载、缓存和版本选择。
- [ ] E08 首批验证 TypeScript、Python、Rust、Go、C# 中至少三种。

### F. 终端、任务和进程

- [ ] F01 实现 ConPTY 终端后端。
- [ ] F02 支持 PowerShell、CMD、Git Bash。
- [ ] F03 设计 WSL 适配，不让 WSL 逻辑污染核心层。
- [ ] F04 实现任务配置、依赖、环境变量和工作目录。
- [ ] F05 实现任务取消、输出解析和测试结果。
- [ ] F06 实现进程树管理、超时和资源限制。
- [ ] F07 实现调试进程与终端的生命周期协调。
- [ ] F08 为 macOS PTY 预留平台接口。

### G. AI Agent

- [ ] G01 定义 Provider Adapter 和模型配置格式。
- [ ] G02 定义 Context Provider、Token Budget 和脱敏规则。
- [ ] G03 定义 Agent Session 和状态机。
- [ ] G04 实现 Plan、Tool Call、Patch、Review、Verify。
- [ ] G05 实现代码搜索、文件读取、LSP、终端、测试工具。
- [ ] G06 实现多文件 Diff 和用户批准流程。
- [ ] G07 实现任务取消、重试、失败归因和上下文压缩。
- [ ] G08 实现会话持久化、摘要和恢复。
- [ ] G09 接入至少两个云模型 Provider。
- [ ] G10 预留本地模型 Provider，不强制 V1 交付。

### H. 安全与扩展

- [ ] H01 定义 Capability API 和权限清单。
- [ ] H02 实现 Workspace Trust / Untrusted Workspace。
- [ ] H03 实现文件、网络、终端、密钥和 Git 权限。
- [ ] H04 实现 Agent 命令审批、数据脱敏和审计。
- [ ] H05 实现 WASM 扩展生命周期和沙箱。
- [ ] H06 实现扩展崩溃隔离和自动禁用。
- [ ] H07 建立 VSCode 兼容 Tier 1：语言类和基础命令类扩展。
- [ ] H08 设计 Node 扩展兼容宿主及资源限制。
- [ ] H09 实现插件权限查看、撤销、升级和回滚。

### I. 协作和远程能力

- [ ] I01 评估 Automerge/Yjs 等 CRDT 方案。
- [ ] I02 定义远程操作、版本向量和 Presence 模型。
- [ ] I03 实现离线编辑和恢复合并。
- [ ] I04 实现协作中继和断线重连。
- [ ] I05 定义远程工作区和 Remote Agent 协议。
- [ ] I06 实现 SSH/容器开发 Spike。
- [ ] I07 建立数据驻留、租户和协作权限模型。

### J. Windows 交付与 macOS 移植

- [ ] J01 创建 Windows Setup.exe、MSI 和 Portable ZIP。
- [ ] J02 实现用户目录安装和无管理员安装。
- [ ] J03 完成文件关联、右键菜单和默认终端检测。
- [ ] J04 完成代码签名、自动更新和回滚。
- [ ] J05 完成 Windows 10/11 安装矩阵测试。
- [ ] J06 实现 macOS App Bundle、签名和 Notarization。
- [ ] J07 验证 Intel/Apple Silicon 支持策略。
- [ ] J08 完成 macOS 文件权限、Keychain、PTY 和高 DPI。

### K. QA、性能和运维

- [ ] K01 建立单元、集成、端到端和兼容性测试框架。
- [ ] K02 建立 Windows 10/11 真机或虚拟机矩阵。
- [ ] K03 建立 LSP/DAP/终端/Git 回归测试集。
- [ ] K04 建立扩展崩溃、权限拒绝和数据恢复测试。
- [ ] K05 建立性能基线：输入延迟、打开、搜索、索引、内存。
- [ ] K06 实现 Crash Reporter、Hang 检测和日志导出。
- [ ] K07 建立 CI：编译、测试、签名、打包和冒烟安装。
- [ ] K08 建立 Release Candidate、灰度和回滚流程。
- [ ] K09 建立安全扫描、依赖升级和供应链审计。

## 4. 里程碑与退出标准

### M0：架构冻结（2—4 周）

交付：PRD、ADR、模块图、接口草案、技术 Spike 计划、Windows 支持矩阵。  
退出标准：UI、编辑器内核、扩展模型、AI Tool Schema 和安装路线完成决策。

### M1：可安装编辑器（6—10 周）

交付：Windows 安装包、窗口、文件树、编辑、保存、搜索、Git 初版、终端。  
退出标准：Windows 10/11 新机器可安装；打开、编辑、保存、关闭恢复稳定。

### M2：专业编码 IDE（8—12 周）

交付：LSP、Formatter、Test、Task、DAP、Git 完整基础链路。  
退出标准：至少三种语言可完成补全、跳转、诊断和基础调试。

### M3：AI 原生版本（8—14 周）

交付：代码库上下文、Agent、工具调用、Diff Review、测试验证、多模型。  
退出标准：Agent 不能绕过权限中心；所有写入都进入 Review 和编辑事务流。

### M4：扩展与安全（10—16 周）

交付：WASM 扩展、Capability、Workspace Trust、VSCode Tier 1 兼容。  
退出标准：扩展崩溃不影响核心；未授权文件、终端、网络访问必须被拒绝。

### M5：协作与远程（12—20 周）

交付：CRDT、Presence、离线合并、远程工作区 Spike。  
退出标准：断网可编辑；恢复后无静默数据丢失；远程断线可恢复。

### M6：macOS 版本（8—14 周）

交付：macOS App、签名、公证、核心编辑、LSP、Git、终端。  
退出标准：核心功能与 Windows 版本一致，Apple Silicon 验证通过。

> 周期为中型团队的粗略估计，不是单人承诺；若团队少于 3 人，应按功能子集重新估算。

## 5. 团队与责任

| 角色 | 责任 |
|---|---|
| 产品负责人 | 用户场景、优先级、版本验收和范围控制 |
| 架构负责人 | 模块边界、ADR、接口和技术风险 |
| Rust 内核工程师 | Editor Kernel、VFS、性能和数据模型 |
| Windows/桌面工程师 | 窗口、GPU、终端、安装和平台适配 |
| UI 工程师 | 工作区、布局、主题和交互 |
| 语言工具工程师 | LSP、DAP、Formatter、Test Adapter |
| AI 工程师 | Context、Agent、Provider、Patch、评测 |
| 安全/扩展工程师 | WASM、Capability、权限、审计和兼容宿主 |
| QA/发布工程师 | 回归、性能、兼容矩阵、安装和发布 |

最小可行团队：5—7 人。  
完整产品团队：9—14 人。

## 6. 质量与验收标准

### 6.1 性能目标

- 键盘输入到画面更新：P95 < 16 ms。
- 搜索响应：常规项目 P95 < 300 ms。
- 普通文件保存：P95 < 100 ms。
- 索引过程不能阻塞编辑输入。
- 中型项目打开目标 < 5 秒，具体以基准仓库定义。
- 扩展崩溃恢复目标 < 3 秒。

### 6.2 稳定性目标

- 核心编辑器崩溃不得丢失已确认保存的数据。
- LSP、DAP、终端和扩展均可独立重启。
- 自动更新失败必须可回滚。
- 版本升级不得破坏工作区和设置。
- 所有长时间运行测试不得出现持续内存增长。

### 6.3 安全目标

- 工作区默认区分 Trusted / Untrusted。
- Agent 默认不能直接执行高风险终端命令。
- 未授权扩展不能访问文件、网络和密钥。
- API Key 不进入普通日志和模型上下文。
- 发送到云端前执行敏感信息脱敏。
- 权限拒绝、审批、Patch 和工具调用可审计。
- 安装包和更新包必须签名。

### 6.4 兼容目标

V1 首批平台：

- Windows 10 x64。
- Windows 11 x64。

后续平台：

- macOS Apple Silicon。
- macOS Intel（根据维护成本决定）。

首批语言：TypeScript、Python、Rust、Go、C# 中至少三种。  
首批终端：PowerShell、CMD、Git Bash；WSL 后续加强。  
首批扩展：语言服务、命令、配置、基础 Tree View 类扩展。

## 7. 主要风险与应对

| 风险 | 影响 | 应对策略 |
|---|---|---|
| 目标范围过大 | 项目长期无法交付 | 以 M1/M2/M3 为硬边界，协作和完整扩展后置 |
| VSCode 扩展兼容成本失控 | 延迟和稳定性下降 | 分 Tier 兼容，优先协议而不是内部 API |
| WebView 性能不足 | 编辑体验退化 | 高频编辑路径保持原生/GPU，WebView 仅作低频 UI |
| AI 误修改或执行危险命令 | 数据和安全事故 | Patch Review、Capability、审批、审计、fail-closed |
| 大型项目索引占用资源 | 输入卡顿 | 增量索引、后台调度、缓存、取消和资源限额 |
| Windows 工具链碎片化 | 终端和调试不稳定 | ConPTY/适配器隔离，建立真实环境兼容矩阵 |
| 跨平台推迟导致返工 | macOS 成本升高 | 第一版就定义 Platform Interface 和禁止平台泄漏规则 |
| 协作数据冲突 | 数据丢失 | CRDT Spike、版本向量、离线合并可解释化 |
| 单人或小团队资源不足 | 交付周期不可控 | 先做本地 IDE + LSP + Git + 基础 AI，暂停生态和协作 |

## 8. 首个两周执行清单

### 第 1 周：决策和 Spike

- [ ] 完成 V1 PRD 和非目标。
- [ ] 完成 Rust Core、UI、IPC、扩展、AI 五项技术选型。
- [ ] 完成编辑事务模型草案。
- [ ] 完成 Capability API 草案。
- [ ] 完成 Agent Tool Schema 草案。
- [ ] 选定首批三种语言和基准仓库。
- [ ] 选定 Windows 安装方案。
- [ ] 创建 ADR-001 至 ADR-005。

### 第 2 周：可安装技术基线

- [ ] 创建 Rust Workspace。
- [ ] 创建 Desktop Shell 空壳。
- [ ] 创建 Editor Kernel 空实现。
- [ ] 创建 VFS 和配置系统。
- [ ] 创建日志和错误码系统。
- [ ] 创建 Windows 安装工程。
- [ ] 创建 CI 编译、测试和打包流水线。
- [ ] 输出第一个可安装内部版本。

两周的硬验收：

```text
Windows 10/11
 -> 安装
 -> 启动窗口
 -> 打开文件
 -> 修改文件
 -> 保存文件
 -> 关闭并恢复
 -> 运行自动化测试
```

## 9. 项目完成定义

项目不能以“代码写完”作为完成标准，而应以以下条件定义完成：

- Windows 10/11 有稳定安装和升级路径。
- 核心编辑、文件、搜索、Git、终端、LSP、任务和调试可用。
- AI Agent 能够在权限和 Review 约束下完成代码修改和验证。
- 扩展不能通过崩溃或越权破坏核心。
- 关键性能、稳定性和安全指标有自动化证据。
- macOS 平台适配不需要重写编辑器内核。
- 每个版本都有明确的功能边界、回滚方式和验收报告。

## 10. 当前下一步

立即进入 M0，按以下顺序执行：

1. 选择 UI/渲染方案。
2. 冻结 Editor Kernel 文档事务模型。
3. 冻结 Windows 安装和更新方案。
4. 完成 LSP、终端、Git、AI Agent 四个 Spike。
5. 建立 Rust Workspace、CI 和第一个安装包。
6. 以 M1 为第一个产品里程碑，而不是先做插件市场或多人协作。
