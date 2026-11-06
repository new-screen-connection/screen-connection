//! 配置跨设备迁移（P1-3）：口令加密导出 / 预览 / 导入。
//! 不做账号、不走云：载荷只经用户自选的通道（文本/二维码/文件）在设备间传递。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes256Gcm;
use argon2::Argon2;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::platform::{load_profiles, save_profiles, Profile};

/// 外层载荷前缀：SCMIG<schema>:
pub const MIGRATION_PREFIX: &str = "SCMIG1:";
/// 支持的最高 schema（高于它 → 提示升级）
pub const SUPPORTED_SCHEMA: u32 = 1;
pub const PASSWORD_MIN: usize = 6;
pub const PASSWORD_MAX: usize = 32;
/// 解密失败的统一提示（不区分具体原因，避免泄露信息）
pub const DECRYPT_FAIL_MSG: &str = "口令不正确或文件已损坏";

// Argon2id 参数（OWASP 推荐：19 MiB / t=2 / p=1），几毫秒到几十毫秒级，不拖 UI
const ARGON_M_KIB: u32 = 19_456;
const ARGON_T: u32 = 2;
const ARGON_P: u32 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MigratePrefs {
    /// 退出时自动还原软件层改动
    #[serde(default = "default_true")]
    pub revert_on_exit: bool,
}

impl Default for MigratePrefs {
    fn default() -> Self {
        MigratePrefs { revert_on_exit: true }
    }
}

/// 自包含迁移包（解密后的明文结构）
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MigrateBundle {
    pub schema: u32,
    pub exported_at: String,
    pub app_version: String,
    pub platform: String,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub prefs: MigratePrefs,
}

/// 导入前预览：让用户确认哪些新增、哪些同名覆盖、哪些档基于其他显示器
#[derive(Serialize, Clone, Debug)]
pub struct MigratePreview {
    pub schema: u32,
    pub exported_at: String,
    pub app_version: String,
    pub platform: String,
    /// 将新增的档
    pub new_profiles: Vec<Profile>,
    /// 将按同名覆盖的档（展示导入方的内容）
    pub overwrite_profiles: Vec<Profile>,
    /// 这些档内的显示器 EDID 与本机不匹配，应用时可能部分项不生效（不阻止导入）
    pub mismatched: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct MigrateImportReport {
    pub added: usize,
    pub overwritten: usize,
    pub added_names: Vec<String>,
    pub overwritten_names: Vec<String>,
    pub mismatched: Vec<String>,
    /// 偏好是否已合并（退出还原开关）
    pub prefs_applied: bool,
}

fn default_true() -> bool {
    true
}

// ===== 载荷编解码 =====

/// Argon2id 派生 32 字节密钥
fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let params = argon2::Params::new(ARGON_M_KIB, ARGON_T, ARGON_P, Some(32))
        .map_err(|_| "内部错误：Argon2 参数".to_string())?;
    let a2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; 32];
    a2.hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|_| "内部错误：密钥派生失败".to_string())?;
    Ok(key)
}

/// 加密：salt(16) || nonce(12) || AES-256-GCM(ct)，整体 base64，加前缀
fn encrypt_payload(bundle_json: &str, password: &str) -> Result<String, String> {
    let mut salt = [0u8; SALT_LEN];
    getrandom::getrandom(&mut salt).map_err(|e| format!("随机数生成失败：{}", e))?;
    let key = derive_key(password, &salt)?;
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|e| format!("随机数生成失败：{}", e))?;
    let cipher = Aes256Gcm::new(aes_gcm::Key::<Aes256Gcm>::from_slice(&key));
    let ct = cipher
        .encrypt(nonce.as_ref().into(), bundle_json.as_bytes())
        .map_err(|_| "内部错误：加密失败".to_string())?;
    let mut blob = Vec::with_capacity(SALT_LEN + NONCE_LEN + ct.len());
    blob.extend_from_slice(&salt);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    Ok(format!("{}{}", MIGRATION_PREFIX, B64.encode(&blob)))
}

enum DecryptOutcome {
    Ok(String),
    /// 统一失败：口令不对 / 载荷损坏
    Fail(String),
    /// 格式识别到了但 schema 更新
    NeedUpgrade,
}

