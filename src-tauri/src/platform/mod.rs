use serde::Serialize;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// 电源联动开关（进程级共享）：Ctrl+Alt+L 全局快捷键只有在该开关打开时才会
/// 让屏幕待机/唤醒 —— 否则用户没开联动，按快捷键屏幕却黑了（用户反馈）。
/// lib.rs 在读写 power-link-enabled 偏好时同步到这里。
static POWER_LINK_ON: AtomicBool = AtomicBool::new(false);

pub fn set_power_link_on(on: bool) {
    POWER_LINK_ON.store(on, Ordering::Relaxed);
}

pub fn power_link_on() -> bool {
    POWER_LINK_ON.load(Ordering::Relaxed)
}

/// 显示器的跨平台结构化描述
#[derive(Serialize, Clone, Debug)]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub resolution: String,
    pub hz: Option<u32>,
    pub main: bool,
    pub connected: bool,
    pub ddc: bool,
    pub brightness: Option<u32>,
    pub volume: Option<u32>,
    /// 当前色彩配置文件名（ColorSync profile）
    pub color_profile: Option<String>,
    /// 平台特定的显示器句柄（macOS=CGDirectDisplayID；Windows 用内部 id）
    #[serde(skip_serializing)]
    #[allow(dead_code)] // macOS 用于 DDC 句柄；Windows 恒为 None，非死代码
    pub ddc_id: Option<u32>,
}

/// Operating-system sleep state for one display; this does not claim physical panel power.
#[derive(Serialize, Clone, Debug)]
pub struct DisplaySleepState {
    pub id: String,
    pub asleep: bool,
}

/// macOS 当前旋转方向可用的显示模式。
#[derive(Serialize, Clone, Debug)]
pub struct DisplayMode {
    pub mode: u32,
    pub resolution: String,
    pub hz: u32,
    pub color_depth: u32,
    pub scaled: bool,
    pub current: bool,
}

/// 通用命令运行器：返回 stdout（成功）或错误信息。
///
/// 注意：不能只看退出码。PowerShell 的 `-Command` 只要脚本里产生过任何 ErrorRecord
/// （哪怕已被 `-ErrorAction SilentlyContinue` 抑制、stdout 已经输出完整结果）就会返回
/// 退出码 1。实测 get_displays 的脚本正是如此：stdout 是完整的显示器列表，退出码却是 1，
/// 于是整份列表被当成失败丢弃 → 界面显示"未检测到显示器"。
/// 因此以 stdout 为准：有输出就是成功。
pub fn run_cmd(program: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    // CREATE_NO_WINDOW：PowerShell / adb 等子进程不要闪出控制台黑框。
    // 之前漏了这个标志，每次 DDC 读写、蓝牙查询都会在屏幕上闪一个终端窗口。
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("无法启动 {}: {}", program, e))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !stdout.trim().is_empty() {
        return Ok(stdout);
    }
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr)
            .to_string()
            .trim()
            .to_string())
    }
}

/// Open one of the application's fixed public pages in the system browser.
#[cfg(target_os = "macos")]
pub fn open_external_page(page: &str) -> Result<(), String> {
    let url = match page {
        "releases" => "https://github.com/Personalscreenguard/screenguard/releases/latest",
        "privacy" => "https://github.com/Personalscreenguard/screenguard/blob/main/PRIVACY.md",
        _ => return Err("不支持打开此外部页面".into()),
    };
    Command::new("/usr/bin/open").arg(url).spawn()
        .map(|_| ()).map_err(|e| format!("无法打开系统浏览器：{}", e))
}

#[cfg(target_os = "windows")]
pub fn open_external_page(page: &str) -> Result<(), String> {
    let url = match page {
        "releases" => "https://github.com/Personalscreenguard/screenguard/releases/latest",
        "privacy" => "https://github.com/Personalscreenguard/screenguard/blob/main/PRIVACY.md",
        _ => return Err("不支持打开此外部页面".into()),
    };
    Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", url]).spawn()
        .map(|_| ()).map_err(|e| format!("无法打开系统浏览器：{}", e))
}

/// 判断某命令是否存在于 PATH（macOS 用；Windows 走 PowerShell/注册表，无需此函数）
#[cfg(not(target_os = "windows"))]
pub fn which(program: &str) -> bool {
    Command::new("which")
        .arg(program)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 系统默认输出设备信息（Windows CoreAudio 端点 / macOS 系统默认输出）
/// 结构体放共享层：命令签名两平台一致。
#[derive(Serialize, Clone, Debug)]
pub struct SystemAudio {
    /// 端点友好名（如「扬声器 (2- Realtek(R) Audio)」「NE160QDM-NZ8 (NVIDIA High Definition Audio)」）
    pub name: String,
    /// 当前音量 0-100
    pub volume: u32,
    /// 是否静音
    pub mute: bool,
    /// false = 固定音量端点：Windows 滑块也无效（部分 HDMI/DP 音频如此），
    /// 此时显示器喇叭音量请用对应显示器的 DDC 0x62 滑块
    pub adjustable: bool,
    /// 端点形态因子（9 = DigitalAudioDisplayDevice，即 HDMI/DP 显示器音频）
    pub form_factor: u32,
}

/// 带电池的连接设备（笔记本内电池 / USB·2.4G 无线键鼠 / 蓝牙耳机鼠标等）
/// Windows 枚举内置/外设电池；macOS 返回内置电池；结构体放共享层。
#[derive(Serialize, Clone, Debug)]
pub struct BatteryInfo {
    /// 设备名（注册表友好名，兜底电池栈设备名）
    pub name: String,
    /// 电量百分比 0-100；-1 = 读取不到
    pub percent: i32,
    /// 充电中
    pub charging: bool,
    /// 已接外接电源（满电 / 浮充）
    pub on_ac: bool,
    /// 连接类型：内置 / 蓝牙 / USB/无线 / USB / 其它
    pub conn: String,
    /// 健康度（满充容量/设计容量 ×100）；-1 = 该设备不提供
    pub health: i32,
    /// 当前容量（mWh；相对模式的 HID 外设为 0）
    pub cap_mwh: u32,
    /// 满充容量（mWh；相对模式的 HID 外设为 0）
    pub full_mwh: u32,
}

// ---------- 结构化体检报告（P0-1，两平台共用数据契约） ----------

/// 检查项严重程度。serde 用小写：ok / info / warn / error
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// 显式正常项（检查列表可能显式输出「无问题」条目）
    #[allow(dead_code)]
    Ok,
    Info,
    Warn,
    Error,
}

/// 一条检查结果。id 用稳定短标识（如 "refresh_below_max"），前端按 id 配图标与文案。
#[derive(Serialize, Clone, Debug)]
pub struct HealthCheck {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    /// 具体数值或命令输出，不许只给结论
    pub evidence: Vec<String>,
    /// 可接入的修复动作 id（P0-4 接线；当前前端只渲染占位按钮）
    pub fix_ids: Vec<String>,
}

/// 单块屏的体检结果
#[derive(Serialize, Clone, Debug)]
pub struct DisplayHealth {
    pub display_id: String,
    pub name: String,
    pub native_resolution: Option<String>,
    pub current_resolution: String,
    pub max_hz: Option<u32>,
    pub current_hz: Option<u32>,
    pub ddc_capable: bool,
    pub ddc_readable: bool,
    pub color_profile: Option<String>,
    pub hdr: Option<bool>,
    pub connected: bool,
    pub checks: Vec<HealthCheck>,
    /// 显示链路拓扑（GPU→接口→线缆→扩展坞→显示器）；未在活动路径上的屏为 None
    pub topology: Option<Topology>,
}

// ---------- 显示链路拓扑（P0-2，两平台共用数据契约） ----------

/// 跳的类别。serde 用小写：gpu / port / cable / hub / display
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HopKind {
    Gpu,
    Port,
    Cable,
    Hub,
    Display,
}

/// 跳的状态。ok = 确认无问题；suspect = 有算式/证据的疑似问题；unknown = 系统未提供，不猜
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HopStatus {
    Ok,
    Suspect,
    Unknown,
}

/// 链路上的一跳
#[derive(Serialize, Clone, Debug)]
pub struct TopologyHop {
    pub kind: HopKind,
    pub label: String,
    pub detail: Option<String>,
    pub status: HopStatus,
}

/// 单屏链路拓扑：按顺序 GPU → 接口 → 线缆 → 扩展坞 → 显示器
#[derive(Serialize, Clone, Debug)]
pub struct Topology {
    pub hops: Vec<TopologyHop>,
    pub note: Option<String>,
}

/// 当前模式所需的未压缩视频数据速率（Gbps，8bpc RGB）。
/// 像素时钟 ≈ W×H×Hz × 1.08（CVT-RB 消隐开销近似），每像素 24 bit。
/// 例：3840×2160@60 → 12.9 Gbps；2560×1440@144 → 13.8 Gbps。
pub(crate) fn required_bandwidth_gbps(w: u32, h: u32, hz: u32) -> f64 {
    w as f64 * h as f64 * hz as f64 * 24.0 * 1.08 / 1e9
}

