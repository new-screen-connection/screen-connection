# 归档：Personalscreenguard/screenguard 的议题与 PR

> 老仓库与新组织已合并删除，此文件保留当时的议题/PR 文本作为追溯依据（不含评论）。

## [PR #12] Complete desktop basics for macOS and Windows
- 状态：closed · 创建：2026-10-21 · 作者：nanyu-lu

## Summary
- Add user-confirmed GitHub update checks and an explicit releases-page action.
- Add same-platform export/import for allowlisted preferences while preserving the local hardware recovery baseline.
- Add macOS startup baseline and restore support for the states the OS/DDC path can safely read or restore.
- Clarify platform coverage and local data handling in README and PRIVACY.md.

## Validation
- `python3 scripts/check_repo.py`
- `node --check ui/main.js`
- `git diff --check`
- `cargo check --locked --offline --target aarch64-apple-darwin`
- `SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo test --locked --offline --target aarch64-apple-darwin` (18 passed)

Windows x64 build, packaging, and installer smoke-check are delegated to the existing GitHub Actions workflow. No physical display behavior is claimed as tested.

## [PR #11] Fix portable release checksum manifests
- 状态：closed · 创建：2026-10-21 · 作者：nanyu-lu

## Summary
- Write macOS checksum entries using asset basenames so the aggregate manifest resolves correctly from `release-assets/`.
- Write the Windows sidecar as ASCII with LF line endings rather than PowerShell's platform-default newline behavior.
- Validate `SHA256SUMS.txt` in the release job before uploading assets.

## Context
During v0.4.49 release verification, the generated manifest contained `release-assets/` path prefixes for macOS assets, and the Windows sidecar used CRLF. The published release's checksum files were corrected and read back from GitHub; this PR prevents the same packaging issue on future releases.

## Verification
- `git diff --check`
- `python3 scripts/check_repo.py`
- YAML parse of `.github/workflows/release.yml`
- Local checksum-format simulation with `shasum -a 256 -c`

## [PR #10] build(windows): avoid separate VC++ Redistributable on Windows 10+
- 状态：closed · 创建：2026-10-21 · 作者：nanyu-lu

## Summary
- Enable static MSVC CRT linking for the x64 Windows target.
- Add a `dumpbin /DEPENDENTS` CI/release audit that rejects imports of `VCRUNTIME*.dll`, `MSVCP*.dll`, and `CONCRT*.dll`.
- Document the supported Windows 10+ baseline and remaining UCRT API-set imports. UCRT is a Windows component on Windows 10+, so these imports do not require a separate VC++ Redistributable for the supported target.
- Correct the WebView2 offline-installer size note; this CI artifact was about 209 MiB.

## Verification
- `git diff --check` and both changed workflow YAML files pass local checks.
- Existing Windows CI run 39529518062 built the NSIS installer, passed the PE import audit, passed the offline WebView2 size check, and passed install/start/uninstall smoke.
- Windows PE output contained `api-ms-win-crt-*` UCRT API-set imports but no VCRUNTIME/MSVCP/CONCRT imports.
- Updated PR CI run 39531143416 is queued.

## Scope
This removes a separate VC++ Redistributable requirement for the supported Windows 10+ MSVC target. It does not claim physical Windows device or display-hardware acceptance.

## [PR #9] docs: update v0.4.48 release status
- 状态：closed · 创建：2026-10-21 · 作者：nanyu-lu

## Summary
- Update the current delivery snapshot to reflect merged PR #2/#8 and the published v0.4.48 release.
- Replace stale CI run IDs and package hashes with the public release evidence.
- Keep the historical release notes and hardware acceptance boundaries intact.

## Verification
- 
- Live GitHub release, Actions run, and open-PR checks

No application source or release artifact changed.

## [PR #8] Generate standard Windows ICC profiles in Screenguard
- 状态：closed · 创建：2026-10-20 · 作者：nanyu-lu

## Summary
- Generate standard RGB reference ICC profiles inside Screenguard for Windows targets, so users do not need to find external ICC files.
- Prefer user-provided monitor profiles and Windows-installed profiles; keep monitor-specific calibration profiles available as overrides.
- Disable the Windows grayscale target for RGB monitors and explain that generated reference profiles do not perform hardware calibration.
- Escape apostrophes in PowerShell literals used for bridge paths and ICC profile paths so Windows account/profile names such as `O'Brien` do not break ICC operations.
- Add a Windows hosted-runner smoke that installs the NSIS package, launches Screenguard and confirms its main window exists, then uninstalls it. Keep the bundled offline WebView2 size gate.
- Update the release candidate to version 0.4.48 and refresh delivery documentation.

