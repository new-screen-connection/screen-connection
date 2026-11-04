# 签名与分发策略（零成本版）

> 本文是当下的操作手册，不是愿望清单。原则：预算为 0 元，能做多少做多少，
> 做不到的部分向用户解释清楚，不写任何「建议购买 X 证书」的引导。

## 一张表说清现状

| 平台 | 零成本能做到 | 零成本做不到 | 用户会看到什么 |
|---|---|---|---|
| Windows | 通过 SignPath Foundation 获得真实可用的 Authenticode 云签名（免费，面向开源项目），SmartScreen 不再显示「未知发布者」 | 用个人/公司名义签发证书——发布者会显示 SignPath Foundation，这是免费通道的固有特征 | 未签名版本：SmartScreen 提示「更多信息 → 仍要运行」；已签名版本：正常安装 |
| macOS | ad-hoc 签名（已配置，`signingIdentity: "-"`），避免 Apple Silicon 上「应用已损坏，无法打开」这个最糟的提示 | 完全消除 Gatekeeper 警告。Apple Developer ID + 公证必须付费，没有免费替代 | 双击打开提示「未验证的开发者」，右键打开即可运行 |

## Windows：SignPath Foundation 免费签名通道

### 申请条件逐条对照

| 条件 | 本项目现状 |
|---|---|
| OSI 认可的开源许可证 | ✓ MIT（LICENSE） |
| 项目已公开发布 | ✓ GitHub Releases 已有产物 |
| 活跃维护 | ✓ 持续迭代中 |
| 无专有 / 闭源组件 | ✓ 全部代码与随包组件开源 |
| 团队成员开启 MFA | ☐ 需人工在 SignPath 后台确认并在此打勾 |

### 重要警告

**SignPath 要求项目不含任何专有 / 闭源组件。** 如果将来把付费功能做成闭源版本，
就不能再使用这条免费通道——届时要么整体开源，要么停用签名回到未签名分发。
当前完全符合条件，先用。

### 接入步骤（人工审核制，无法全自动）

1. 在 SignPath Foundation 官网提交开源项目申请，等待人工审核。
2. 审核通过后，在仓库 `Settings → Secrets and variables → Actions` 配置：
   - Variable `SIGNPATH_ENABLED` = `true`
   - Secret `SIGNPATH_API_TOKEN`
3. 替换 `.github/workflows/release.yml` 中 `sign-windows-installer` 任务里的三个 TODO 值：
   organization id、project slug、artifact configuration id。
4. **每次发布需在 SignPath 后台人工批准签名请求**——这是该通道的设计，不是缺陷。
5. 未配置上述变量时，release 工作流照常产出未签名安装包，日志打印
   `[sign] 未配置代码签名，本次 Windows 产物为未签名版本`。

代码侧注意：**不要**在 `tauri.conf.json` 的 `bundle.windows` 里配置 `certificateThumbprint`。
SignPath 是对 CI 产出的 exe 做后置云签名（私钥在其 HSM），不读本地证书，配了只会误导。

## macOS：ad-hoc 签名（已落地）

配置在 `src-tauri/tauri.conf.json` 的 `bundle.macOS`：

- `signingIdentity: "-"` —— ad-hoc 签名。CI 如需换真证书，设置环境变量
  `APPLE_SIGNING_IDENTITY` 即可覆盖，无需改配置文件。
- `entitlements: "entitlements.plist"` —— 已声明 `com.apple.security.automation.apple-events`
  （窗口跨屏、电源联动走 AppleScript 需要）。现在用不上付费证书，先落地，将来换真证书不用重做。
- 不打 .pkg 包（未签名 pkg 的放行步骤比 dmg 更麻烦），继续 dmg/zip。
- hardened runtime：由打包器在签名时处理，无需单独配置。

### 已知的体验代价（不是 bug）

ad-hoc 签名的 designated requirement 不稳定，**每次版本更新后**，系统
「隐私与安全性 → 辅助功能」里的旧授权可能失效，用户需要删掉旧条目重新添加。
应用已做三件事缓解：

1. 启动时检测辅助功能可用性，不可用且此前授权过时弹出说明卡，一键跳转系统设置面板；
2. 更新后的首次启动同样展示提示，不静默失败；
3. 安装引导页（install.html）写明这是未公证应用更新后的正常现象。

## 用户侧引导

- 安装引导页：`site/install.html`（README 顶部与 Release 说明均有链接），
  含 macOS 三种放行方式、Windows 解除锁定命令、SHA-256 校验方法。
- 校验：所有 Release 产物附 `.sha256` 与汇总的 `SHA256SUMS.txt`，
  macOS 用 `shasum -a 256 -c SHA256SUMS.txt`，Windows 用 `Get-FileHash -Algorithm SHA256`。

## CI 约定

- 构建流水线在不配置任何密钥时也能完整产出安装包，只在日志里标注「未签名 / ad-hoc」。
- release workflow 中签名相关 secret / vars 缺失时走「跳过 + 明确警告」分支，构建不失败。
- 环境变量清单：`SIGNPATH_ENABLED`（vars）、`SIGNPATH_API_TOKEN`（secret，接入后才有）、
  `APPLE_SIGNING_IDENTITY`（可选，覆盖 ad-hoc）。文档与 CI 读取的名字保持一致。

## 明确不做的事

- 不购买任何证书；不注册 Apple Developer Program；不开通任何云签名付费服务。
- 不打 .pkg；不做 Homebrew Cask（2026 年 9 月起 Homebrew 不再接受无法通过
  Gatekeeper 检查的 cask，未公证 app 这条路已关闭，若将来付费公证了再考虑）。
- 不引入自动更新框架（Sparkle / tauri-plugin-updater），避免与未签名状态冲突。
- 不改 identifier（会打断 macOS 辅助功能授权连续性）。