/// 常见接口协议的未压缩带宽上限（Gbps，含文档口径）：按从低到高排列
pub(crate) const PROTO_CAPS: [(&str, f64); 6] = [
    ("HDMI 1.4", 8.16),
    ("HDMI 2.0", 14.4),
    ("DP 1.2", 17.28),
    ("DP 1.4", 25.92),
    ("HDMI 2.1", 42.6),
    ("DP 2.0", 77.4),
];

/// 带宽档位阶梯文本：所需带宽 X Gbps，哪些协议够 / 不够
pub(crate) fn bandwidth_ladder(req: f64) -> String {
    let mut below: Vec<&str> = Vec::new();
    let mut above: Vec<&str> = Vec::new();
    for (name, cap) in PROTO_CAPS.iter() {
        if req > *cap {
            below.push(name);
        } else {
            above.push(name);
        }
    }
    if above.is_empty() {
        format!(
            "约 {:.1} Gbps，超出所有常见协议上限（DP 2.0 ≈ 77.4），需 DSC 压缩或专用链路",
            req
        )
    } else {
        format!(
            "约 {:.1} Gbps：{} 承载不了，{} 及以上可以",
            req,
            below.join("、"),
            above.first().unwrap_or(&"?")
        )
    }
}

/// 汇总：三种计数 + 一句人话结论
#[derive(Serialize, Clone, Debug)]
pub struct HealthSummary {
    pub errors: u32,
    pub warnings: u32,
    pub infos: u32,
    pub headline: String,
}

/// 完整体检报告
#[derive(Serialize, Clone, Debug)]
pub struct HealthReport {
    pub generated_at: String,
    pub platform: String,
    pub app_version: String,
    pub os_version: String,
    pub displays: Vec<DisplayHealth>,
    /// 便携屏一线通区域（P1-4）；仅在检测到便携屏时非空，前端据此决定是否渲染该区域
    pub portable: Vec<PortableSection>,
    pub summary: HealthSummary,
}

// ---------- 便携屏一线通功耗诊断（P1-4，两平台共用判定与文案） ----------
//
// 诚实边界（写死）：软件改不了供电。这一项的价值是"告诉用户为什么暗/为什么闪，以及
// 该怎么接"，不是"修好它"。绝不做"一键提升亮度""解锁面板上限"这类假承诺。

/// 前端「便携屏」区域的数据（仅检测到便携屏时由后端给出）
#[derive(Serialize, Clone, Debug)]
pub struct PortableSection {
    pub display_id: String,
    pub name: String,
    /// 一句结论
    pub conclusion: String,
    /// 可执行建议（≤5 条，措辞固定，不许改成承诺式）
    pub advice: Vec<String>,
    /// 支撑数据（识别依据、亮度数值），不许只给结论
    pub evidence: Vec<String>,
}

/// 建议库（固定 5 条，来自开发提示词包；验收要求不出现"提升/解锁/增强亮度"式承诺）
pub(crate) const ADVICE_DOCK_POWER: &str = "改用带独立供电的 USB-C 扩展坞";
pub(crate) const ADVICE_SEPARATE_POWER: &str = "便携屏接专用电源适配器，只用 C 线传视频";
pub(crate) const ADVICE_TURN_OFF_HDR: &str = "在便携屏上关闭 HDR，改用 SDR + 提高亮度";
pub(crate) const ADVICE_DROP_60HZ: &str = "把便携屏刷新率降到 60Hz 释放带宽";
pub(crate) const ADVICE_FULL_CABLE: &str =
    "换用支持 DP Alt Mode 的全功能 C 线（当前线材可能只有 USB 2.0 数据能力）";

/// EDID 物理尺寸（cm）→ 对角线英寸
pub(crate) fn diagonal_inches(cm_h: f64, cm_v: f64) -> f64 {
    (cm_h * cm_h + cm_v * cm_v).sqrt() / 2.54
}

/// 分辨率是否 ≤2K（最长边 ≤2560 视为 2K 档；1920x1080、2560x1440、2560x1600 都算）
pub(crate) fn res_at_most_2k(w: u32, h: u32) -> bool {
    let m = w.max(h);
    m > 0 && m <= 2560
}

/// 便携屏判定（只按实测，绝不猜）：
/// - 内建面板永远不算（一线通只针对外接屏，也避免把笔记本内屏误报成便携屏）；
/// - EDID 上报了物理尺寸：对角线 < 18 英寸 → 便携屏；≥ 18 英寸 → 明确是桌面屏，不再走兜底；
/// - EDID 尺寸缺失：分辨率 ≤2K 的外接屏按便携屏对待（桌面显示器几乎都上报尺寸，此兜底极少命中）。
/// 返回 Some(识别依据)；判定不了返回 None。
pub(crate) fn portable_reason(
    builtin: bool,
    diag_in: Option<f64>,
    res: Option<(u32, u32)>,
) -> Option<String> {
    if builtin {
        return None;
    }
    match diag_in {
        Some(d) if d >= 18.0 => None,
        Some(d) if d > 0.0 => {
            Some(format!("EDID 上报物理尺寸约 {:.1} 英寸（< 18 英寸）", d))
        }
        _ => match res {
            Some((w, h)) if res_at_most_2k(w, h) => Some(format!(
                "EDID 未上报物理尺寸，按外接屏 + 分辨率 {}x{}（≤2K）判为便携屏",
                w, h
            )),
            _ => None,
        },
    }
}

/// 便携屏功耗诊断四项（P1-4）。全部只解释机制 + 给接线建议。
/// 入参均为实测值；拿不到的传 None，对应检查不产生（不猜）。
/// - bri_cur/bri_max：DDC VCP 0x10 读回的当前亮度与面板上报上限
/// - hdr_on：HDR 开关状态（读不到传 None）
/// - usbc_evidence：能判断 USB-C 一线通时给依据文本，判断不了传 None
/// - multi：多屏同时高刷时给 (总带宽 Gbps, 各屏明细行)，否则 None
pub(crate) fn build_portable_checks(
    reason: &str,
    bri_cur: Option<u32>,
    bri_max: Option<u32>,
    hdr_on: Option<bool>,
    usbc_evidence: Option<String>,
    multi: Option<(f64, Vec<String>)>,
) -> Vec<HealthCheck> {
    let mut v = Vec::new();

    // ① portable_power_budget：亮度已到 100 仍偏暗 → 供电可能不足，驱动板在限制背光
    if let Some(cur) = bri_cur {
        if cur >= 100 {
            let mut ev = vec![format!("当前亮度 = {}（已到上限）", cur)];
            match bri_max {
                Some(m) if m > cur => ev.push(format!(
                    "面板 VCP 0x10 上报范围 0–{}：软件的 100 只在其标尺的 {:.0}%",
                    m,
                    cur as f64 / m as f64 * 100.0
                )),
                Some(m) => ev.push(format!("面板 VCP 0x10 上报范围 0–{}", m)),
                None => ev.push("面板 VCP 上限读取不到，按软件标尺 0–100 判断".to_string()),
            }
            ev.push(format!("识别依据：{}", reason));
            v.push(HealthCheck {
                id: "portable_power_budget".into(),
                severity: Severity::Warn,
                title: "亮度已到上限，屏幕仍偏暗（供电可能不足）".into(),
                detail: "亮度调到最大仍然偏暗，常见于一线通供电不足：背光功率上不去，驱动板主动压低亮度来保证供电稳定。这是供电问题，软件改不了，建议改接线方式。".into(),
                evidence: ev,
                fix_ids: vec![],
            });
        }
    }

    // ② usbc_single_cable：能判断一线通时才发（判断不了不猜）
    if let Some(usbc) = usbc_evidence {
        v.push(HealthCheck {
            id: "usbc_single_cable".into(),
            severity: Severity::Info,
            title: "USB-C 一线通同时供电与传视频".into(),
            detail: "一根 C 线同时取电和传视频时，开启 HDR 或高刷会挤占背光功率，出现亮度下降或闪屏。".into(),
            evidence: vec![usbc],
            fix_ids: vec![],
        });
    }

    // ③ hdr_on_portable：便携屏开 HDR
    if hdr_on == Some(true) {
        v.push(HealthCheck {
            id: "hdr_on_portable".into(),
            severity: Severity::Warn,
            title: "便携屏开启 HDR，亮度可能不升反降".into(),
            detail: "便携屏开 HDR 常导致亮度不升反降：HDR 提高背光瞬态功耗，供电不足的驱动板会压低背光来保供电。建议在便携屏上关闭 HDR。".into(),
            evidence: vec!["HDR = 开".into()],
            fix_ids: vec![],
        });
    }

    // ④ multi_display_bandwidth：多屏同时高刷，复用 P0-2 的带宽反推
    if let Some((total, lines)) = multi {
        let mut ev = lines;
        ev.push(format!("总带宽 ≈ {:.1} Gbps（超过 DP 1.2 上限 17.28）", total));
        v.push(HealthCheck {
            id: "multi_display_bandwidth".into(),
            severity: Severity::Warn,
            title: "多屏同时高刷，总带宽接近链路上限".into(),
            detail: "多块屏同时高刷时总带宽超过 DP 1.2 上限，接在扩展坞或单条链路上会互相挤占，可能出现掉帧或闪烁。把非主力屏降到 60Hz 释放带宽。".into(),
            evidence: ev,
            fix_ids: vec![],
        });
    }

    v
}