fn decrypt_payload(payload: &str, password: &str) -> DecryptOutcome {
    let t = payload.trim();
    let Some(rest) = t.strip_prefix("SCMIG") else {
        return DecryptOutcome::Fail(DECRYPT_FAIL_MSG.to_string());
    };
    let Some((ver, b64)) = rest.split_once(':') else {
        return DecryptOutcome::Fail(DECRYPT_FAIL_MSG.to_string());
    };
    let Ok(ver) = ver.parse::<u32>() else {
        return DecryptOutcome::Fail(DECRYPT_FAIL_MSG.to_string());
    };
    if ver > SUPPORTED_SCHEMA {
        return DecryptOutcome::NeedUpgrade;
    }
    let Ok(blob) = B64.decode(b64.trim()) else {
        return DecryptOutcome::Fail(DECRYPT_FAIL_MSG.to_string());
    };
    if blob.len() < SALT_LEN + NONCE_LEN + 16 {
        return DecryptOutcome::Fail(DECRYPT_FAIL_MSG.to_string());
    }
    let (salt, rest2) = blob.split_at(SALT_LEN);
    let (nonce, ct) = rest2.split_at(NONCE_LEN);
    let key = match derive_key(password, salt) {
        Ok(k) => k,
        Err(_) => return DecryptOutcome::Fail(DECRYPT_FAIL_MSG.to_string()),
    };
    let cipher = Aes256Gcm::new(aes_gcm::Key::<Aes256Gcm>::from_slice(&key));
    match cipher.decrypt(nonce.into(), ct) {
        Ok(pt) => match String::from_utf8(pt) {
            Ok(s) => DecryptOutcome::Ok(s),
            Err(_) => DecryptOutcome::Fail(DECRYPT_FAIL_MSG.to_string()),
        },
        Err(_) => DecryptOutcome::Fail(DECRYPT_FAIL_MSG.to_string()),
    }
}

// ===== 脱敏 =====

/// 剥掉设备唯一标识：captured_id（Windows 是设备接口路径）、main_display（本机 display id）、
/// audio_device_id（音频端点实例路径）。保留 edid_key（厂商码+产品码，型号标识，供适配匹配）。
/// 解密后的明文里不允许搜到序列号 / 机器名。
pub fn sanitize_bundle(bundle: &mut MigrateBundle) {
    for p in &mut bundle.profiles {
        p.targets.main_display = None;
        p.targets.audio_device_id = None;
        for d in &mut p.targets.displays {
            d.captured_id = None;
        }
    }
}

// ===== 对外实现 =====

/// 导出：全部（或指定 id 的）场景档 + 通用偏好，脱敏后加密为 base64 载荷
pub fn migrate_export_impl(profile_ids: Option<&[String]>, password: &str) -> Result<String, String> {
    let pw = password.trim();
    if pw.len() < PASSWORD_MIN || pw.len() > PASSWORD_MAX {
        return Err(format!("口令需要 {}–{} 位字符", PASSWORD_MIN, PASSWORD_MAX));
    }
    let all = load_profiles();
    let profiles = match profile_ids {
        Some(ids) if !ids.is_empty() => all.into_iter().filter(|p| ids.iter().any(|i| i == &p.id)).collect(),
        _ => all,
    };
    if profiles.is_empty() {
        return Err("本机还没有场景档可导出".to_string());
    }
    let prefs = MigratePrefs { revert_on_exit: crate::platform::pref_revert_on_exit() };
    let mut bundle = MigrateBundle {
        schema: SUPPORTED_SCHEMA,
        exported_at: crate::platform::now_str(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: platform_name().to_string(),
        profiles,
        prefs,
    };
    sanitize_bundle(&mut bundle);
    let json = serde_json::to_string(&bundle).map_err(|e| format!("序列化失败：{}", e))?;
    encrypt_payload(&json, pw)
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    }
}

/// 解析并解密载荷为 bundle（导出/导入共用）
fn parse_bundle(payload: &str, password: &str) -> Result<MigrateBundle, String> {
    match decrypt_payload(payload, password) {
        DecryptOutcome::Ok(json) => {
            let b: MigrateBundle = serde_json::from_str(&json)
                .map_err(|_| DECRYPT_FAIL_MSG.to_string())?;
            if b.schema > SUPPORTED_SCHEMA {
                return Err("该配置来自更新版本，请先升级".to_string());
            }
            Ok(b)
        }
        DecryptOutcome::Fail(m) => Err(m),
        DecryptOutcome::NeedUpgrade => Err("该配置来自更新版本，请先升级".to_string()),
    }
}

