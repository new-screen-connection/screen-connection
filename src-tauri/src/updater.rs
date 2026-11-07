//! 应用内更新（v0.4.1）：GitHub Releases 检查 + 应用内下载 + 一键安装。
//! 用户要求：不再跳转 GitHub 网页（有使用门槛），软件内直接检测和更新。
//! 检测/下载用 ureq（同步阻塞，与排障助手同一依赖，不引 tokio 全家桶）。

use serde::Serialize;

const RELEASE_API: &str = "https://api.github.com/repos/new-screen-connection/screen-connection/releases/latest";

#[derive(Serialize, Clone)]
pub struct UpdateInfo {
    /// 当前安装的版本（不带 v 前缀）
    pub current: String,
    /// GitHub 最新版本（不带 v 前缀）
    pub latest: String,
    pub has_update: bool,
    /// Release 说明（截断到 500 字符，原样展示）
    pub notes: String,
    /// 安装包资源名（如 xxx_x64-setup.exe）
    pub asset_name: String,
    /// 下载直链
    pub asset_url: String,
    /// 安装包字节数（前端显示进度用）
    pub asset_size: u64,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        // 不设整体超时：211MiB 下载在慢网络下会超过任何固定时限，
        // 只限制连接握手，传输靠 TCP 自身的保活/超时兜底
        .user_agent("screen-connection-updater")
        .build()
}

/// "v0.4.1" / "0.4.1-beta" -> [0, 4, 1]
fn version_triplets(v: &str) -> Vec<u64> {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or("0");
    core.split('.').map(|s| s.trim().parse().unwrap_or(0)).collect()
}

fn newer_than(latest: &str, current: &str) -> bool {
    let b = version_triplets(latest);
    let a = version_triplets(current);
    if a.len() < 3 || b.len() < 3 {
        return false;
    }
    (b[0], b[1], b[2]) > (a[0], a[1], a[2])
}

/// 查询 GitHub Latest Release，挑出 Windows x64 NSIS 安装包（*_x64-setup.exe）
pub fn update_check_impl(current: String) -> Result<UpdateInfo, String> {
    let body = agent()
        .get(RELEASE_API)
        .call()
        .map_err(|e| format!("访问 GitHub 失败：{}", e))?
        .into_string()
        .map_err(|e| format!("读取响应失败：{}", e))?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("解析响应失败：{}", e))?;

    let latest = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    if latest.is_empty() {
        return Err("GitHub 响应里没有版本号".to_string());
    }

    // 优先 x64-setup.exe（NSIS）；没有退回 msi；都没有就报错
    let mut picked: Option<(&str, &str, u64)> = None;
    if let Some(assets) = json.get("assets").and_then(|a| a.as_array()) {
        for a in assets {
            let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let url = a
                .get("browser_download_url")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let size = a.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            if name.is_empty() || url.is_empty() {
                continue;
            }
            if (name.ends_with("_x64-setup.exe") || name.ends_with("-x64-setup.exe"))
                && picked.is_none()
            {
                picked = Some((name, url, size));
            }
        }
    }
    let (asset_name, asset_url, asset_size) = picked
        .map(|(n, u, s)| (n.to_string(), u.to_string(), s))
        .ok_or_else(|| "最新版本没有 Windows 安装包（x64-setup.exe）".to_string())?;

    let notes = json
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(500)
        .collect::<String>();

    Ok(UpdateInfo {
        has_update: newer_than(&latest, &current),
        current,
        latest,
        notes,
        asset_name,
        asset_url,
        asset_size,
    })
}

/// 流式下载安装包到 %TEMP%，每 2MiB 发一次 update-progress 事件。
/// 返回落盘路径（update_install 用）。
pub fn update_download_impl(
    app: tauri::AppHandle,
    url: String,
    total: u64,
) -> Result<String, String> {
    use std::io::{Read, Write};
    use tauri::Emitter;

    let dst = std::env::temp_dir().join("screen-connection-setup.exe");
    let resp = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("下载失败：{}", e))?;

    let mut reader = resp.into_reader();
    let mut f = std::fs::File::create(&dst).map_err(|e| format!("无法写入临时目录：{}", e))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let mut next_emit: u64 = 2 * 1024 * 1024;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("下载中断：{}", e))?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n])
            .map_err(|e| format!("写入失败：{}", e))?;
        downloaded += n as u64;
        if downloaded >= next_emit {
            next_emit += 2 * 1024 * 1024;
            let _ = app.emit(
                "update-progress",
                serde_json::json!({ "downloaded": downloaded, "total": total.max(downloaded) }),
            );
        }
    }
    let _ = app.emit(
        "update-progress",
        serde_json::json!({ "downloaded": downloaded, "total": total.max(downloaded) }),
    );
    Ok(dst.to_string_lossy().to_string())
}

/// 启动 NSIS 安装包（GUI 程序，不受 CREATE_NO_WINDOW 影响）。
/// 调用方（lib.rs）负责随后退出本应用，避免文件占用。
pub fn update_install_impl(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Err("安装包不存在，请重新下载".to_string());
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new(p)
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| format!("启动安装程序失败：{}", e))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = path;
        Err("macOS 请到 GitHub Releases 下载 DMG 手动安装".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(newer_than("0.4.1", "0.4.0"));
        assert!(newer_than("v0.5.0", "0.4.9"));
        assert!(newer_than("1.0.0", "0.9.9"));
        assert!(!newer_than("0.4.0", "0.4.0"));
        assert!(!newer_than("0.3.9", "0.4.0"));
        assert!(!newer_than("0.4.0", "0.4.1"));
        assert!(!newer_than("", "0.4.0"));
        assert!(!newer_than("abc", "0.4.0"));
        assert!(newer_than("0.4.1-beta.2", "0.4.0"));
    }
}
