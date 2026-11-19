# AGENTS.md

项目：LightC —— Tauri 2 桌面应用（Windows C 盘清理工具），单 crate Rust 后端（`src-tauri`）+ TypeScript 前端（`src`）。
本项目会**物理删除用户数据**，安全规则的优先级高于一切其他规则。

## 1. Windows 环境（每次会话开始）

Always use UTF-8.

Before executing any PowerShell command, run:

```
chcp 65001
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new()
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()
$OutputEncoding = [System.Text.UTF8Encoding]::new()
```

Never retry because of garbled console output. If output contains unreadable characters, inspect the exit code first.

网络环境备注：代理下 `github.com` 返回 502（curl 同样失败），但 `api.github.com` / `raw.githubusercontent.com` / `objects.githubusercontent.com` 可达。

---

## 2. 改动纪律（每次改代码都适用）

- 只做用户明确要求的事情，不主动扩展需求；默认先读完整项目上下文再行动
- 优先最小改动，不修改与当前任务无关的文件；**改动处本身必须达到生产级**，但不做范围外的"顺手优化"和重构
- 保持现有代码风格（命名 / 格式 / 结构），不随意引入新依赖，不做"顺手优化"
- 修改必须完整可运行：不留 TODO / 未完成逻辑，不提交半成品，涉及多处逻辑时全部同步修改
- **提交前门禁（强制，本地先全绿再提交）**：
  - `cargo fmt --all -- --check`（rustfmt.toml：edition 2021 / max_width 100 / Unix 换行）
  - `cargo clippy --all-targets` 0 error（`clippy::correctness` / `clippy::suspicious` 已在 Cargo.toml deny）
  - `cargo test --lib` 全部通过（不允许任何失败，当前基线见最近一次提交）
  - 改了依赖：`cargo deny --offline check`（`--offline` 是全局 flag，必须写在 `check` 之前；advisory-db 本地在 `C:\Users\Passacaglia\.cargo\advisory-dbs\`）
  - 动了前端：`npm run build` 类型检查通过
  - CI（`.github/workflows/ci.yml`）跑同样的门禁，本地不过别指望推上去过

---

## 3. 安全规则（本项目最高优先级）

- **「这条路径能不能删」全仓只允许一份实现**，所有删除入口必须走统一校验（基准：`scanner/leftovers.rs` 的 `is_safe_leftover_path_under`——拒绝 `.`/`..` 组件、按 component 比较而非字符串前缀、先 canonicalize 展开短名）。禁止新增任何字符串前缀匹配式的路径校验。
- 新增判断、映射、校验之前，先搜仓库有没有现成的。审计已确认过 8 份路径校验、4 套扫描取消标志、2 套盘符获取并存——同一条规则写两处，两处迟早不一致。
- 路径比较（白名单 / 保护路径）：写入侧与比较侧的规范化必须一致；8.3 短名（如 `PASSAC~1`）必须经 canonicalize 展开，否则用户白名单静默失效。
- 安全校验失败必须硬失败（`return Err`），不允许检查后只打 warn 继续执行。
- 子进程超时必须杀整棵进程树（`taskkill /T /F` 或 Job Object）：只 kill 壳进程（如 `cmd /C dism`）会让管道 reader 的 `join()` 永久挂起。
- 回收站清理 API 是"整盘清空"语义，任何涉及部分选择的逻辑都必须向用户明示实际影响范围。

---

## 4. 依赖与升级纪律

- 不随意新增依赖；能用 std / 原生能力解决的不用库；引入必须有明确收益；不再被引用的依赖随手删除
- **禁止一次性全量 `cargo update`**（锁文件约 600 个包）；依赖升级按档拆分、分批提交
- 安全公告治理先试 lockfile 补丁升级（源码零改动），再考虑大版本
- `tauri-plugin-updater` 的 Rust 版本与 npm 版本必须配对升级（IPC 虽兼容，API 有差异）

---

## 5. 测试纪律

- 测试必须自包含：不读宿主机注册表、不依赖 `%TEMP%` 的拼写、不依赖网络；宿主相关输入用 fixture 注入（参考 `runtime.rs` 的参数注入写法）
- 修 bug 前先写一个能复现它的测试，再改代码
- 不手抄长常量（公钥、密钥等）：用 `include_str!` 读配置文件。抄错一个字符会让排查方向整个错掉

---

## 6. 代码质量要求（强制）

- 生产级标准：高可读性、高内聚低耦合、模块化设计；单函数职责单一（建议 ≤ 50 行）；避免重复代码（必要时抽取复用）
- 扩展功能前先判断"这段逻辑该归谁"：公共层只能调用，不许在模块里再复制一份"看起来差不多"的实现；视图层不拼业务规则，业务规则不写进组件
- 错误处理：所有可能失败的逻辑必须处理；不允许吞异常（catch 后无处理、`join()` 结果用 `let _ =` 丢弃、锁中毒 `unwrap()` 炸全进程）；错误信息要明确、可定位；异步代码必须考虑失败分支
- 边界与健壮性：考虑空值、异常输入、极端情况；对外部数据（接口 / 文件 / 用户输入）必须校验，不假设输入正确
- 命名语义清晰，禁止无意义命名（`data` / `temp` / `value`），禁止除通用约定（id、url）外的缩写
- 性能意识（实测导向）：热路径零分配（参考 `deep_junk.rs` 的 `could_be_junk_path`）；避免重复全树遍历（扫描阶段算过的大小随路径传递，不要删之前再算一遍）；避免每文件创建子进程（Windows 上一次约 10-20ms，批量场景必须进程内调用）；单遍历 + 分类器优于多线程重复遍历
- 不保留调试输出（Rust 的 `dbg!` / `println!`、前端的 `console.log`）；必要日志结构清晰、不冗余
- **禁止过度设计**：不为"可能的未来需求"设计复杂结构，不引入多余抽象层；优先简单直接实现
- **改完自问三句**：半年后别人看得懂吗？改一个需求要动几处？删掉它会不会连带别的东西坏？

---

## 7. 注释规范（强制）

- 所有新增或修改的代码必须包含**英文注释**，不写中文注释
- 不要求回头翻译既有的中文注释；只有该处本来就要重写时才顺带改成英文
- 注释说明"为什么这么做"，而不是重复代码；关键逻辑（边界处理、特殊判断）必须解释清楚
- 不写无意义注释（如 `// increment by 1`）；`unsafe` 块必须带 `// SAFETY:` 说明

