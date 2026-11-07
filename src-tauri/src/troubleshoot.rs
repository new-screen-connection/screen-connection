//! P2-1 排障助手：本地规则引擎（第一、二层）。
//!
//! 分层顺序不能反：
//! ① 纯本地意图识别（关键词 + 轻量规则）→ 8 个场景；
//! ② 确定性执行：直接吃本次 health_report，把检查项映射成「原因卡片」（推测原因 +
//!    体检证据数值 + 修复动作 id），大多数问题在这层解决，不联网；
//! ③ 可选 LLM：仅在本层无法定位时由前端提示，用户同意后才调 `agent_llm_chat`。
//!
//! 铁律：对话里引用的所有数值都来自本次体检报告；定位不了就诚实说「体检未发现异常」，
//! 给可操作建议（重新插拔/换线/换口），绝不编原因。

use crate::platform::{health_report_impl, DisplayHealth, HealthCheck, HealthReport, Severity};
use serde::Serialize;

// ---------- 意图识别（第一层，纯本地） ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scene {
    /// 屏幕不亮 / 黑屏
    BlackScreen,
    /// 副屏不识别 / 少屏
    NotDetected,
    /// 分辨率不对 / 字糊
    ResBlur,
    /// 刷新率上不去
    RefreshLow,
    /// 亮度调不了
    BrightnessStuck,
    /// 颜色不对
    ColorWrong,
    /// 切换电脑后异常
    AfterSwitch,
    /// 其他（自由问答 / 转可选 LLM）
    Other,
}

impl Scene {
    pub fn as_str(self) -> &'static str {
        match self {
            Scene::BlackScreen => "black_screen",
            Scene::NotDetected => "not_detected",
            Scene::ResBlur => "res_blur",
            Scene::RefreshLow => "refresh_low",
            Scene::BrightnessStuck => "brightness_stuck",
            Scene::ColorWrong => "color_wrong",
            Scene::AfterSwitch => "after_switch",
            Scene::Other => "other",
        }
    }
}

fn has_any(t: &str, keys: &[&str]) -> bool {
    keys.iter().any(|k| t.contains(k))
}

/// 意图识别。关键词有限且明确，识别不了就归 Other（宁可交给第二层诚实兜底，不硬猜）。
pub fn classify(text: &str) -> Scene {
    let t = text.trim().to_lowercase();

    // 具体投诉优先（这些词不太会和黑屏/不识别混淆）
    if t.contains("亮度") && has_any(&t, &["调不了", "不能调", "没反应", "无效", "失灵", "不起作用", "不生效", "不管用", "不工作", "锁死"]) {
        return Scene::BrightnessStuck;
    }
    if has_any(&t, &["偏色", "偏黄", "偏蓝", "偏绿", "偏红", "发黄", "发蓝", "发绿", "色差", "色准", "泛白", "颜色"])
        || t.contains("色彩")
    {
        return Scene::ColorWrong;
    }
    if t.contains("刷新率") || t.contains("hz") || t.contains("赫兹") {
        return Scene::RefreshLow;
    }
    if has_any(&t, &["分辨率", "字糊", "模糊", "发虚", "锯齿", "拉伸", "变形", "比例不对", "好糊", "很糊"]) {
        return Scene::ResBlur;
    }

    // 黑屏 / 不识别（顺序：黑屏在前——「副屏又黑了」不该被识别场景截走）
    if has_any(&t, &["黑屏", "不亮", "黑了", "点不亮", "没信号", "无信号", "画面没了", "一片黑", "全黑"]) {
        return Scene::BlackScreen;
    }
    if has_any(&t, &[
        "不识别", "识别不到", "识别不出", "检测不到", "检测不出", "找不到", "认不到", "没认出", "掉屏", "掉了",
        "消失", "不见了", "没出现", "少了一个屏", "少了一块屏", "少个屏", "少块屏", "没接上", "连接不上", "连不上",
    ]) {
        return Scene::NotDetected;
    }
    if has_any(&t, &["切换", "切回", "切过去", "换了电脑", "换电脑", "另一台", "对端", "kvm", "接回来"]) {
        return Scene::AfterSwitch;
    }
    Scene::Other
}

