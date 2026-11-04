# 屏幕连接助手 · ScreenConnection

外接屏的体检医生 + 工位开关：显示器亮度音量、色彩同步、布局编排、屏幕体检与一键修复。
跨平台（macOS + Windows），Tauri 2 + Rust + Vue 3，本机运行，无账号、无遥测、无云同步。

> 名称对应关系：用户看到的叫「屏幕连接助手」（窗口标题、安装名、快捷方式），
> 系统里注册的标识仍是 `com.nanyu.screenguard`（保持 macOS 辅助功能授权连续，不可改动），
> 仓库与构建产物名沿用 `screen-connection` / `ScreenConnection`（ASCII，便于 CI 与下载链接）。

## 下载

从 [GitHub Releases](https://github.com/new-screen-connection/screen-connection/releases) 获取：

| 平台 | 附件 |
|---|---|
| Windows x64 | `ScreenConnection_<版本>_x64-setup.exe`（NSIS，含 WebView2 离线运行时） |
| macOS Apple Silicon | `ScreenConnection_<版本>_aarch64.dmg` |
| macOS Intel | `ScreenConnection_<版本>_x86_64.dmg` |

每个产物附 `.sha256` 校验文件与汇总的 `SHA256SUMS.txt`。校验方法与首次运行放行步骤见
**[安装引导页](https://new-screen-connection.github.io/screen-connection/install.html)**。

## 代码签名策略（Code Signing Policy）

本项目为开源软件（MIT），发布产物由 GitHub Actions 自动构建，维护者人工审核后发布。

- **Windows**：正在接入 [SignPath Foundation](https://signpath.org) 的开源项目免费签名通道。
  签名由 SignPath Foundation 的 HSM 提供，签名后发布者显示为 SignPath Foundation
  而非个人维护者——这是免费通道的固有特征。接入前，产物为未签名版本，
  SmartScreen 可能提示「未知发布者」，选择「更多信息 → 仍要运行」即可。
  用户可通过 Release 附带的 SHA-256 校验文件验证产物来源与完整性。
- **macOS**：因不购买每年 $99 的 Apple Developer Program 而不做 Developer ID 签名与公证，
  采用 ad-hoc 签名（`signingIdentity: "-"`）。这不会让软件更危险，但系统会要求首次打开时
  手动放行；应用更新后「辅助功能」授权可能需要重新添加一次。安装引导页有完整步骤。

详细策略与操作手册见 [docs/SIGNING.md](docs/SIGNING.md)。
本项目承诺：不包含任何专有/闭源组件；付费功能如未来出现，将保持签名通道合规（整体开源）。

## 本地开发

```bash
npm install
npm run tauri:dev        # 开发调试（接真实后端）
npm run build            # 前端构建 + vue-tsc 类型检查
cd src-tauri && cargo check   # 后端检查
npm run tauri:build:win  # 打 Windows NSIS 安装包
```

- 新增后端命令需三处登记：`lib.rs` 的 `generate_handler!`、`src/types.ts` 的 CMD 表、`src/api/mock.ts` 分支。
- 跨平台：新能力需同时实现 Windows/macOS，或在 `platform/mod.rs` 给出可读存根，禁止假装成功。
- 仓库自检：`python scripts/check_repo.py`（版本一致性、品牌统一、签名配置、脚本语法）。

## 文档

- [docs/SIGNING.md](docs/SIGNING.md) —— 签名与分发策略（零成本版）
- [docs/DELIVERY_AND_RELEASE.md](docs/DELIVERY_AND_RELEASE.md) —— 发布流程与历史验收记录
- [PRIVACY.md](PRIVACY.md) —— 隐私说明：本机运行，不联网上报

## License

[MIT](LICENSE)