/// 本机 EDID 键集合。macOS 捕获时不记录 EDID（走顺序兜底），无法判定适配 → 返回 None 跳过提示。
fn local_edid_keys() -> Option<Vec<String>> {
    #[cfg(target_os = "windows")]
    {
        let m = crate::platform::edid_keys();
        Some(m.values().cloned().collect())
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// 档内显示器是否全部与本机不匹配（true = 「基于其他显示器创建」）
fn profile_all_mismatched(p: &Profile, keys: &[String]) -> bool {
    let with_edid: Vec<_> = p.targets.displays.iter().filter_map(|d| d.edid_key.as_deref()).collect();
    if with_edid.is_empty() {
        return false; // 没有 EDID 信息就别乱警告
    }
    with_edid.iter().all(|k| !keys.iter().any(|lk| lk == k))
}

/// 导入前预览：解密 + 与本机对比（同名覆盖 / 其余新增 / 硬件适配提示）。不落盘。
pub fn migrate_preview_impl(payload: &str, password: &str) -> Result<MigratePreview, String> {
    let bundle = parse_bundle(payload, password)?;
    let local = load_profiles();
    let mut new_profiles = Vec::new();
    let mut overwrite_profiles = Vec::new();
    for p in &bundle.profiles {
        if local.iter().any(|lp| lp.name == p.name) {
            overwrite_profiles.push(p.clone());
        } else {
            new_profiles.push(p.clone());
        }
    }
    let mismatched = match local_edid_keys() {
        Some(keys) => bundle
            .profiles
            .iter()
            .filter(|p| profile_all_mismatched(p, &keys))
            .map(|p| p.name.clone())
            .collect(),
        None => Vec::new(),
    };
    Ok(MigratePreview {
        schema: bundle.schema,
        exported_at: bundle.exported_at,
        app_version: bundle.app_version,
        platform: bundle.platform,
        new_profiles,
        overwrite_profiles,
        mismatched,
    })
}

/// 导入落盘：同名覆盖（保留本地 id 与创建时间，替换内容），其余新增（新 id 防撞车）。
/// 另合并「退出还原」偏好。不做自动冲突合并——只覆盖与新增，用户已在预览确认。
pub fn migrate_import_impl(payload: &str, password: &str) -> Result<MigrateImportReport, String> {
    let bundle = parse_bundle(payload, password)?;
    let keys = local_edid_keys();
    let mut list = load_profiles();
    let mut report = MigrateImportReport {
        added: 0,
        overwritten: 0,
        added_names: Vec::new(),
        overwritten_names: Vec::new(),
        mismatched: Vec::new(),
        prefs_applied: false,
    };
    for p in &bundle.profiles {
        if let Some(idx) = list.iter().position(|lp| lp.name == p.name) {
            let local_id = list[idx].id.clone();
            let local_created = list[idx].created_at.clone();
            list[idx] = p.clone();
            list[idx].id = local_id;
            list[idx].created_at = local_created;
            list[idx].updated_at = crate::platform::now_str();
            report.overwritten += 1;
            report.overwritten_names.push(p.name.clone());
        } else {
            let mut np = p.clone();
            np.id = imported_id();
            np.created_at = crate::platform::now_str();
            np.updated_at = np.created_at.clone();
            report.added += 1;
            report.added_names.push(np.name.clone());
            list.push(np);
        }
        if let Some(k) = &keys {
            if profile_all_mismatched(p, k) {
                report.mismatched.push(p.name.clone());
            }
        }
    }
    save_profiles(&list)?;
    // 偏好合并：只动通用开关，不碰本机基线/音频端点等设备相关状态
    if crate::platform::pref_set_revert_on_exit(bundle.prefs.revert_on_exit).is_ok() {
        report.prefs_applied = true;
    }
    Ok(report)
}

/// 导入档的新 id：时间戳 + 随机后缀，避免与现有 p<时间戳> 撞车
fn imported_id() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut rnd = [0u8; 2];
    let _ = getrandom::getrandom(&mut rnd);
    format!("pm{}{:02x}{:02x}", ts, rnd[0], rnd[1])
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{DisplayTarget, ProfileTargets};

    fn demo_profile(name: &str) -> Profile {
        Profile {
            id: format!("p{}", name.len()),
            name: name.to_string(),
            created_at: "2026-11-06 10:00:00".into(),
            updated_at: "2026-11-06 10:00:00".into(),
            targets: ProfileTargets {
                displays: vec![DisplayTarget {
                    edid_key: Some("JKA27B3".into()),
                    captured_id: Some(r"\\?\DISPLAY#JKA27B3#5&2b3c&0&UID16777488#{e6f07b5f-ee97-4a90-b076-9f59ee8bd074}".into()),
                    resolution: Some("3840x2160".into()),
                    hz: Some(60),
                    rotation: Some(0),
                    brightness: Some(80),
                    volume: Some(30),
                    color_space: Some("sRGB IEC61966-2.1.icm".into()),
                }],
                main_display: Some("\\\\.?\\DISPLAY1\\{5f3e5f9a-...}".into()),
                audio_device_id: Some(r"{0.0.0.00000000}.{a1b2c3d4-1234-5678-9abc-def012345678}".into()),
                hdr: Some(false),
            },
            match_: None,
            auto: false,
        }
    }

    #[test]
    fn export_import_roundtrip() {
        let mut bundle = MigrateBundle {
            schema: 1,
            exported_at: "2026-11-06 22:00:00".into(),
            app_version: "0.3.0".into(),
            platform: "windows".into(),
            profiles: vec![demo_profile("工作模式"), demo_profile("观影模式")],
            prefs: MigratePrefs::default(),
        };
        sanitize_bundle(&mut bundle);
        let json = serde_json::to_string(&bundle).unwrap();
        let payload = encrypt_payload(&json, "hunter2").unwrap();
        assert!(payload.starts_with(MIGRATION_PREFIX));
        // 预览：本机无同名档 → 全部新增（load_profiles 在测试环境返回本机真实数据，
        // 只验证能解出结构与字段，不验证新增数量）
        let pv = migrate_preview_impl(&payload, "hunter2").unwrap();
        assert_eq!(pv.schema, 1);
        assert_eq!(pv.app_version, "0.3.0");
        let total_in = pv.new_profiles.len() + pv.overwrite_profiles.len();
        assert_eq!(total_in, 2);
        // 导入走真实文件系统由 kvm_pair_loopback 风格的冒烟测试覆盖，这里不重复落盘
    }

    #[test]
    fn wrong_password_unified_error() {
        let mut bundle = MigrateBundle {
            schema: 1,
            exported_at: "t".into(),
            app_version: "0.3.0".into(),
            platform: "windows".into(),
            profiles: vec![demo_profile("x")],
            prefs: MigratePrefs::default(),
        };
        sanitize_bundle(&mut bundle);
        let json = serde_json::to_string(&bundle).unwrap();
        let payload = encrypt_payload(&json, "correct-horse").unwrap();
        // 错口令、坏载荷、乱文本三种情况提示完全一致
        let e1 = migrate_preview_impl(&payload, "wrong-pass").unwrap_err();
        let e2 = migrate_preview_impl("SCMIG1:not-base64!!", "correct-horse").unwrap_err();
        let e3 = migrate_preview_impl("hello world", "x").unwrap_err();
        assert_eq!(e1, DECRYPT_FAIL_MSG);
        assert_eq!(e2, DECRYPT_FAIL_MSG);
        assert_eq!(e3, DECRYPT_FAIL_MSG);
    }

    #[test]
    fn sanitized_json_has_no_device_identity() {
        let mut bundle = MigrateBundle {
            schema: 1,
            exported_at: "t".into(),
            app_version: "0.3.0".into(),
            platform: "windows".into(),
            profiles: vec![demo_profile("工作模式")],
            prefs: MigratePrefs::default(),
        };
        sanitize_bundle(&mut bundle);
        let json = serde_json::to_string(&bundle).unwrap();
        // 机器名不得出现
        let host = crate::platform::kvm_device_name();
        assert!(!json.to_lowercase().contains(&host.to_lowercase()), "明文 JSON 泄露机器名：{}", host);
        // 设备实例路径/端点 id 不得出现（GUID 形态）
        assert!(!json.contains("e6f07b5f"), "泄露 captured_id 设备路径");
        assert!(!json.contains("a1b2c3d4"), "泄露音频端点 id");
        // EDID 型号键允许保留（适配匹配需要）
        assert!(json.contains("JKA27B3"));
    }

    #[test]
    fn password_length_validated() {
        let r = migrate_export_impl(None, "short");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("6–32"));
        let r = migrate_export_impl(None, "");
        assert!(r.is_err());
    }

    #[test]
    fn future_schema_prompts_upgrade() {
        // 伪造更高版本前缀（不解密就能识别版本）
        let r = migrate_preview_impl("SCMIG9:AAAA", "whatever");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("请先升级"));
    }
}