// ---------- 原因卡片（第二层，确定性执行） ----------

/// 一个可能原因：推测原因 + 来自本次体检的证据数值 + 可执行的修复动作。
#[derive(Serialize, Clone, Debug)]
pub struct DiagCause {
    pub title: String,
    /// 支撑证据：全部摘自体检报告的具体数值/状态，不是模型生成
    pub evidence: Vec<String>,
    /// 可接入的修复动作 id（空 = 只能手动处理，manual 里有操作建议）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub fix_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub display_id: Option<String>,
    /// 手动操作建议（每条一句话，无修复按钮时展示）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub manual: Vec<String>,
}

/// 排障回答：一句结论 + 原因卡片列表 + 可操作建议。前端按卡片渲染，不出大段文字。
#[derive(Serialize, Clone, Debug)]
pub struct DiagReply {
    pub scene: String,
    /// 一句结论，如「找到 2 个可能原因」
    pub conclusion: String,
    pub causes: Vec<DiagCause>,
    /// 定位不了时的诚实回答（None = 已给出原因卡片）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub healthy_note: Option<String>,
    /// 可操作建议（重新插拔、换线、换口等）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub advice: Vec<String>,
    /// 底部「以上都不对？」入口：展开完整体检报告
    pub show_report: bool,
}

fn manual_replug() -> Vec<String> {
    vec![
        "重新插拔视频线（显示器和电脑两端都要）".into(),
        "换一条视频线或换一个接口再试".into(),
    ]
}

fn cause_from_check(d: &DisplayHealth, c: &HealthCheck) -> DiagCause {
    DiagCause {
        title: c.title.clone(),
        evidence: c.evidence.clone(),
        fix_ids: c.fix_ids.clone(),
        display_id: Some(d.display_id.clone()),
        manual: Vec::new(),
    }
}

/// 外接屏判定（用于「切输入源」这类只对外接屏有意义的动作）：有拓扑且链路上有 cable/hub/port 跳，
/// 或干脆没有内建标记可用时一律按外接处理（宁可多给一个切源按钮，也不漏）。
fn is_external(d: &DisplayHealth) -> bool {
    d.connected
}

fn all_checks<'a>(report: &'a HealthReport) -> Vec<(&'a DisplayHealth, &'a HealthCheck)> {
    report
        .displays
        .iter()
        .flat_map(|d| d.checks.iter().map(move |c| (d, c)))
        .collect()
}

fn warn_or_error<'a>(report: &'a HealthReport) -> Vec<(&'a DisplayHealth, &'a HealthCheck)> {
    all_checks(report)
        .into_iter()
        .filter(|(_, c)| matches!(c.severity, Severity::Warn | Severity::Error))
        .collect()
}

fn reply(scene: Scene, conclusion: String, causes: Vec<DiagCause>, healthy_note: Option<String>, advice: Vec<String>) -> DiagReply {
    DiagReply {
        scene: scene.as_str().to_string(),
        conclusion,
        causes,
        healthy_note,
        advice,
        show_report: true,
    }
}