/// 组装前端「便携屏」区域数据：一句结论 + 建议清单（去重、≤5 条）+ 支撑数据
pub(crate) fn build_portable_section(
    display_id: &str,
    name: &str,
    reason: &str,
    bri_cur: Option<u32>,
    bri_max: Option<u32>,
    hdr_on: Option<bool>,
    high_multi: bool,
) -> PortableSection {
    let conclusion = if hdr_on == Some(true) {
        "这块屏按便携屏识别，且 HDR 正开着——亮度上不去多半与 HDR 挤占供电有关。".to_string()
    } else if bri_cur.map_or(false, |b| b >= 100) {
        "这块屏按便携屏识别：亮度已到上限仍偏暗，多半是一线通供电不足限制了背光。".to_string()
    } else {
        "这块屏按外接便携屏识别：一线通供电时，亮度与高刷能力会受供电限制。".to_string()
    };

    let mut advice = Vec::new();
    if hdr_on == Some(true) {
        advice.push(ADVICE_TURN_OFF_HDR.to_string());
    }
    if high_multi {
        advice.push(ADVICE_DROP_60HZ.to_string());
    }
    advice.push(ADVICE_SEPARATE_POWER.to_string());
    if bri_cur.map_or(false, |b| b >= 100) {
        advice.push(ADVICE_DOCK_POWER.to_string());
    }
    advice.push(ADVICE_FULL_CABLE.to_string());
    let mut seen: Vec<String> = Vec::new();
    advice.retain(|a| {
        if seen.contains(a) {
            false
        } else {
            seen.push(a.clone());
            true
        }
    });

    let mut evidence = vec![format!("识别依据：{}", reason)];
    match (bri_cur, bri_max) {
        (Some(c), Some(m)) => evidence.push(format!("当前亮度 {}（面板上报上限 {}）", c, m)),
        (Some(c), None) => evidence.push(format!("当前亮度 {}（面板上限未读取到）", c)),
        _ => {}
    }
    if hdr_on == Some(true) {
        evidence.push("HDR = 开".into());
    }

    PortableSection {
        display_id: display_id.to_string(),
        name: name.to_string(),
        conclusion,
        advice,
        evidence,
    }
}

impl HealthReport {
    /// 按 checks 聚合 summary，headline 用一句人话描述
    pub fn summarize(&mut self) {
        let (mut e, mut w, mut i) = (0u32, 0u32, 0u32);
        for d in &self.displays {
            for c in &d.checks {
                match c.severity {
                    Severity::Error => e += 1,
                    Severity::Warn => w += 1,
                    Severity::Info => i += 1,
                    Severity::Ok => {}
                }
            }
        }
        let n = self.displays.len();
        let active = self.displays.iter().filter(|d| d.connected).count();
        self.summary = HealthSummary {
            errors: e,
            warnings: w,
            infos: i,
            headline: if !active_is_sane(active, n) || e > 0 {
                format!("{} 块屏已连接，其中 {} 项需要处理", active, e)
            } else if w > 0 {
                format!("{} 块屏已连接，其中 {} 项可优化", active, w)
            } else if i > 0 {
                format!("{} 块屏已连接，整体状态良好，{} 条提示", active, i)
            } else {
                format!("{} 块屏已连接，一切正常", active)
            },
        };
    }
}

fn active_is_sane(active: usize, _total: usize) -> bool {
    active > 0
}

// ---------- 体检报告 Markdown 导出（P0-3，两平台共用渲染与脱敏） ----------

/// 默认脱敏：把一行里可能带隐私的片段替换成占位符。
/// 判据刻意从宽——本报告的正常字段（型号名、分辨率、带宽算式等）都不含反斜杠，
/// 因此任何出现反斜杠的词块都按路径处理：保留「设备路径：」这类中文前缀，路径本体隐藏。
/// 覆盖：\\?\ 内核路径、MONITOR\USB\VID_ 设备实例路径、C:\Users\<用户名> 文件路径。
pub(crate) fn sanitize_line(s: &str) -> String {
    const PH: &str = "〈路径已隐藏〉";
    let mut out = String::with_capacity(s.len());
    for (i, tok) in s.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match tok.find('\\') {
            None => out.push_str(tok),
            Some(pos) => {
                // 前缀（如「设备路径」）有上下文价值，保留；「C:」这类盘符没有，直接整块替换
                let prefix = tok[..pos].trim_end_matches(|c| matches!(c, '：' | ':' | '=' | ' '));
                let keep_prefix = !prefix.is_empty()
                    && prefix.chars().count() > 2
                    && !prefix.ends_with(':')
                    && !prefix.ends_with('：');
                if keep_prefix {
                    out.push_str(prefix);
                    out.push('：');
                }
                out.push_str(PH);
            }
        }
    }
    out
}

/// Markdown 表格单元格转义：竖线会切断列、换行会破坏行结构，必须处理。
/// 先脱敏再转义，保证导出的表格在 GitHub/知乎/微信里列数一致。
pub(crate) fn md_cell(s: &str) -> String {
    sanitize_line(s)
        .replace('|', "\\|")
        .replace('\r', "")
        .replace('\n', " ")
}

/// 显示器序列号（Windows 实现；其它平台暂无读取通道，返回空 = 如实标「未获取」）。
/// 注意：只有在用户明确选择「包含序列号」时才会被调用，默认导出不读它。
#[cfg(not(target_os = "windows"))]
pub fn display_serials() -> Vec<(String, String)> {
    Vec::new()
}

fn sev_cn(s: &Severity) -> &'static str {
    match s {
        Severity::Error => "问题",
        Severity::Warn => "建议",
        Severity::Info => "提示",
        Severity::Ok => "正常",
    }
}

fn sev_rank(s: &Severity) -> u8 {
    match s {
        Severity::Error => 0,
        Severity::Warn => 1,
        Severity::Info => 2,
        Severity::Ok => 3,
    }
}

/// 把体检报告渲染成 Markdown（P0-3）。结构固定：元信息表 → 每屏一节（基本表/链路/问题）→ 落款。
/// `include_serial = false`（默认）时报告里不出现序列号，也根本不去读。
pub fn render_health_markdown(
    r: &HealthReport,
    serials: &[(String, String)],
    include_serial: bool,
) -> String {
    let platform_cn = match r.platform.as_str() {
        "windows" => "Windows",
        "macos" => "macOS",
        other => other,
    };
    let mut m = String::from("# 屏幕连接助手 · 显示体检报告\n\n");
    if include_serial {
        m.push_str("> 注意：本报告包含设备序列号，请勿公开分享。\n\n");
    }

    // 元信息表（固定两列）
    let active = r.displays.iter().filter(|d| d.connected).count();
    // 系统版本串常自带「Microsoft Windows …」前缀，去重避免出现「Windows Microsoft Windows …」
    let mut os_line = r.os_version.trim().to_string();
    if os_line.to_lowercase().starts_with("microsoft ") {
        os_line = os_line[10..].trim().to_string();
    }
    let lower = os_line.to_lowercase();
    if !lower.is_empty() && !lower.starts_with("windows") && !lower.starts_with("macos") {
        os_line = format!("{} {}", platform_cn, os_line);
    }
    if os_line.is_empty() {
        os_line = platform_cn.to_string();
    }
    m.push_str("| 项目 | 内容 |\n| --- | --- |\n");
    m.push_str(&format!("| 生成时间 | {} |\n", md_cell(&r.generated_at)));
    m.push_str(&format!("| 平台 | {} |\n", md_cell(&os_line)));
    m.push_str(&format!(
        "| 应用版本 | v{} |\n",
        md_cell(&r.app_version)
    ));
    m.push_str(&format!(
        "| 显示器数量 | {}（活动中 {}） |\n",
        r.displays.len(),
        active
    ));
    m.push_str(&format!(
        "| 结论 | {} |\n\n",
        md_cell(&r.summary.headline)
    ));

    for (i, d) in r.displays.iter().enumerate() {
        m.push_str(&format!(
            "## 显示器 {}：{}\n\n",
            i + 1,
            md_cell(&d.name)
        ));
        let hz = d
            .current_hz
            .map(|v| format!(" @{}Hz", v))
            .unwrap_or_default();
        m.push_str("| 项目 | 内容 |\n| --- | --- |\n");
        m.push_str(&format!(
            "| 状态 | {} |\n",
            if d.connected { "活动中" } else { "未在活动路径上" }
        ));
        m.push_str(&format!(
            "| 当前模式 | {} |\n",
            md_cell(&format!("{}{}", d.current_resolution, hz))
        ));
        m.push_str(&format!(
            "| 原生分辨率 | {} |\n",
            md_cell(d.native_resolution.as_deref().unwrap_or("系统未提供"))
        ));
        m.push_str(&format!(
            "| DDC/CI | {} |\n",
            if d.ddc_readable {
                "可用"
            } else if d.ddc_capable {
                "可识别但读不到数据"
            } else {
                "不可达"
            }
        ));
        m.push_str(&format!(
            "| 色彩配置 | {} |\n",
            md_cell(d.color_profile.as_deref().unwrap_or("系统未提供"))
        ));
        m.push_str(&format!(
            "| HDR | {} |\n",
            match d.hdr {
                Some(true) => "开",
                Some(false) => "关",
                None => "系统未提供",
            }
        ));
        if include_serial {
            let sn = serials
                .iter()
                .find(|(n, _)| !n.is_empty() && (n.eq_ignore_ascii_case(&d.name)))
                .map(|(_, s)| s.as_str())
                .unwrap_or("未获取（系统未报告）");
            m.push_str(&format!("| 序列号 | {} |\n", md_cell(sn)));
        } else {
            m.push_str("| 序列号 | 未输出（不含设备序列号） |\n");
        }

        if let Some(t) = &d.topology {
            let chain: Vec<&str> = t.hops.iter().map(|h| h.label.as_str()).collect();
            m.push_str(&format!("\n连接链路：{}\n", chain.join(" → ")));
        }

        if d.checks.is_empty() {
            m.push_str("\n这块屏没有发现需要留意的地方。\n\n");
        } else {
            m.push_str("\n### 发现的问题\n\n");
            let mut checks: Vec<&HealthCheck> = d.checks.iter().collect();
            checks.sort_by(|a, b| {
                sev_rank(&a.severity)
                    .cmp(&sev_rank(&b.severity))
                    .then_with(|| a.id.cmp(&b.id))
            });
            for c in checks {
                m.push_str(&format!(
                    "- **[{}] {}**\n",
                    sev_cn(&c.severity),
                    c.title
                ));
                let detail = sanitize_line(&c.detail);
                if !detail.is_empty() {
                    m.push_str(&format!("  - 细节：{}\n", detail));
                }
                for ev in &c.evidence {
                    let ev = sanitize_line(ev);
                    if !ev.is_empty() {
                        m.push_str(&format!("  - 证据：{}\n", ev));
                    }
                }
            }
            m.push('\n');
        }
    }

    m.push_str("---\n\n");
    m.push_str(&format!(
        "由 屏幕连接助手 v{} 生成 · 下载与反馈：https://github.com/new-screen-connection/screen-connection\n",
        r.app_version
    ));
    m
}