---

## 8. CHANGELOG 与文档同步（强制）

- **哪些必须写**：用户能感知的功能与修复（修好了什么、在哪里能看到、用户该怎么做）必须同步更新中英两个 CHANGELOG，逐条对应
- **哪些不写**：纯内部重构、技术名词（console / API / 函数名 / 文件路径 / 环境变量）、排查过程、性能实测值、"支持作者"相关展示调整
- 每个版本 2-4 条，能用两三句话说完就别列清单；多个同类修复合并成一条，用"其他修复"收尾
- 不允许代码与文档不一致

反例（不要这样写）：
`便携版：不再依赖额外标记文件。程序会通过安装登记判断自己是不是被安装的，即使只保留了 LightC.exe，也能正确识别为便携版；程序目录不可写时设置页会说明数据临时位置。`

正例：
`便携版不再被误当成安装版把数据写进 AppData，程序目录不可写时会明确提示。`

---

## 9. Git 与发布

- commit 粒度清晰，message 简洁说明变更内容，默认英文，不提交无关文件
- 打 tag 或准备发 Release 前，先读 `RELEASE-CHECKLIST.md` 并逐项核对（不发版则无需关注）；发布链路硬约束见 `RELEASE-TODO.md`（tag `v*` 触发；版本号取自 `CHANGELOG-zh.md` 第一个 `## vX.Y.Z` 且必须与 tag 一致；`releaseDraft: false` + `prerelease: false` → 推 tag 即正式发布）
- 本项目从未发过 Release：自动更新链路（check → download → install）第一次发版时必须做真实回归，这是唯一用户不可逆感知的功能
- 签名私钥只在 GitHub Secrets，本地只有公钥 `.tauri/lightc.key.pub`

---

## 10. 输出格式

- 所有改动必须标明文件路径、保证可直接应用；只输出必要代码片段，不重复未修改代码
- 如果涉及 README 修改，同时输出 README 变更内容

示例：

```diff
# src-tauri/src/path_guard.rs

- if !self.is_in_allowed_scope(file_path) {
-     warn!("path outside allowed scope: {}", file_path.display());
- }
+ if !self.is_in_allowed_scope(file_path) {
+     return Err(format!("path outside allowed scope: {}", file_path.display()));
+ }
```

---

## 11. 多任务并行（与审计议题隔离）

- 允许多议题并行推进，但开工前自查三问：改动面大（≥5 个文件或跨模块）？周期超过一天？与进行中的工作有文件交集或共享构建产物？**命中两条以上先隔离再动手**
- 完整流程（worktree 建立、文件所有权登记、收敛顺序、已知坑与真实案例）见 WorkBuddy 的 `parallel-workflow` skill，不在本文件重复
- 仓库特定约定：
  - 分支与 commit 用审计议题编号命名（如 `h4-bindings`、`m13-s3`，commit 标注如 `(M13, part 1 of S3)`）
  - 议题间的文件所有权边界写进对应方案文档（范本：workspace 的 H4 契约文档 §3/§10 对 M13 的边界声明）；交集文件**谁后做谁适配**，不抢
  - 共享工作区时 cargo 系命令互斥（构建锁）：短期用独立 `CARGO_TARGET_DIR` 绕开，长期并行用 `git worktree`
  - 同一文件被两个议题触碰时**不拆 hunk 提交**——整个文件归属一个议题，拆错的代价比少拆大
  - 删依赖后的验收口径是"我们的使用面归零"，不是"依赖图归零"（传递依赖不可控）