/// 第二层：把体检报告映射成原因卡片。所有数值都来自 report 本身。
pub fn diag(scene: Scene, report: &HealthReport) -> DiagReply {
    let n_total = report.displays.len();
    let active: Vec<&DisplayHealth> = report.displays.iter().filter(|d| d.connected).collect();
    let n_active = active.len();
    let inactive: Vec<&DisplayHealth> = report.displays.iter().filter(|d| !d.connected).collect();

    match scene {
        // ---------- 屏幕不亮 / 黑屏 ----------
        Scene::BlackScreen => {
            let mut causes = Vec::new();
            for d in &inactive {
                causes.push(DiagCause {
                    title: format!("「{}」当前未点亮", d.name),
                    evidence: vec![format!(
                        "体检：{} 台显示器中有 {} 台未处于活动状态，该屏在系统里已登记但拿不到画面",
                        n_total,
                        inactive.len()
                    )],
                    fix_ids: vec!["rescan_displays".into(), "force_redetect".into()],
                    display_id: Some(d.display_id.clone()),
                    manual: vec![
                        "确认显示器电源已打开".into(),
                        "确认显示器输入源与所接接口一致（用显示器 OSD 菜单查看）".into(),
                    ],
                });
            }
            if causes.is_empty() && n_active > 0 {
                causes.push(DiagCause {
                    title: "输入源可能切错（画面亮着但不是你要的那一路）".into(),
                    evidence: active
                        .iter()
                        .map(|d| format!("{}：{} @ {}", d.name, d.current_resolution, d.current_hz.map(|v| format!("{}Hz", v)).unwrap_or_else(|| "?Hz".into())))
                        .collect(),
                    fix_ids: Vec::new(),
                    display_id: None,
                    manual: vec![
                        "用显示器 OSD 菜单确认当前输入源".into(),
                        "在「体检」页对对应屏幕用「切换输入源」修复动作试切".into(),
                    ],
                });
                causes.push(DiagCause {
                    title: "背光/对比度可能被调到 0（机器没坏，只是画面全黑）".into(),
                    evidence: vec!["体检报告不包含背光数值，此项需用屏幕物理按键确认".into()],
                    fix_ids: Vec::new(),
                    display_id: None,
                    manual: vec!["用屏幕物理按键进 OSD 菜单检查背光/对比度".into()],
                });
            }
            if causes.is_empty() {
                return reply(
                    scene,
                    "体检未发现异常".into(),
                    Vec::new(),
                    Some(format!(
                        "体检显示 {} 台显示器全部处于点亮状态，系统层面没有能解释「黑屏」的痕迹。",
                        n_active
                    )),
                    {
                        let mut a = manual_replug();
                        a.push("断电 10 秒再接回显示器电源线".into());
                        a
                    },
                );
            }
            reply(
                scene,
                format!("找到 {} 个可能原因", causes.len()),
                causes,
                None,
                manual_replug(),
            )
        }

        // ---------- 副屏不识别 ----------
        Scene::NotDetected => {
            let mut causes = Vec::new();
            for d in &inactive {
                causes.push(DiagCause {
                    title: format!("「{}」被系统记住但当前未响应", d.name),
                    evidence: vec![
                        "体检：该屏在系统显示列表中有登记（线缆身份识别曾经成功）".into(),
                        "当前状态：未连接 / 拿不到画面".into(),
                    ],
                    fix_ids: vec!["rescan_displays".into(), "force_redetect".into()],
                    display_id: Some(d.display_id.clone()),
                    manual: Vec::new(),
                });
            }
            if causes.is_empty() {
                return reply(
                    scene,
                    "体检未发现异常".into(),
                    Vec::new(),
                    Some(format!(
                        "体检检测到 {} 台显示器且全部活动，系统层面没有「少屏」。问题多半在物理连接上。",
                        n_active
                    )),
                    {
                        let mut a = manual_replug();
                        a.push("若是扩展坞 / 转接头接入，改为直连试一次（坞是掉屏高发环节）".into());
                        a.push("便携屏：确认供电，一线通供电不足会导致整屏掉线".into());
                        a
                    },
                );
            }
            reply(
                scene,
                format!("找到 {} 个可能原因", causes.len()),
                causes,
                None,
                manual_replug(),
            )
        }

        // ---------- 分辨率不对 / 字糊 ----------
        Scene::ResBlur => {
            let mut causes = Vec::new();
            for (d, c) in all_checks(report) {
                if c.id == "resolution_not_native" {
                    let mut cause = cause_from_check(d, c);
                    cause.title = format!("「{}」在用非原生分辨率，发虚/拉伸多半是它", d.name);
                    cause.manual = vec![format!(
                        "到系统显示设置把分辨率切回原生（报告里已列出原生值）；或在本应用「画质」页直接切换"
                    )];
                    causes.push(cause);
                }
            }
            if causes.is_empty() {
                // 都在原生分辨率 → 字糊常见于信号握手 / 缩放设置，给一个软握手修复按钮
                for d in active.iter().filter(|d| is_external(d)) {
                    causes.push(DiagCause {
                        title: format!("「{}」分辨率已是原生，可能是信号握手不完整", d.name),
                        evidence: vec![format!(
                            "体检：当前 {}x{} 即面板原生分辨率",
                            d.current_resolution,
                            d.native_resolution.clone().unwrap_or_default()
                        )],
                        fix_ids: vec!["reapply_mode".into()],
                        display_id: Some(d.display_id.clone()),
                        manual: Vec::new(),
                    });
                }
                let empty = causes.is_empty();
                return reply(
                    scene,
                    if empty { "体检未发现异常".to_string() } else { "分辨率都在原生值，试试信号软握手".to_string() },
                    causes,
                    if empty {
                        Some("体检显示各屏均在原生分辨率下运行，系统层面没有能解释「字糊」的问题。".into())
                    } else {
                        None
                    },
                    vec![
                        "把 Windows 缩放（DPI）设为 100% 或系统推荐值试一次".into(),
                        "字糊也可能来自显示器自身锐化设置（OSD 菜单里的 Sharpness）".into(),
                    ],
                );
            }
            reply(
                scene,
                format!("找到 {} 个可能原因", causes.len()),
                causes,
                None,
                Vec::new(),
            )
        }

        // ---------- 刷新率上不去 ----------
        Scene::RefreshLow => {
            let mut causes = Vec::new();
            for (d, c) in all_checks(report) {
                if c.id == "refresh_below_max" {
                    let mut cause = cause_from_check(d, c);
                    cause.title = format!("「{}」刷新率低于该分辨率下支持的最高值", d.name);
                    cause.manual = vec![
                        "线材带宽不足是常见原因：换 DP 线或 HDMI 2.0 以上规格的线".into(),
                        "确认线材后在系统显示设置把刷新率调到最高档".into(),
                    ];
                    causes.push(cause);
                }
                if c.id == "multi_display_bandwidth" {
                    causes.push(cause_from_check(d, c));
                }
            }
            if causes.is_empty() {
                return reply(
                    scene,
                    "体检未发现异常".into(),
                    Vec::new(),
                    Some(format!(
                        "体检显示当前刷新率已是各屏支持的最高值（{}）。",
                        active
                            .iter()
                            .map(|d| format!("{} @ {}", d.name, d.current_hz.map(|v| format!("{}Hz", v)).unwrap_or_else(|| "?".into())))
                            .collect::<Vec<_>>()
                            .join("，")
                    )),
                    vec![
                        "确认系统显示设置的刷新率下拉框里有没有更高档位（没有 = 线材/面板不支持）".into(),
                        "部分显示器需要在 OSD 菜单里开启「高刷新率/Overclock」选项".into(),
                    ],
                );
            }
            reply(
                scene,
                format!("找到 {} 个可能原因", causes.len()),
                causes,
                None,
                Vec::new(),
            )
        }

        // ---------- 亮度调不了 ----------
        Scene::BrightnessStuck => {
            let mut causes = Vec::new();
            for (d, c) in all_checks(report) {
                if c.id == "ddc_unreachable" {
                    let mut cause = cause_from_check(d, c);
                    cause.title = format!("「{}」的 DDC/CI 通道不可用，软件调不了它的亮度", d.name);
                    cause.manual = vec!["到显示器 OSD 菜单确认 DDC/CI 选项已开启（部分机型默认关闭）".into()];
                    causes.push(cause);
                }
                if c.id == "hdr_brightness_conflict" {
                    let mut cause = cause_from_check(d, c);
                    cause.title = format!("「{}」开着 HDR：系统接管背光，亮度滑块经常无效", d.name);
                    causes.push(cause);
                }
            }
            if causes.is_empty() {
                return reply(
                    scene,
                    "体检未发现异常".into(),
                    Vec::new(),
                    Some("体检显示各屏亮度通道正常（DDC/CI 可读）。可能只是刚才一次调节没有生效。".into()),
                    vec![
                        "用本应用「画质」页的亮度滑块再调一次观察".into(),
                        "部分商用机型在 OSD 里有「Child Lock / 锁定」会禁用外部亮度控制".into(),
                    ],
                );
            }
            reply(
                scene,
                format!("找到 {} 个可能原因", causes.len()),
                causes,
                None,
                Vec::new(),
            )
        }

        // ---------- 颜色不对 ----------
        Scene::ColorWrong => {
            let mut causes = Vec::new();
            for (d, c) in all_checks(report) {
                if c.id == "color_profile_generic" {
                    let mut cause = cause_from_check(d, c);
                    cause.title = format!("「{}」用的是系统通用色彩配置（未校准）", d.name);
                    cause.manual = vec!["对色彩要求高时，用「画质 → 色彩同步」把这块屏对齐到标准色域".into()];
                    causes.push(cause);
                }
                if c.id == "hdr_brightness_conflict" {
                    causes.push(cause_from_check(d, c));
                }
            }
            if causes.is_empty() {
                let hdr_line = active
                    .iter()
                    .map(|d| format!("{} HDR={}", d.name, d.hdr.map(|v| if v { "开" } else { "关" }).unwrap_or("读不到")))
                    .collect::<Vec<_>>()
                    .join("，");
                return reply(
                    scene,
                    "体检未发现异常".into(),
                    Vec::new(),
                    Some(format!("体检显示各屏色彩配置均已校准、HDR 状态：{}。", hdr_line)),
                    vec![
                        "先确认是哪块屏偏色：让两屏显示同一张纯白/纯灰图片对比".into(),
                        "偏色屏在 OSD 菜单里把色温设为「标准 / sRGB」".into(),
                        "导出体检报告，比对两屏的色彩配置差异".into(),
                    ],
                );
            }
            reply(
                scene,
                format!("找到 {} 个可能原因", causes.len()),
                causes,
                None,
                Vec::new(),
            )
        }

        // ---------- 切换电脑后异常 ----------
        Scene::AfterSwitch => {
            let mut causes = Vec::new();
            for d in active.iter().filter(|d| is_external(d)) {
                causes.push(DiagCause {
                    title: format!("「{}」输入源可能停在上一个主机的档位", d.name),
                    evidence: vec![format!(
                        "体检：该屏当前点亮（{} @ {}），但切换主机后显示器输入源经常需要重选",
                        d.current_resolution,
                        d.current_hz.map(|v| format!("{}Hz", v)).unwrap_or_else(|| "?Hz".into())
                    )],
                    fix_ids: vec!["switch_input".into()],
                    display_id: Some(d.display_id.clone()),
                    manual: vec!["也可以用显示器 OSD 菜单手动选输入源".into()],
                });
            }
            for (d, c) in all_checks(report) {
                if c.id == "ddc_unreachable" {
                    causes.push(cause_from_check(d, c)); // KVM/坞半转发状态
                }
            }
            if causes.is_empty() {
                return reply(
                    scene,
                    "体检未发现异常".into(),
                    Vec::new(),
                    Some(format!(
                        "体检显示当前 {} 台屏全部正常点亮，切换后的链路是通的。",
                        n_active
                    )),
                    vec![
                        "若画面仍异常，先试「切换输入源」修复动作".into(),
                        "把常用布局存成「场景档」，切换回来后一键恢复".into(),
                    ],
                );
            }
            reply(
                scene,
                format!("找到 {} 个可能原因", causes.len()),
                causes,
                None,
                manual_replug(),
            )
        }

        // ---------- 其他：拿 warn/error 项兜底，绝不编 ----------
        Scene::Other => {
            let issues = warn_or_error(report);
            if issues.is_empty() {
                return reply(
                    scene,
                    "体检未发现异常".into(),
                    Vec::new(),
                    Some("体检未发现异常。你描述的问题没有在系统层面留下可检测的痕迹。".into()),
                    {
                        let mut a = manual_replug();
                        a.push("告诉我更具体的时机（比如「拔掉扩展坞之后」「开机十分钟内」）有助于缩小范围".into());
                        a
                    },
                );
            }
            let causes: Vec<DiagCause> = issues
                .into_iter()
                .map(|(d, c)| cause_from_check(d, c))
                .collect();
            reply(
                scene,
                format!("体检发现 {} 项可能相关的问题", causes.len()),
                causes,
                None,
                Vec::new(),
            )
        }
    }
}