/// 导出用：取报告 → （可选）读序列号 → 渲染 Markdown。
pub fn health_report_markdown_impl(include_serial: bool) -> Result<String, String> {
    let report = health_report_impl()?;
    let serials = if include_serial {
        display_serials()
    } else {
        Vec::new()
    };
    Ok(render_health_markdown(&report, &serials, include_serial))
}

// ---------- 自愈动作集（P0-4，两平台共用契约） ----------

/// 修复动作结果。applied = 已执行且回读核验；not_applicable = 此平台/此屏做不到（附人话原因）；
/// failed = 试了但没成（附手动操作路径）。needs_confirmation / needs_permission 预留——
/// 二次确认统一在前端做，后端不拦。
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepairResult {
    Applied,
    NotApplicable,
    Failed,
    NeedsConfirmation,
    NeedsPermission,
}

/// 一个修复动作的执行结果。message 必须是给用户看的人话，说清做了什么、现在什么状态。
#[derive(Serialize, Clone, Debug)]
pub struct RepairOutcome {
    pub action: String,
    pub result: RepairResult,
    pub message: String,
    /// 回读核验后实际发生变化的项（如「显示器数量 1 → 2」「输入源 → DisplayPort」）
    pub changed: Vec<String>,
}

/// 需要前端弹二次确认才允许执行的动作（会改分辨率/刷新率/输入源/暂时关屏）。
/// 前后端共用同一份名单，避免两端各写一套判断。
pub const REPAIR_NEEDS_CONFIRM: &[&str] = &["reapply_mode", "switch_input", "ddc_power_cycle"];

// ---------- 场景档 Profile（P0-5，两平台共用契约与存储） ----------

/// 一个场景档：一组显示相关配置的命名快照。match/auto 供 P1-1 自动触发，本轮只存字段。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub targets: ProfileTargets,
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
    pub match_: Option<ProfileMatch>,
    #[serde(default)]
    pub auto: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct ProfileTargets {
    /// 逐屏目标：按 EDID 厂商码+产品码匹配，匹配不到则按顺序兜底（got 里会标注）
    #[serde(default)]
    pub displays: Vec<DisplayTarget>,
    /// 主屏（macOS 能可靠切换；Windows 捕获时不记录此字段）
    #[serde(default)]
    pub main_display: Option<String>,
    #[serde(default)]
    pub audio_device_id: Option<String>,
    #[serde(default)]
    pub hdr: Option<bool>,
}

/// 单屏目标。所有字段可选——捕获时读得到什么存什么，应用时存了什么应用什么。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct DisplayTarget {
    /// EDID 厂商码+产品码，形如 "JKA27B3"（拿不到为 None，走顺序兜底）
    #[serde(default)]
    pub edid_key: Option<String>,
    /// 捕获时的 display_id（顺序兜底用 + 报告里好认）
    #[serde(default)]
    pub captured_id: Option<String>,
    /// "3840x2160"
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub hz: Option<u32>,
    /// 0=横 1=顺时针90 2=180 3=逆时针90
    #[serde(default)]
    pub rotation: Option<u32>,
    /// 0-100
    #[serde(default)]
    pub brightness: Option<u32>,
    #[serde(default)]
    pub volume: Option<u32>,
    /// ICC 配置名（捕获时的 profile 名，应用时模糊匹配 icc_path）
    #[serde(default)]
    pub color_space: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct ProfileMatch {
    #[serde(default = "default_true")]
    pub require_all_displays: bool,
    #[serde(default)]
    pub docking: Option<bool>,
    #[serde(default)]
    pub on_ac: Option<bool>,
    /// 时间段触发，形如 "09:00-19:00"（支持跨零点如 "22:00-06:00"；空/非法 = 不限）
    #[serde(default)]
    pub time_range: Option<String>,
}

fn default_true() -> bool {
    true
}

/// 应用过程中的一步。want/got 都用人话（应用失败时用户要能看懂差在哪）。
#[derive(serde::Serialize, Clone, Debug)]
pub struct ApplyStep {
    pub target: String,
    pub field: String,
    pub want: String,
    pub got: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// 应用结果。ok=false 且 rolled_back=true 表示已整体回滚到应用前状态。
#[derive(serde::Serialize, Clone, Debug)]
pub struct ApplyReport {
    pub ok: bool,
    pub rolled_back: bool,
    pub steps: Vec<ApplyStep>,
}

/// profiles.json 路径（与 state.json 同目录）
#[cfg(target_os = "windows")]
pub fn profiles_file() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
        .join("Screenguard")
        .join("profiles.json")
}

#[cfg(target_os = "macos")]
pub fn profiles_file() -> std::path::PathBuf {
    app_support_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp")).join("profiles.json")
}

/// 读档列表。文件损坏时不崩：把损坏文件改名 .bak 保留，返回空列表让应用继续可用。
pub fn load_profiles() -> Vec<Profile> {
    let path = profiles_file();
    let raw = match std::fs::read_to_string(&path) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    if raw.trim().is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Vec<Profile>>(&raw) {
        Ok(v) => v,
        Err(e) => {
            let bak = path.with_extension("json.bak");
            let _ = std::fs::rename(&path, &bak);
            eprintln!("profiles.json 损坏（{}），已备份为 {} 并按空列表继续", e, bak.display());
            Vec::new()
        }
    }
}

pub fn save_profiles(list: &[Profile]) -> Result<(), String> {
    let path = profiles_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("无法创建配置目录：{}", e))?;
    }
    let json = serde_json::to_string_pretty(list).map_err(|e| format!("序列化失败：{}", e))?;
    std::fs::write(&path, json).map_err(|e| format!("写入 profiles.json 失败：{}", e))
}