/// 真机回环冒烟：备份用户数据 → 造两个测试档 → 导出 → 预览 → 导入落盘 → 核对 → 恢复。
/// 验收点：档内容完整、同名覆盖保留本地 id、明文 JSON 搜不到机器名、错口令统一报错。
#[test]
#[ignore]
fn migrate_smoke() {
    let dir = crate::platform::profiles_file()
        .parent()
        .expect("profiles 目录")
        .to_path_buf();
    let path = dir.join("profiles.json");
    let backup = dir.join("profiles.json.mig-bak");
    let had_user = path.exists();
    if had_user {
        std::fs::copy(&path, &backup).expect("备份 profiles.json 失败");
    }

    let restore = |msg: &str| {
        let _ = std::fs::remove_file(&path);
        if had_user {
            std::fs::rename(&backup, &path).expect(msg);
        }
    };

    let r = (|| -> Result<(), String> {
        use crate::platform::{DisplayTarget, ProfileTargets};
        // ① 造两个测试档（一个会用到的名字在导入时同名覆盖）
        let mk = |name: &str| crate::platform::Profile {
            id: crate::platform::new_profile_id(),
            name: name.to_string(),
            created_at: "t".into(),
            updated_at: "t".into(),
            targets: ProfileTargets {
                displays: vec![DisplayTarget {
                    edid_key: Some("JKA27B3".into()),
                    captured_id: Some("uid-with-serial-abc123".into()),
                    resolution: Some("3840x2160".into()),
                    hz: Some(60),
                    rotation: Some(0),
                    brightness: Some(80),
                    volume: Some(40),
                    color_space: None,
                }],
                main_display: Some("local-display-id-xyz".into()),
                audio_device_id: Some("endpoint-instance-guid".into()),
                hdr: Some(false),
            },
            match_: None,
            auto: false,
        };
        let list = vec![mk("迁移冒烟-覆盖档"), mk("迁移冒烟-新增档")];
        crate::platform::save_profiles(&list)?;
        let ids: Vec<String> = list.iter().map(|p| p.id.clone()).collect();

        // ② 导出（真机偏好读写）
        let payload = migrate_export_impl(Some(&ids), "smoke-pass-1")?;
        assert!(payload.starts_with(MIGRATION_PREFIX));

        // ③ 明文核对：脱敏后的 bundle 搜不到设备唯一标识与机器名
        let bundle = parse_bundle(&payload, "smoke-pass-1")?;
        let json = serde_json::to_string(&bundle).unwrap();
        assert!(!json.contains("serial-abc123"), "明文泄露 captured_id");
        assert!(!json.contains("endpoint-instance-guid"), "明文泄露音频端点 id");
        assert!(!json.contains("local-display-id-xyz"), "明文泄露 main_display");
        let host = crate::platform::kvm_device_name();
        assert!(!json.to_lowercase().contains(&host.to_lowercase()), "明文泄露机器名 {}", host);

        // ④ 预览（此刻本机就有这两个名字 → 全部走同名覆盖路径）
        let pv = migrate_preview_impl(&payload, "smoke-pass-1")?;
        assert_eq!(pv.overwrite_profiles.len(), 2, "预览应识别 2 个同名覆盖");
        assert_eq!(pv.new_profiles.len(), 0);

        // ⑤ 导入落盘并核对：本地 id 保留、数量不翻倍
        let before = crate::platform::load_profiles();
        let before_ids: Vec<String> = before.iter().map(|p| p.id.clone()).collect();
        let rep = migrate_import_impl(&payload, "smoke-pass-1")?;
        assert_eq!(rep.overwritten, 2, "应覆盖 2 个同名档");
        let after = crate::platform::load_profiles();
        let after_ids: Vec<String> = after.iter().map(|p| p.id.clone()).collect();
        for id in &before_ids {
            assert!(after_ids.contains(id), "同名覆盖必须保留本地 id {}", id);
        }
        assert_eq!(after.len(), before.len(), "同名覆盖不应增加档数");
        // 覆盖后 captured_id 仍为空（导入内容来自脱敏 bundle）
        let overwritten = after.iter().find(|p| p.name == "迁移冒烟-覆盖档").unwrap();
        assert!(overwritten.targets.displays[0].captured_id.is_none());

        // ⑥ 错口令统一报错
        let e = migrate_preview_impl(&payload, "wrong-pass").unwrap_err();
        assert_eq!(e, DECRYPT_FAIL_MSG);
        println!("migrate_smoke 全部通过（覆盖 2，新增 0，机器名={}）", host);
        Ok(())
    })();

    // ⑦ 恢复用户数据
    restore("恢复用户 profiles.json 失败");
    if let Err(e) = r {
        panic!("migrate_smoke 失败：{}", e);
    }
}