/// 便捷入口：意图识别 + 跑体检 + 生成回答（一条命令完成第一、二层）。
pub fn troubleshoot_diag_impl(text: &str) -> Result<DiagReply, String> {
    let report = health_report_impl()?;
    if report.displays.is_empty() {
        // 瞬时读不到显示器（WMI/DDC 抖动）时如实报错，绝不输出"体检未发现异常"造成误导
        return Err("这次体检没有读到任何显示器信息，请稍等几秒重试".to_string());
    }
    let scene = classify(text);
    Ok(diag(scene, &report))
}

// ---------- 可选 LLM（第三层，默认关闭，用户同意后才发） ----------

/// 组装将要发送的 JSON（脱敏：体检报告本身不含序列号；再剥去逐屏 display_id 与拓扑细节，
/// 只保留与排障相关的字段）。`agent_llm_chat` 发送的就是这份，保证「所见即所发」。
pub fn build_llm_payload(text: &str, report: &HealthReport) -> serde_json::Value {
    let displays: Vec<serde_json::Value> = report
        .displays
        .iter()
        .map(|d| {
            serde_json::json!({
                "name": d.name,
                "native_resolution": d.native_resolution,
                "current_resolution": d.current_resolution,
                "max_hz": d.max_hz,
                "current_hz": d.current_hz,
                "ddc_capable": d.ddc_capable,
                "ddc_readable": d.ddc_readable,
                "color_profile": d.color_profile,
                "hdr": d.hdr,
                "connected": d.connected,
                "checks": d.checks.iter().map(|c| serde_json::json!({
                    "id": c.id, "severity": c.severity, "title": c.title,
                    "detail": c.detail, "evidence": c.evidence,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "question": text,
        "platform": report.platform,
        "os_version": report.os_version,
        "app_version": report.app_version,
        "displays": displays,
    })
}

/// 记录「是否联网分析过」——只在日志目录追加一行时间戳，不记录任何内容（隐私铁律）。
fn log_llm_used() {
    #[cfg(target_os = "windows")]
    let dir = match std::env::var("APPDATA") {
        Ok(v) => std::path::PathBuf::from(v).join("Screenguard").join("logs"),
        Err(_) => return,
    };
    #[cfg(target_os = "macos")]
    let dir = match crate::platform::app_support_dir() {
        Ok(d) => d.join("logs"),
        Err(_) => return,
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let dir = std::env::temp_dir().join("screenguard-logs");
    if dir.exists() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("agent_net.log"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, format!("llm_used {}\n", ts).as_bytes()));
    }
}

/// 第三层：调用 LLM（DeepSeek 兼容 chat/completions）。仅在用户已同意发送后调用。
/// `api_key` 由前端传入（存在用户本机，不入库不上报）。失败返回可读错误。
pub fn agent_llm_chat_impl(text: &str, api_key: &str) -> Result<String, String> {
    if api_key.trim().is_empty() {
        return Err("未配置 API Key：请在「工具 → AI 助手」里勾选并填写".to_string());
    }
    let report = health_report_impl()?;
    let payload = build_llm_payload(text, &report);

    let body = serde_json::json!({
        // 2026-11 实测：deepseek-chat 已下线（models 列表只剩 deepseek-flash /
        // deepseek-v4-pro），旧模型名会得到 internal_error
        "model": "deepseek-flash",
        "messages": [
            {
                "role": "system",
                "content": "你是一个显示器排障助手。只回答与显示器/显示连接相关的问题，其他问题一律回复「我只管屏幕这件事」。基于给出的体检报告回答，引用报告里的具体数值；报告之外的数值不许编造。回答控制在 200 字以内，给出可操作步骤。"
            },
            {
                "role": "user",
                "content": serde_json::to_string(&payload).map_err(|e| e.to_string())?
            }
        ],
        "temperature": 0.3,
        // deepseek-flash 是带思维链的模型（reasoning_content 先耗 token），
        // 500 会被思考吃光导致 content 为空；给到 2000 才有正文
        "max_tokens": 2000,
        "stream": false
    });

    let resp = ureq::post("https://api.deepseek.com/chat/completions")
        .set("Authorization", &format!("Bearer {}", api_key.trim()))
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(30))
        .send_string(&body.to_string())
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("LLM 服务返回错误码 {}", code),
            _ => format!("联网分析请求失败：{}（请检查网络）", e),
        })?;

    log_llm_used();

    let v: serde_json::Value = resp.into_json().map_err(|e| format!("LLM 响应解析失败：{}", e))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if content.is_empty() {
        return Err("LLM 返回了空回复".to_string());
    }
    Ok(content)
}

// ---------- 单测 ----------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{HealthSummary, Topology};

    fn bare_display(id: &str, name: &str, connected: bool) -> DisplayHealth {
        DisplayHealth {
            display_id: id.into(),
            name: name.into(),
            native_resolution: Some("2560x1440".into()),
            current_resolution: "2560x1440".into(),
            max_hz: Some(144),
            current_hz: Some(60),
            ddc_capable: true,
            ddc_readable: true,
            color_profile: Some("sRGB".into()),
            hdr: Some(false),
            connected,
            checks: Vec::new(),
            topology: None,
        }
    }

    fn report_of(displays: Vec<DisplayHealth>) -> HealthReport {
        HealthReport {
            generated_at: "t".into(),
            platform: "windows".into(),
            app_version: "0.3.0".into(),
            os_version: "test".into(),
            displays,
            portable: Vec::new(),
            summary: HealthSummary {
                errors: 0,
                warnings: 0,
                infos: 0,
                headline: String::new(),
            },
        }
    }

    #[test]
    fn classify_covers_all_scenes() {
        assert_eq!(classify("副屏又黑了"), Scene::BlackScreen);
        assert_eq!(classify("屏幕不亮了"), Scene::BlackScreen);
        assert_eq!(classify("显示器的电源打开了吗？怎么没信号"), Scene::BlackScreen);
        assert_eq!(classify("副屏不识别"), Scene::NotDetected);
        assert_eq!(classify("第二块屏检测不到"), Scene::NotDetected);
        assert_eq!(classify("字看得很糊"), Scene::ResBlur);
        assert_eq!(classify("为什么只有 60Hz"), Scene::RefreshLow);
        assert_eq!(classify("刷新率上不去"), Scene::RefreshLow);
        assert_eq!(classify("亮度调不了"), Scene::BrightnessStuck);
        assert_eq!(classify("亮度没反应"), Scene::BrightnessStuck);
        assert_eq!(classify("屏幕颜色发黄"), Scene::ColorWrong);
        assert_eq!(classify("两块屏偏色不一致"), Scene::ColorWrong);
        assert_eq!(classify("切换电脑后副屏就不对了"), Scene::AfterSwitch);
        assert_eq!(classify("KVM 切过去之后画面不对"), Scene::AfterSwitch);
        assert_eq!(classify("今天天气怎么样"), Scene::Other);
        assert_eq!(classify(""), Scene::Other);
    }

    #[test]
    fn black_screen_maps_inactive_display() {
        let mut d = bare_display("dev-2", "便携副屏", false);
        d.checks.push(HealthCheck {
            id: "display_not_active".into(),
            severity: Severity::Error,
            title: "未点亮".into(),
            detail: "".into(),
            evidence: vec!["状态 = 未连接".into()],
            fix_ids: vec!["rescan_displays".into()],
        });
        let r = report_of(vec![bare_display("dev-1", "主屏", true), d]);
        let reply = diag(Scene::BlackScreen, &r);
        assert_eq!(reply.causes.len(), 1);
        assert!(reply.causes[0].title.contains("便携副屏"));
        assert!(reply.causes[0].fix_ids.contains(&"rescan_displays".to_string()));
        assert!(reply.conclusion.contains("1 个可能原因"));
    }

    #[test]
    fn black_screen_all_active_gives_actionable_causes_not_fake() {
        let r = report_of(vec![bare_display("dev-1", "主屏", true)]);
        let reply = diag(Scene::BlackScreen, &r);
        // 全部点亮时：要么诚实说未发现异常，要么给的是「输入源/背光」这类可核实原因，
        // 且证据里不得出现体检报告之外的编造数值
        assert!(
            !reply.causes.is_empty() || reply.healthy_note.is_some(),
            "全亮时也应给出输入源/背光等可核实原因或诚实兜底"
        );
        for c in &reply.causes {
            for e in &c.evidence {
                assert!(!e.contains("亮度"), "报告里没有背光数值，不得引用");
            }
        }
    }

    #[test]
    fn brightness_maps_ddc_issue() {
        let mut d = bare_display("dev-2", "Mi Monitor", true);
        d.ddc_readable = false;
        d.checks.push(HealthCheck {
            id: "ddc_unreachable".into(),
            severity: Severity::Warn,
            title: "DDC/CI 通道不可达".into(),
            detail: "".into(),
            evidence: vec!["DDCProbe = 0".into()],
            fix_ids: vec!["reopen_ddc_channel".into(), "ddc_power_cycle".into()],
        });
        let r = report_of(vec![d]);
        let reply = diag(Scene::BrightnessStuck, &r);
        assert_eq!(reply.causes.len(), 1);
        assert!(reply.causes[0].fix_ids.contains(&"reopen_ddc_channel".to_string()));
        assert!(reply.causes[0].evidence.iter().any(|e| e.contains("DDCProbe")));
    }

    #[test]
    fn refresh_maps_below_max() {
        let mut d = bare_display("dev-2", "副屏", true);
        d.checks.push(HealthCheck {
            id: "refresh_below_max".into(),
            severity: Severity::Info,
            title: "刷新率低于该分辨率下支持的最高值".into(),
            detail: "".into(),
            evidence: vec!["当前 2560x1440 @ 60Hz".into(), "该分辨率下支持最高 144Hz".into()],
            fix_ids: vec![],
        });
        let r = report_of(vec![d]);
        let reply = diag(Scene::RefreshLow, &r);
        assert_eq!(reply.causes.len(), 1);
        assert!(reply.causes[0].evidence.iter().any(|e| e.contains("144Hz")));
        // 刷新率检查无软件修复动作 → 手动建议里提到线材
        assert!(reply.causes[0].manual.iter().any(|m| m.contains("线")));
    }

    #[test]
    fn other_with_clean_report_is_honest_no_fabrication() {
        let r = report_of(vec![bare_display("dev-1", "主屏", true)]);
        let reply = diag(Scene::Other, &r);
        assert!(reply.causes.is_empty());
        assert!(reply.healthy_note.as_deref().unwrap().contains("未发现异常"));
        assert!(!reply.advice.is_empty());
        assert!(reply.show_report);
    }

    #[test]
    fn other_with_issues_surfaces_them() {
        let mut d = bare_display("dev-1", "主屏", true);
        d.checks.push(HealthCheck {
            id: "resolution_not_native".into(),
            severity: Severity::Warn,
            title: "当前分辨率不是面板原生分辨率".into(),
            detail: "".into(),
            evidence: vec!["当前 1920x1080".into()],
            fix_ids: vec!["rescan_displays".into()],
        });
        let r = report_of(vec![d]);
        let reply = diag(Scene::Other, &r);
        assert_eq!(reply.causes.len(), 1);
        assert!(reply.conclusion.contains("1 项"));
    }

    #[test]
    fn llm_payload_strips_ids_and_topology() {
        let mut d = bare_display("dev-1", "主屏", true);
        d.topology = Some(Topology {
            hops: Vec::new(),
            note: Some("直连".into()),
        });
        let r = report_of(vec![d]);
        let payload = build_llm_payload("副屏黑了", &r);
        let s = serde_json::to_string(&payload).unwrap();
        assert!(!s.contains("display_id"), "payload 不得包含 display_id");
        assert!(!s.contains("topology"), "payload 不得包含拓扑细节");
        assert!(s.contains("question"));
        assert!(s.contains("副屏黑了"));
    }

    #[test]
    fn diag_reply_serializes_without_empty_noise() {
        let r = report_of(vec![bare_display("dev-1", "主屏", true)]);
        let reply = diag(Scene::Other, &r);
        let s = serde_json::to_string(&reply).unwrap();
        assert!(!s.contains("healthy_note\":null"), "skip_serializing 应去掉 null 字段");
    }
}