pub(crate) fn now_str() -> String {    #[cfg(target_os = "windows")]
    {
        // Windows 没有 Unix date；PowerShell 取本地时间（capture/apply 非热路径，可接受）
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-Date -Format 'yyyy-MM-dd HH:mm:ss'"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("date")
            .args(["+%Y-%m-%d %H:%M:%S"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
}

pub(crate) fn new_profile_id() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("p{}", ts)
}

pub fn profiles_list_impl() -> Vec<Profile> {
    load_profiles()
}

/// 捕获当前状态为新档（targets 由平台实现读取）
pub fn profile_capture_impl(name: &str) -> Result<Profile, String> {
    let targets = profile_capture_platform()?;
    let ts = now_str();
    let p = Profile {
        id: new_profile_id(),
        name: name.trim().to_string(),
        created_at: ts.clone(),
        updated_at: ts,
        targets,
        match_: None,
        auto: false,
    };
    let mut list = load_profiles();
    list.push(p.clone());
    save_profiles(&list)?;
    Ok(p)
}

/// 更新档位：改名 / 重捕获 targets（「更新为当前状态」）/ 开关自动触发标记 / 改匹配条件。
/// match_patch 用双层 Option 区分「没传」（外层 None，不动）与「传 null」（清空 match）。
pub fn profile_update_impl(
    id: &str,
    name: Option<&str>,
    recapture: bool,
    auto: Option<bool>,
    match_patch: Option<Option<ProfileMatch>>,
) -> Result<Profile, String> {
    let mut list = load_profiles();
    let idx = list
        .iter()
        .position(|p| p.id == id)
        .ok_or_else(|| format!("未找到场景档 {}", id))?;
    if let Some(n) = name {
        let n = n.trim();
        if !n.is_empty() {
            list[idx].name = n.to_string();
        }
    }
    if recapture {
        list[idx].targets = profile_capture_platform()?;
    }
    if let Some(a) = auto {
        list[idx].auto = a;
    }
    if let Some(m) = match_patch {
        list[idx].match_ = m;
    }
    list[idx].updated_at = now_str();
    save_profiles(&list)?;
    Ok(list[idx].clone())
}

pub fn profile_delete_impl(id: &str) -> Result<(), String> {
    let mut list = load_profiles();
    let before = list.len();
    list.retain(|p| p.id != id);
    if list.len() == before {
        return Err(format!("未找到场景档 {}", id));
    }
    save_profiles(&list)
}

pub fn profile_reorder_impl(ids: &[String]) -> Result<Vec<Profile>, String> {
    let mut list = load_profiles();
    let mut ordered: Vec<Profile> = Vec::with_capacity(list.len());
    for id in ids {
        if let Some(pos) = list.iter().position(|p| p.id == *id) {
            ordered.push(list.remove(pos));
        }
    }
    // ids 里没提到的档位保持在后面，不丢
    ordered.extend(list);
    save_profiles(&ordered)?;
    Ok(ordered)
}

pub fn profile_export_json_impl(id: &str) -> Result<String, String> {
    let list = load_profiles();
    let p = list.iter().find(|p| p.id == id).ok_or_else(|| format!("未找到场景档 {}", id))?;
    serde_json::to_string_pretty(p).map_err(|e| format!("序列化失败：{}", e))
}

/// 导入一个档（JSON 字符串）。id 冲突时重新生成；match/auto 字段容错缺失。
pub fn profile_import_impl(raw: &str) -> Result<Profile, String> {
    let mut p: Profile = serde_json::from_str(raw).map_err(|e| format!("JSON 解析失败：{}", e))?;
    let mut list = load_profiles();
    if list.iter().any(|x| x.id == p.id) {
        p.id = new_profile_id();
    }
    let ts = now_str();
    p.created_at = ts.clone();
    p.updated_at = ts;
    list.push(p.clone());
    save_profiles(&list)?;
    Ok(p)
}

/// 应用一个档（平台实现负责步骤与回滚）。按钮/热键都算用户手动操作：
/// 记一次手动变更时间，让随后 quiet_minutes 内的自动触发让路。
pub fn profile_apply_impl(id: &str) -> Result<ApplyReport, String> {
    note_manual_change();
    let list = load_profiles();
    let p = list.into_iter().find(|p| p.id == id).ok_or_else(|| format!("未找到场景档 {}", id))?;
    profile_apply_platform(&p)
}

/// 全局快捷键用：应用第 idx 个（0 起）档位；不存在时静默忽略（热键不该弹错）
pub fn profile_apply_by_index(idx: usize) {
    let list = load_profiles();
    if let Some(p) = list.get(idx) {
        let id = p.id.clone();
        let _ = profile_apply_impl(&id);
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn profile_capture_platform() -> Result<ProfileTargets, String> {
    profile_capture_impl_platform()
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) fn profile_apply_platform(p: &Profile) -> Result<ApplyReport, String> {
    profile_apply_impl_platform(p)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn profile_capture_platform() -> Result<ProfileTargets, String> {
    Err("当前平台暂不支持场景档".to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn profile_apply_platform(_p: &Profile) -> Result<ApplyReport, String> {
    Err("当前平台暂不支持场景档".to_string())
}

// ---------- 场景自动触发（P1-1：插屏/接电/时间段自动应用场景档） ----------

/// 三个触发源，各自可独立开关（总开关 enabled 关掉时全部失效）
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AutoSources {
    #[serde(default)]
    pub displays_changed: bool,
    #[serde(default)]
    pub on_ac: bool,
    #[serde(default)]
    pub time_range: bool,
}

impl Default for AutoSources {
    fn default() -> Self {
        // 默认只开最主要的显示器变化源；接电与时间段由用户显式打开
        AutoSources { displays_changed: true, on_ac: false, time_range: false }
    }
}

/// 最近一次自动应用记录（设置面板展示用）
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AutoLastApplied {
    pub profile_id: String,
    pub profile_name: String,
    pub at: String,
    pub reason: String,
    pub ok: bool,
}

/// 自动触发配置。paused_until_epoch 用 Unix 秒（进程内不引本地时区解析，
/// 剩余分钟由前端用 epoch 差值展示）。last_applied 在 auto_rules_set 时保留不覆盖。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AutoConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub sources: AutoSources,
    /// 用户手动改动后的静默分钟数（期间自动应用跳过）
    #[serde(default = "default_quiet_minutes")]
    pub quiet_minutes: u32,
    /// 暂停截止时间（Unix 秒）；None = 未暂停
    #[serde(default)]
    pub paused_until_epoch: Option<u64>,
    #[serde(default)]
    pub last_applied: Option<AutoLastApplied>,
}

fn default_quiet_minutes() -> u32 {
    5
}

impl Default for AutoConfig {
    fn default() -> Self {
        AutoConfig {
            enabled: false,
            sources: AutoSources::default(),
            quiet_minutes: default_quiet_minutes(),
            paused_until_epoch: None,
            last_applied: None,
        }
    }
}

fn auto_cfg_file() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::path::PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
            .join("Screenguard")
            .join("auto.json")
    }
    #[cfg(target_os = "macos")]
    {
        app_support_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
            .join("auto.json")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::path::PathBuf::from("/tmp/screenguard-auto.json")
    }
}

/// 读自动触发配置；文件缺失给默认（enabled=false），损坏时备份 .bak 后按默认继续
pub fn auto_cfg_load() -> AutoConfig {
    let path = auto_cfg_file();
    let raw = match std::fs::read_to_string(&path) {
        Ok(v) => v,
        Err(_) => return AutoConfig::default(),
    };
    if raw.trim().is_empty() {
        return AutoConfig::default();
    }
    match serde_json::from_str::<AutoConfig>(&raw) {
        Ok(c) => c,
        Err(e) => {
            let bak = path.with_extension("json.bak");
            let _ = std::fs::rename(&path, &bak);
            eprintln!("auto.json 损坏（{}），已备份为 {} 并按默认配置继续", e, bak.display());
            AutoConfig::default()
        }
    }
}

pub fn auto_cfg_save(cfg: &AutoConfig) -> Result<(), String> {
    let path = auto_cfg_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败：{}", e))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| format!("序列化失败：{}", e))?;
    std::fs::write(&path, json).map_err(|e| format!("写入 auto.json 失败：{}", e))
}

pub fn auto_rules_get_impl() -> AutoConfig {
    auto_cfg_load()
}

/// 保存用户在设置面板改的配置；last_applied 由引擎负责，这里保留旧值
pub fn auto_rules_set_impl(mut cfg: AutoConfig) -> Result<AutoConfig, String> {
    let old = auto_cfg_load();
    cfg.last_applied = old.last_applied;
    // 静默期至少 1 分钟，防止填 0 导致自动应用与用户操作互相打架
    cfg.quiet_minutes = cfg.quiet_minutes.max(1);
    auto_cfg_save(&cfg)?;
    Ok(cfg)
}

/// 暂停自动切换 minutes 分钟（0 = 立即恢复）
pub fn auto_pause_impl(minutes: u32) -> Result<AutoConfig, String> {
    let mut cfg = auto_cfg_load();
    if minutes == 0 {
        cfg.paused_until_epoch = None;
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        cfg.paused_until_epoch = Some(now + u64::from(minutes) * 60);
    }
    auto_cfg_save(&cfg)?;
    Ok(cfg)
}

/// "HH:MM-HH:MM" 是否包含当前时刻（hhmm = HH*100+MM）。支持跨零点；非法串一律 false。
pub fn time_in_range(range: &str, hhmm: u32) -> bool {
    let s = range.trim();
    let (a, b) = match s.split_once('-') {
        Some(v) => v,
        None => return false,
    };
    let parse = |t: &str| -> Option<u32> {
        let t = t.trim();
        let (h, m) = t.split_once(':')?;
        let h: u32 = h.trim().parse().ok()?;
        let m: u32 = m.trim().parse().ok()?;
        if h > 23 || m > 59 {
            return None;
        }
        Some(h * 100 + m)
    };
    let (lo, hi) = match (parse(a), parse(b)) {
        (Some(a), Some(b)) => (a, b),
        _ => return false,
    };
    if lo == hi {
        return false; // 零长度区间视为未配置
    }
    if lo < hi {
        hhmm >= lo && hhmm < hi
    } else {
        // 跨零点：22:00-06:00
        hhmm >= lo || hhmm < hi
    }
}

/// 档位匹配判定（纯函数，便于测试）：
/// - match 为 None 的档永不自动触发
/// - 档内屏幕键（edid_key 优先，captured_id 兜底）必须全部在当前屏幕键集合里；
///   require_all_displays=true 还要求集合完全一致（防「只接了部分屏也套全工位配置」）
/// - match.on_ac 与当前供电一致才命中；match.time_range 与当前时刻一致才命中
pub fn profile_match_hit(
    p: &Profile,
    current_keys: &[String],
    on_ac: Option<bool>,
    hhmm: Option<u32>,
) -> bool {
    let m = match p.match_.as_ref() {
        Some(m) => m,
        None => return false,
    };
    let want: Vec<String> = p
        .targets
        .displays
        .iter()
        .filter_map(|d| d.edid_key.clone().or_else(|| d.captured_id.clone()))
        .collect();
    if !want.is_empty() {
        if m.require_all_displays {
            if want.len() != current_keys.len() {
                return false;
            }
        }
        for k in &want {
            if !current_keys.iter().any(|c| c == k) {
                return false;
            }
        }
    }
    if let Some(ac) = m.on_ac {
        if on_ac != Some(ac) {
            return false;
        }
    }
    if let Some(tr) = m.time_range.as_deref() {
        if !tr.trim().is_empty() {
            match hhmm {
                Some(t) if time_in_range(tr, t) => {}
                _ => return false,
            }
        }
    }
    true
}

/// 从候选档里挑第一个命中的（列表顺序优先，不叠加）。
/// range_hits：刚进入时间段的档位 id（时间段触发不依赖全局触发源）。
pub fn auto_pick_profile(
    profiles: &[Profile],
    current_keys: &[String],
    on_ac: Option<bool>,
    hhmm: Option<u32>,
    trigger_reason: Option<&str>,
    range_hits: &[String],
) -> Option<(Profile, String)> {
    for p in profiles {
        if !p.auto {
            continue;
        }
        if range_hits.iter().any(|id| id == &p.id) {
            let tr = p
                .match_
                .as_ref()
                .and_then(|m| m.time_range.clone())
                .unwrap_or_default();
            return Some((p.clone(), format!("到达时间段 {}", tr)));
        }
        if let Some(reason) = trigger_reason {
            if profile_match_hit(p, current_keys, on_ac, hhmm) {
                return Some((p.clone(), reason.to_string()));
            }
        }
    }
    None
}

/// 手动变更标记：两平台共用语义——用户主动改配置后，quiet_minutes 内自动触发让路。
#[cfg(target_os = "windows")]
pub fn note_manual_change() {
    windows::mark_change();
}

#[cfg(target_os = "macos")]
pub fn note_manual_change() {
    macos::mark_change();
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn note_manual_change() {}

/// quiet_minutes 内是否发生过用户手动改动
#[cfg(target_os = "windows")]
pub fn manual_recently(secs: u64) -> bool {
    windows::recently_changed(secs)
}

#[cfg(target_os = "macos")]
pub fn manual_recently(secs: u64) -> bool {
    macos::recently_changed(secs)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn manual_recently(_secs: u64) -> bool {
    false
}

#[cfg(test)]
mod markdown_tests {
    use super::*;

    fn sample_report() -> HealthReport {
        HealthReport {
            generated_at: "2026-11-04 17:40:00".into(),
            platform: "windows".into(),
            app_version: "0.3.0".into(),
            os_version: "Windows 11 专业版 10.0.26100".into(),
            displays: vec![DisplayHealth {
                display_id: "dev-1".into(),
                name: "Mi Monitor".into(),
                native_resolution: Some("3840x2160".into()),
                current_resolution: "3840x2160".into(),
                max_hz: Some(60),
                current_hz: Some(60),
                ddc_capable: true,
                ddc_readable: false,
                color_profile: Some("sRGB IEC61966-2.1".into()),
                hdr: Some(false),
                connected: true,
                topology: None,
                checks: vec![HealthCheck {
                    id: "ddc_unreachable".into(),
                    severity: Severity::Warn,
                    title: "DDC/CI 通道读不到数据".into(),
                    detail: "亮度/音量控制依赖 DDC/CI，此通道不通时只能用手动按键。".into(),
                    evidence: vec![
                        "GetPhysicalMonitorsFromHMONITOR 返回句柄但 GetVCPFeature 失败".into(),
                        "设备路径：\\\\?\\DISPLAY#JKA27B3#5&2f3a4b5c&0&UID4353#{e6f07b5f-ee97-4a90-b076-ae13dcbbaa7d}".into(),
                        "配置文件 C:\\Users\\zhangsan\\AppData\\Local\\sc\\cache.json 读取失败".into(),
                    ],
                    fix_ids: vec!["reopen_ddc_channel".into()],
                }],
            }],
            portable: vec![],
            summary: HealthSummary {
                errors: 0,
                warnings: 1,
                infos: 0,
                headline: "1 块屏已连接，其中 1 项可优化".into(),
            },
        }
    }

    #[test]
    fn default_export_redacts_paths_and_omits_serial() {
        let md = render_health_markdown(&sample_report(), &[], false);
        assert!(md.contains("# 屏幕连接助手 · 显示体检报告"));
        assert!(md.contains("显示器 1：Mi Monitor"));
        // 隐私断言：用户名、内核路径不得出现；序列号行必须是「未输出」
        assert!(!md.contains("zhangsan"), "用户名泄漏：{}", md);
        assert!(!md.contains("DISPLAY#"), "设备路径泄漏：{}", md);
        assert!(!md.contains("e6f07b5f"), "设备路径泄漏：{}", md);
        assert!(!md.contains('\\'), "脱敏后不应残留反斜杠：{}", md);
        assert!(md.contains("未输出（不含设备序列号）"));
        assert!(md.contains("由 屏幕连接助手 v0.3.0 生成"));
    }

    #[test]
    fn include_serial_appends_serial_and_warning() {
        let serials = vec![("Mi Monitor".to_string(), "11731JKA00P7".to_string())];
        let md = render_health_markdown(&sample_report(), &serials, true);
        assert!(md.contains("包含设备序列号，请勿公开分享"));
        assert!(md.contains("11731JKA00P7"));
    }

    #[test]
    fn table_cells_escape_pipes() {
        let line = sanitize_line("a | b");
        assert_eq!(md_cell(&line), "a \\| b");
        // 换行不能破坏表格行
        assert_eq!(md_cell("x\ny"), "x y");
    }
}

#[cfg(test)]
mod portable_tests {
    use super::*;

    /// P1-4 验收：文案不得出现承诺性措辞
    const FORBIDDEN: [&str; 3] = ["提升亮度", "解锁", "增强亮度"];

    #[test]
    fn diagonal_inches_math() {
        // 34.5cm × 19.4cm ≈ 15.6 英寸便携屏
        let d = diagonal_inches(34.5, 19.4);
        assert!((d - 15.6).abs() < 0.1, "got {}", d);
        // 59.8cm × 33.6cm ≈ 27 英寸桌面屏（16:9）
        let d = diagonal_inches(59.8, 33.6);
        assert!((d - 27.0).abs() < 0.2, "got {}", d);
    }

    #[test]
    fn res_at_most_2k_boundaries() {
        assert!(res_at_most_2k(1920, 1080));
        assert!(res_at_most_2k(2560, 1440));
        assert!(res_at_most_2k(2560, 1600)); // 16:10 竖边超出但主边 2560
        assert!(res_at_most_2k(1440, 2560)); // 竖屏
        assert!(!res_at_most_2k(2561, 1440));
        assert!(!res_at_most_2k(3840, 2160));
        assert!(!res_at_most_2k(0, 0));
    }

    #[test]
    fn portable_reason_by_edid_size() {
        // 内建面板永远不算
        assert_eq!(portable_reason(true, Some(15.6), Some((1920, 1080))), None);
        // 15.6 英寸外接便携屏
        let r = portable_reason(false, Some(15.6), Some((1920, 1080))).unwrap();
        assert!(r.contains("15.6") && r.contains("英寸"));
        // 17.9 英寸临界值仍算便携
        assert!(portable_reason(false, Some(17.9), None).is_some());
        // 27 英寸桌面屏：有 EDID 尺寸就不走兜底，绝不误报
        assert_eq!(portable_reason(false, Some(27.0), Some((1920, 1080))), None);
        // EDID 尺寸缺失：≤2K 外接屏判为便携
        let r = portable_reason(false, None, Some((1920, 1080))).unwrap();
        assert!(r.contains("≤2K"));
        // EDID 尺寸缺失：4K 外接屏不判
        assert_eq!(portable_reason(false, None, Some((3840, 2160))), None);
        // 什么都没有：不猜
        assert_eq!(portable_reason(false, None, None), None);
    }

    #[test]
    fn power_budget_only_at_max_brightness() {
        // 亮度 80：不产生
        let c = build_portable_checks("EDID 15.6 英寸", Some(80), Some(100), None, None, None);
        assert!(!c.iter().any(|x| x.id == "portable_power_budget"));
        // 亮度 100：产生，evidence 同时给当前亮度与最大亮度
        let c = build_portable_checks("EDID 15.6 英寸", Some(100), Some(100), None, None, None);
        let p = c.iter().find(|x| x.id == "portable_power_budget").unwrap();
        assert_eq!(p.severity, Severity::Warn);
        assert!(p.detail.contains("供电不足"));
        assert!(p.evidence.iter().any(|e| e.contains("100")));
        // 面板标尺 0–255：软件 100 只在其 39%，如实标注
        let c = build_portable_checks("EDID 15.6 英寸", Some(100), Some(255), None, None, None);
        let p = c.iter().find(|x| x.id == "portable_power_budget").unwrap();
        assert!(p.evidence.iter().any(|e| e.contains("0–255") && e.contains("39%")));
    }

    #[test]
    fn usbc_and_hdr_checks() {
        // 能判断 USB-C 才发
        let c = build_portable_checks("r", Some(60), None, None, Some("连接类型 = DisplayPort".into()), None);
        assert!(c.iter().any(|x| x.id == "usbc_single_cable"));
        let c = build_portable_checks("r", Some(60), None, None, None, None);
        assert!(!c.iter().any(|x| x.id == "usbc_single_cable"));
        // HDR 开 → warn；HDR 关/未知 → 不发
        let c = build_portable_checks("r", Some(60), None, Some(true), None, None);
        assert!(c.iter().any(|x| x.id == "hdr_on_portable"));
        let c = build_portable_checks("r", Some(60), None, Some(false), None, None);
        assert!(!c.iter().any(|x| x.id == "hdr_on_portable"));
    }

    #[test]
    fn multi_bandwidth_check() {
        let lines = vec!["A：2560x1440@144Hz ≈ 13.8 Gbps".to_string(), "B：1920x1080@120Hz ≈ 6.4 Gbps".to_string()];
        let c = build_portable_checks("r", Some(60), None, None, None, Some((20.2, lines)));
        let m = c.iter().find(|x| x.id == "multi_display_bandwidth").unwrap();
        assert_eq!(m.severity, Severity::Warn);
        assert!(m.evidence.iter().any(|e| e.contains("20.2")));
        let c = build_portable_checks("r", Some(60), None, None, None, None);
        assert!(!c.iter().any(|x| x.id == "multi_display_bandwidth"));
    }

    #[test]
    fn section_advice_dedup_and_cap() {
        let s = build_portable_section("d1", "便携屏 16", "EDID 16.1 英寸", Some(100), Some(100), Some(true), true);
        // ≤5 条 + 无重复
        assert!(s.advice.len() <= 5, "advice 过多：{:?}", s.advice);
        let uniq: std::collections::HashSet<&String> = s.advice.iter().collect();
        assert_eq!(uniq.len(), s.advice.len());
        // HDR 开 → 关 HDR 建议排第一
        assert_eq!(s.advice[0], ADVICE_TURN_OFF_HDR);
        // 结论 + 边界数据都来自实测
        assert!(s.conclusion.contains("HDR"));
        assert!(s.evidence.iter().any(|e| e.contains("识别依据")));
        // 建议文案无承诺性措辞
        for a in &s.advice {
            for f in FORBIDDEN {
                assert!(!a.contains(f), "建议文案含承诺性措辞「{}」：{}", f, a);
            }
        }
    }
}

/// 最大公约数（宽高比约分用）
pub(crate) fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// 通用/未校准色彩配置名判定（两平台同名规则）
pub(crate) fn looks_generic_profile(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("generic") || n.contains("通用") || n.contains("sRGB")
}

/// 由支持模式列表推导原生分辨率与当前分辨率下的最高刷新率。
/// `modes`: (宽, 高, 刷新率)。原生 = 非缩放模式中面积最大者（退化取全部）。
/// 返回 (原生分辨率, 原生下最高 Hz, 当前分辨率下最高 Hz)。
pub(crate) fn infer_from_modes(
    modes: &[(u32, u32, u32)],
    cur_w: u32,
    cur_h: u32,
) -> (Option<String>, Option<u32>, Option<u32>) {
    if modes.is_empty() {
        return (None, None, None);
    }
    // 面积最大的模式视为原生分辨率（数字面板驱动总是把原生排进支持列表）
    let native = modes.iter().copied().max_by_key(|(w, h, _)| w * h);
    let native_res = native.map(|(w, h, _)| format!("{}x{}", w, h));
    let max_hz_native = native.map(|(_, _, hz)| hz);
    let max_hz_cur = modes
        .iter()
        .filter(|(w, h, _)| *w == cur_w && *h == cur_h)
        .map(|(_, _, hz)| *hz)
        .max();
    (native_res, max_hz_native, max_hz_cur)
}

/// 宽高比是否一致（约分后比较，容忍 1 像素级差异）
pub(crate) fn aspect_mismatch(cur: (u32, u32), native: (u32, u32)) -> bool {
    if cur.0 == 0 || cur.1 == 0 || native.0 == 0 || native.1 == 0 {
        return false;
    }
    let g1 = gcd(cur.0, cur.1);
    let g2 = gcd(native.0, native.1);
    (cur.0 / g1, cur.1 / g1) != (native.0 / g2, native.1 / g2)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn health_report_impl() -> Result<HealthReport, String> {
    Err("当前平台暂不支持体检报告".to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn repair_action_impl(
    _action: &str,
    _display_id: Option<&str>,
    _value: Option<&str>,
) -> Result<RepairOutcome, String> {
    Err("当前平台暂不支持修复动作".to_string())
}

// ---------- 软件层总亮度 / HDR / 前台窗口铺满 / 全局快捷键（非 Windows 存根） ----------

#[cfg(not(target_os = "windows"))]
pub fn gamma_get() -> String {
    String::new()
}

#[cfg(not(target_os = "windows"))]
pub fn gamma_set(_pct: u32) -> Result<String, String> {
    Err("总亮度控制仅支持 Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn hdr_states() -> String {
    String::new()
}

#[cfg(not(target_os = "windows"))]
pub fn hdr_set(_on: bool) -> Result<String, String> {
    Err("HDR 同步仅支持 Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn match_dpi_apply() -> Result<String, String> {
    Err("跨屏缩放对齐仅支持 Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn match_dpi_restore() -> Result<String, String> {
    Err("跨屏缩放对齐仅支持 Windows".to_string())
}

/// 音频输出端点（Windows 可枚举/切换；macOS 暴露系统默认端点）
#[derive(Serialize, Clone, Debug)]
pub struct AudioEndpoint {
    pub id: String,
    pub name: String,
    pub volume: u32,
    pub mute: bool,
    pub adjustable: bool,
    pub form_factor: u32,
    /// 是否是当前用户选定要控制的那一个
    pub selected: bool,
    /// 是否是 Windows 当前的默认输出设备（用户没明确选时优先控制它）
    pub is_default: bool,
}

#[cfg(not(target_os = "windows"))]
pub fn hdr_set_dev(_dev: &str, _on: bool) -> Result<String, String> {
    Err("HDR 控制仅支持 Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn mitv_volume_get() -> Result<u32, String> {
    Err("小米显示器音量控制仅支持 Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn mitv_volume_set(_target: u32) -> Result<u32, String> {
    Err("小米显示器音量控制仅支持 Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn mitv_available() -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
pub fn primary_work_area() -> Result<(i32, i32, i32, i32), String> {
    Err("仅支持 Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn standby_state() -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
pub fn diag_scan() -> Result<String, String> {
    Err("一键检测仅支持 Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn diag_fix() -> Result<String, String> {
    Err("一键修复仅支持 Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn recently_changed(_secs: u64) -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
pub fn open_log_dir() -> Result<(), String> {
    Err("仅支持 Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn diag_log_path() -> String {
    String::new()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn align_report() -> Result<String, String> {
    Err("当前平台暂不支持双屏对齐体检".to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn align_apply() -> Result<String, String> {
    Err("当前平台暂不支持应用双屏对齐组合".to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn align_restore_modes() -> Result<String, String> {
    Err("当前平台暂不支持还原双屏模式".to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn get_display_modes(_display_id: &str) -> Result<Vec<DisplayMode>, String> {
    Err("当前平台暂不支持读取显示模式".to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn set_display_mode(_display_id: &str, _mode: u32) -> Result<String, String> {
    Err("当前平台暂不支持切换显示模式".to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn display_action(
    _action: &str,
    _display_id: &str,
    _target_id: Option<&str>,
    _value: Option<&str>,
) -> Result<String, String> {
    Err("此显示控制目前仅支持 macOS".to_string())
}

// ---------- 屏幕电源控制 ----------
#[cfg(not(target_os = "macos"))]
pub fn display_sleep_states() -> Vec<DisplaySleepState> {
    Vec::new()
}

// ---------- 显示器健康：KVM 切换 / EDID 重新协商异常检测与修复（非 Windows 存根） ----------

#[cfg(not(target_os = "windows"))]
pub fn health_events(_minutes: u32) -> String {
    String::new()
}

#[cfg(not(target_os = "windows"))]
pub fn mode_detail() -> String {
    String::new()
}

#[cfg(not(target_os = "windows"))]
pub fn repair_display(_level: u32) -> Result<String, String> {
    Err("显示器健康修复暂仅支持 Windows".to_string())
}

// ---------- 方案B：ADB 联网精细控制（可选增强能力） ----------

/// ADB 能力状态：是否找到 adb、已连接哪些设备、给用户的提示
#[derive(serde::Serialize, Clone)]
pub struct AdbStatus {
    pub available: bool,
    pub path: String,
    pub devices: Vec<String>,
    pub tip: String,
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "windows")]
pub use windows::*;

// ===== 迁移用偏好读写（P1-3：只碰通用开关，不碰设备相关状态）=====

/// 「退出时自动还原」开关当前值
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn pref_revert_on_exit() -> bool {
    #[cfg(target_os = "windows")]
    {
        windows::app_state().revert_on_exit
    }
    #[cfg(target_os = "macos")]
    {
        macos::mac_app_state().revert_on_exit
    }
}

/// 写「退出时自动还原」开关
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn pref_set_revert_on_exit(v: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let mut st = windows::app_state();
        st.revert_on_exit = v;
        windows::save_app_state(&st)
    }
    #[cfg(target_os = "macos")]
    {
        let mut st = macos::mac_app_state();
        st.revert_on_exit = v;
        macos::save_mac_app_state(&st)
    }
}

// ===== 软 KVM 跨平台分发（P1-2）=====
// kvm.rs 只调这组 kvm_* 函数，不直接碰平台模块，避免 cfg 分支散落。

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn kvm_device_name() -> String {
    device_name()
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn kvm_device_name() -> String {
    "未知设备".to_string()
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn kvm_clip_get() -> Option<String> {
    clipboard_get_text()
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn kvm_clip_get() -> Option<String> {
    None
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn kvm_clip_set(text: &str) -> Result<(), String> {
    clipboard_set_text(text)
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn kvm_clip_set(_text: &str) -> Result<(), String> {
    Err("当前平台不支持剪贴板".to_string())
}

/// 把系统默认输出切到指定端点（软 KVM 接管侧用）
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn kvm_audio_set_default(id: &str) -> Result<(), String> {
    audio_set_default(id)
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn kvm_audio_set_default(_id: &str) -> Result<(), String> {
    Err("当前平台不支持音频切换".to_string())
}

/// 音频输出端点候选：接管侧要挑「显示器自带音频」；发起侧要挑「本机扬声器」
/// 返回 (id, name, form_factor, is_default)；form_factor==9 视为 HDMI/DP 音频（显示器）
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn kvm_audio_endpoints() -> Result<Vec<(String, String, u32, bool)>, String> {
    #[cfg(target_os = "windows")]
    {
        let list = audio_endpoints()?;
        Ok(list
            .into_iter()
            .map(|e| (e.id, e.name, e.form_factor, e.is_default))
            .collect())
    }
    #[cfg(target_os = "macos")]
    {
        Ok(audio_endpoints_list()?
            .into_iter()
            .map(|(id, name, is_def)| (id, name, 0u32, is_def))
            .collect())
    }
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn kvm_audio_endpoints() -> Result<Vec<(String, String, u32, bool)>, String> {
    Err("当前平台不支持音频端点列表".to_string())
}

/// 本机执行 DDC 切输入源：优先主屏且支持 DDC 的，其次任一 DDC 屏，最后主屏。
/// 返回（是否真发出 DDC 写入、人类可读结果）。
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn kvm_switch_input(source: &str) -> (bool, String) {
    let src_key = match source.trim().to_lowercase().as_str() {
        "dp" | "displayport" | "15" => "dp",
        "hdmi1" | "17" => "hdmi1",
        "hdmi2" | "18" => "hdmi2",
        "usbc" | "usb-c" | "usb_c" | "27" => "usbc",
        _ => source.trim(),
    };
    let displays = {
        let v = get_displays();
        if v.is_empty() {
            return (false, "未枚举到显示器，无法切换输入源".to_string());
        }
        v
    };
    let pick = displays
        .iter()
        .find(|d| d.main && d.ddc)
        .or_else(|| displays.iter().find(|d| d.ddc))
        .or_else(|| displays.iter().find(|d| d.main))
        .unwrap_or(&displays[0]);
    match repair_action_impl("switch_input", Some(&pick.id), Some(src_key)) {
        Ok(o) => match o.result {
            RepairResult::Applied => (true, o.message),
            RepairResult::NotApplicable => (false, o.message),
            _ => (false, o.message),
        },
        Err(e) => (false, e),
    }
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn kvm_switch_input(_source: &str) -> (bool, String) {
    (false, "当前平台不支持输入源切换".to_string())
}

#[cfg(test)]
mod auto_tests {
    use super::*;

    fn prof(name: &str, edid: Option<&str>, m: Option<ProfileMatch>) -> Profile {
        Profile {
            id: format!("p-{}", name),
            name: name.to_string(),
            created_at: "2026-11-04 10:00:00".into(),
            updated_at: "2026-11-04 10:00:00".into(),
            targets: ProfileTargets {
                displays: vec![DisplayTarget {
                    edid_key: edid.map(|s| s.to_string()),
                    captured_id: None,
                    ..Default::default()
                }],
                ..Default::default()
            },
            match_: m,
            auto: true,
        }
    }

    fn m_all() -> ProfileMatch {
        ProfileMatch { require_all_displays: true, docking: None, on_ac: None, time_range: None }
    }

    #[test]
    fn time_range_parses_and_handles_midnight_wrap() {
        assert!(time_in_range("09:00-19:00", 900));
        assert!(time_in_range("09:00-19:00", 1859));
        assert!(!time_in_range("09:00-19:00", 1900)); // 右开区间
        assert!(!time_in_range("09:00-19:00", 858));
        assert!(time_in_range("22:00-06:00", 2330)); // 跨零点
        assert!(time_in_range("22:00-06:00", 300));
        assert!(!time_in_range("22:00-06:00", 1200));
        assert!(!time_in_range("abc", 1200));
        assert!(!time_in_range("25:00-26:00", 2530));
        assert!(!time_in_range("09:00-09:00", 900)); // 零长度
    }

    #[test]
    fn match_none_never_triggers() {
        let p = prof("x", Some("JKA27B3"), None);
        assert!(!profile_match_hit(&p, &["JKA27B3".to_string()], Some(true), None));
    }

    #[test]
    fn require_all_needs_exact_set() {
        let mut p = prof("work", Some("JKA27B3"), Some(m_all()));
        p.targets.displays.push(DisplayTarget {
            edid_key: Some("DEL1234".into()),
            captured_id: None,
            ..Default::default()
        });
        // 只接了档内一块屏 → require_all 拒绝
        assert!(!profile_match_hit(&p, &["JKA27B3".to_string()], None, None));
        // 两块都在 → 命中
        assert!(profile_match_hit(&p, &["JKA27B3".to_string(), "DEL1234".to_string()], None, None));
        // 多接了一块 → 集合不一致，拒绝
        assert!(!profile_match_hit(
            &p,
            &["JKA27B3".to_string(), "DEL1234".to_string(), "HP5678".to_string()],
            None,
            None
        ));
    }

    #[test]
    fn subset_mode_allows_extra_displays() {
        let mut m = m_all();
        m.require_all_displays = false;
        let p = prof("main", Some("JKA27B3"), Some(m));
        assert!(profile_match_hit(&p, &["JKA27B3".to_string(), "HP5678".to_string()], None, None));
        assert!(!profile_match_hit(&p, &["HP5678".to_string()], None, None));
    }

    #[test]
    fn on_ac_and_time_conditions_must_hold() {
        let mut m = m_all();
        m.on_ac = Some(false);
        let p = prof("mobile", None, Some(m));
        assert!(!profile_match_hit(&p, &[], Some(true), None));
        assert!(profile_match_hit(&p, &[], Some(false), None));
        assert!(!profile_match_hit(&p, &[], None, None)); // 未知供电不算命中

        let mut m2 = m_all();
        m2.time_range = Some("09:00-19:00".into());
        let p2 = prof("day", None, Some(m2));
        assert!(profile_match_hit(&p2, &[], None, Some(1000)));
        assert!(!profile_match_hit(&p2, &[], None, Some(2000)));
        assert!(!profile_match_hit(&p2, &[], None, None));
    }

    #[test]
    fn pick_prefers_list_order_and_range_hits() {
        let mut first = prof("a", Some("JKA27B3"), Some(m_all()));
        first.auto = true;
        let mut second = prof("b", None, Some(m_all()));
        second.auto = false; // 关了 auto 的档绝不触发
        let third = prof("c", None, Some(m_all()));
        let list = vec![second, third, first.clone()];
        // 列表靠前者优先：c 在 b（未启用）后，但 a 匹配也排后面——按列表顺序找第一个 auto 且命中的
        let (hit, reason) = auto_pick_profile(
            &list,
            &["JKA27B3".to_string()],
            None,
            None,
            Some("显示器连接变化"),
            &[],
        )
        .expect("应命中 c");
        assert_eq!(hit.id, "p-c");
        assert_eq!(reason, "显示器连接变化");
        // 时间段命中：即使没有全局触发
        let mut with_range = prof("d", None, Some(m_all()));
        with_range.match_.as_mut().unwrap().time_range = Some("09:00-19:00".into());
        let list2 = vec![with_range];
        let (hit2, reason2) = auto_pick_profile(&list2, &[], None, Some(900), None, &["p-d".to_string()])
            .expect("时间段应命中");
        assert_eq!(hit2.id, "p-d");
        assert!(reason2.contains("时间段"));
    }

    #[test]
    fn disabled_profile_never_triggers() {
        let mut p = prof("off", Some("JKA27B3"), Some(m_all()));
        p.auto = false;
        assert!(auto_pick_profile(
            &[p],
            &["JKA27B3".to_string()],
            None,
            None,
            Some("显示器连接变化"),
            &[]
        )
        .is_none());
    }
}