## Verification
- Current PR head: `6388d877ecbd229dfb03f82eb8fa36290038b23f`; product implementation: `d71709e`; Windows PowerShell quoting fix: `5520164`.
- Latest PR CI run [39454897224](https://github.com/Personalscreenguard/screenguard/actions/runs/39454897224) passed all six jobs: repository checks, macOS and Windows cargo checks/tests, Windows NSIS build and install-launch-uninstall smoke, Apple Silicon DMG, and Intel DMG.
- The Windows runner smoke confirmed the installed app created its main window; the offline WebView2 size gate and silent uninstall also passed. This is hosted-runner evidence, not real display hardware acceptance.
- GitHub artifacts from this run: Apple Silicon archive `sha256:996a4427bb99aa4f4cfbf6e2fe86c66d0215bd6fff28c5fa7268bb17eb9a3324`; Intel archive `sha256:80761ffd7952adcbfb8623c1b5725ec3c812e853f755601910b99fe5b502676c`; Windows archive `sha256:bf1faec5de44d932ed5c747a11a2678af02de7c4a91bfbf0f8301168e5cdca99` (219,315,198 bytes). These are GitHub archive digests; they are not hashes of the inner DMG/EXE.
- Release workflow configuration run `39454897257` passed; tag-only packaging is skipped on pull-request events.
- Local validation: `python3 scripts/check_repo.py`, `node --check ui/main.js`, `git diff --check`, and locked macOS Rust tests passed. The earlier application-code CI run `39448238608` explicitly passed the PowerShell apostrophe regression test (Windows 12/12; macOS 18/18).

## Acceptance boundary
Generated ICC profiles encode published reference colorimetry and transfer curves; they are not measured calibration for a particular display. Hosted-runner startup smoke checks do not prove real display color acceptance. PR #8 remains OPEN/DRAFT; review/merge, actual Mac hotkey/display acceptance, Windows user environment acceptance, and release publication remain outstanding.

## [PR #7] Deploy Pages artifact without setup action
- 状态：closed · 创建：2026-10-20 · 作者：nanyu-lu

## Summary
- Omit `actions/configure-pages` for the plain static HTML artifact.
- Keep deployment on GitHub Pages through `upload-pages-artifact` and `deploy-pages`.

The Pages site is already configured to use the GitHub Actions publishing source.

## [PR #6] Host Screenguard website on GitHub Pages
- 状态：closed · 创建：2026-10-20 · 作者：nanyu-lu

## Summary
- Add the Screenguard static product website to the repository under `site/`.
- Deploy the site from GitHub Actions to GitHub Pages on updates to `main`.
- Set the canonical URL to `https://personalscreenguard.github.io/screenguard/`.

The existing site remains available until the Pages deployment is verified.

## [PR #5] Read release metadata as UTF-8 on Windows
- 状态：closed · 创建：2026-10-19 · 作者：nanyu-lu

The Windows release runner uses cp1252 as its default text encoding. Read package.json and tauri.conf.json explicitly as UTF-8 so the Chinese metadata does not fail the release version check.

## [PR #4] Fix Windows release version check shell
- 状态：closed · 创建：2026-10-19 · 作者：nanyu-lu

The release workflow uses a Bash here-document to verify the tag version. Windows runners default to PowerShell, so this step failed before packaging. Select Bash for the shared verification script so Windows and macOS run it consistently.

## [PR #3] Package Screenguard as a single-app Mac and Windows product
- 状态：closed · 创建：2026-10-19 · 作者：nanyu-lu

## Goal
Ship Screenguard for macOS and Windows without requiring users to install a separate display-control app.

## Implementation
- macOS DDC/CI, ColorSync and display controls use Screenguard code or macOS APIs. The MIT-licensed `displayplacer` layout helper is fetched at build time with architecture-specific SHA-256 pins and bundled in the app. macOS HDR control is disabled and labeled unsupported; PIP, streaming, virtual displays and configuration protection have no reliable in-app substitute and their controls are removed. Windows retains native HDR controls, with UI reporting when a driver or vendor service prevents changes.
- Windows uses native display APIs and embeds the WebView2 Evergreen offline installer in its NSIS package.
- Optional Android display control obtains Platform-Tools from inside Screenguard when the user chooses that feature; it does not block ordinary display control.
- Repository checks guard the bundled helper, both architecture hashes, bundle resource configuration, offline WebView2 mode and the absence of BetterDisplay runtime references.
- Linux and the abandoned Xiaomi monitor power-link behavior are out of scope.

## Verification
- Current PR head: `0d5568ecce859a242fcd5b294cc3e3cb1c68c60d`; this final commit updates delivery evidence. App changes are at `a893bc914c0c5fb38efe8ed41f2ed9bc40d6e09c`.
- App CI [39282943351](https://github.com/Personalscreenguard/screenguard/actions/runs/39282943351) concluded success. Its repository checks, Windows/macOS Rust checks and tests, Windows C# bridge, Windows NSIS build, embedded offline WebView2 check, silent install/uninstall smoke, and both Mac DMG build steps succeeded. Draft workflow [39282943344](https://github.com/Personalscreenguard/screenguard/actions/runs/39282943344) succeeded. The current PR-head CI [39286600648](https://github.com/Personalscreenguard/screenguard/actions/runs/39286600648) and Draft workflow [39286600610](https://github.com/Personalscreenguard/screenguard/actions/runs/39286600610) both concluded success.
- Current PR-head artifact metadata from [39286600648](https://github.com/Personalscreenguard/screenguard/actions/runs/39286600648): Windows NSIS archive 219,306,826 bytes, digest `sha256:41cbeaa72cbca4b75bd0016719e54eebb7070763d97cb03c7c53c13cc4d44b71`; Apple Silicon DMG archive 4,003,534 bytes, digest `sha256:8eb593e0ed1e21c79cdcfc043da7ebe174440c14861cff8e6b12925472d8ced5`; Intel DMG archive 4,250,701 bytes, digest `sha256:18dc3c4d8ffd633d99c093675835aefe3ae626dca7b74bc147d467ae325a71e3`. The Windows artifact was downloaded and its installer verified as a 219,260,824-byte Nullsoft PE installer; SHA-256 `12c9539f75c466e71328dcefd8cf1b6cb40c9f2c359ca707d1b5cf989a32a180`.
- Both Mac CI DMGs were downloaded and passed `hdiutil verify`: Apple Silicon SHA-256 `ed3b7500ada0ffac5df87865d7bff90a812e6ee5ac8dd678094535d6ae4cab8d`; Intel SHA-256 `68ccbd610c96bc15a0ba387f75e710b3ebbad78db99924caebb2e974dc9c3c9f`.
- Local Apple Silicon v0.4.47 DMG passed `hdiutil verify` (SHA-256 `35013b9fb507b8e3f3dec68e67ce83d415529f951b18d385d8cdb6945563c25f`). App `codesign --verify --deep --strict` passed and its bundled `displayplacer` is arm64 with the pinned SHA-256. The app is ad-hoc signed, has no Team ID, and `spctl` rejects it.
- `python3 scripts/check_repo.py` and `git diff --check` passed for the updated delivery documentation.

## Remaining release evidence
- Distribution decision: desktop installers are free downloads from the public GitHub Release. No paid Apple Developer ID or Windows Authenticode certificates and no Apple notarization are used; iPhone remains a PWA. Unsigned installers can show macOS security or Windows SmartScreen warnings on first launch.
- Real Windows installation/feature acceptance and macOS external-monitor DDC/CI/ColorSync acceptance remain outstanding. CI success does not equal hardware acceptance.

## [PR #2] Add macOS global hotkeys
- 状态：closed · 创建：2026-10-19 · 作者：nanyu-lu

## Summary
- Register Control+Option+L for the existing all-screen sleep/wake toggle on macOS.
- Register Control+Option+R to restore a previously spanned window.
- Show the correct macOS key combinations in the main window and panel; report registration errors clearly.
- Use the macOS Carbon framework, with no new third-party runtime dependency. Keep Linux explicitly unsupported.

## Verification
- Rebased onto current `main` to resolve the previous stale-base conflict. Current head: `dfa5a5d21ab004fe8aee262183c646e1b1debdf9`.
- Local Apple Silicon `cargo check` and linked `cargo build` passed; Rust library tests passed 15/15.
- Repository check, `node --check ui/main.js`, and `git diff --check` passed.
- GitHub CI run [39452121590](https://github.com/Personalscreenguard/screenguard/actions/runs/39452121590) passed all six jobs: Apple Silicon and Intel DMG builds, Windows NSIS build with offline WebView2 size gate and silent install/uninstall smoke, macOS and Windows cargo checks/tests, and repository checks.
- GitHub artifact archive digests: Apple Silicon `sha256:f0abaebe10876d558d19db660bfb50de20c567771093f7eadc02d026804803d7`; Intel `sha256:ddc6d47ed2b9b705464611b5284f77ac0bb3f878dd22067ba0e942be240cfbb0`; Windows `sha256:bf660e357405e7651ad3d8f041d6d1900bcdb78e6660ff819933e496b92f2279`.
- No display standby/wake command was executed and the shortcut was not physically pressed. Runtime hotkey acceptance remains outstanding; CI artifacts are not a substitute for Mac runtime acceptance.

## [PR #1] Screenguard v0.4.45 macOS restore fix and delivery gates
- 状态：closed · 创建：2026-10-16 · 作者：nanyu-lu

## Summary

- Keep macOS power-link opt-in and preserve native DDC VCP 0xD6, Quartz single-display sleep/wake, and display connect/disconnect monitoring.
- Show saved power-link state, per-display Quartz sleep state, and per-display DDC VCP 0xD6 readings/errors without sending power commands from the diagnostic.
- Fix `restore_foreground`: macOS only restores display modes when this span operation actually aligned them; Windows still restores the app window and its separate display-mode backup.
- Keep Windows NSIS install/uninstall smoke coverage and tag-only Developer ID signing, notarization, stapling, and Gatekeeper gates.

## Verification

- Source version 0.4.45 is aligned across Cargo, Tauri, npm, and lockfiles.
- Local `cargo check --offline` with MacOSX26.5 SDK, `cargo test --locked --lib` (22/22), `python3 scripts/check_repo.py`, `node --check ui/main.js`, and `git diff --check` passed.
- Local Apple Silicon app and DMG build succeeded. The app was ad-hoc signed and passed strict `codesign` verification; it has no Team ID and is not notarized. The custom DMG passed `hdiutil verify`; SHA-256: `91b86a5b5cc50321b6d0bb701d33cbc482ea42338036ce7c48ec8635a703104f`.
- `/Applications/Screenguard.app` is running 0.4.45; installed executable SHA-256: `58f073a79b5cbae286cd368db85e3bae3830e36b5775c6f8cb924651fb8ebfcd`. Previous 0.4.44 app was backed up. The power-link preference remains off.
- CI run `39036637915` for head `e078e125b4fbc4e200fcbb9367c8a30aaa2529e1` passed all jobs: three-platform Rust check/tests, repo checks, macOS DMG, Windows NSIS build, and Windows silent install/uninstall smoke. Draft workflow run `39036637745` passed.
- Current-head Mac artifact `12102169201` has GitHub digest `b0fba11d9f7e48ff246888fca748aedd60fc73d4281c2c8ca8ab79fa0cf2599f`; downloaded DMG SHA-256 `4f4d1fde8e2b91170c9f273b22d8c67d58b9708cfedcfb59e9a63dd88a8b911f`, `hdiutil verify` VALID. Windows artifact `12102531569` has GitHub digest `79f2a8493baada37aaacab8972655b8936a58390fed8b049f5dc52157b2eb3e7`; downloaded NSIS EXE SHA-256 `a6ba13f2d43ea68540b847e3d94b0e9a0e47459d7104974f76ff823d5af1f7b8`. Both current-head installers and SHA-256 sidecars are on the Desktop.

## Acceptance still required

- On the connected two-display Mac, enable the opt-in power-link switch and test main-screen power off/on; verify selected-window spanning and restore.
- Run Windows installation/runtime/hardware acceptance. The newer Windows source mentioned by the user is not on the visible remote branches yet.
- Configure Apple Developer ID signing and notarization credentials before a tag build. No merge, tag, or public release has occurred.

This PR remains open and draft.
