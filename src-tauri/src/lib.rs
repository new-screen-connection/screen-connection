mod platform;

pub mod updater;
#[cfg(any(target_os = "windows", test))]
mod icc_profile;
pub mod kvm;
pub mod migrate;
pub mod troubleshoot;

use platform::DisplayInfo;

// ============================================================================
// 关于「为什么这些命令都是 async + spawn_blocking」
// ----------------------------------------------------------------------------
// Tauri 的**同步**命令是跑在**主线程**上的。而本应用几乎每个平台调用都要起一个
// PowerShell / adb 子进程：一次 DDC 读写约 0.5 秒，蓝牙电池查询更久。同步写法的后果
// 就是——拖一下亮度滑块，界面卡半秒；点一下按钮，界面卡半秒；联动每 2.5 秒轮询一次，
// 界面每隔 2.5 秒顿一下。
//
// 统一改成 async + tauri::async_runtime::spawn_blocking 之后，阻塞工作落到专用线程池，
// 主线程（UI）不再被占用。**新增平台命令时请沿用这个模式**，除非它真的只是瞬时的内存操作。
// 例外：窗口/托盘相关的命令（open_main / quit_app / hide_panel）必须留在主线程，保持同步。
// ============================================================================

/// 返回当前平台名
#[tauri::command]
fn get_platform() -> &'static str {
    platform::platform_name()
}

/// 返回 App 版本号（与 Cargo.toml / tauri.conf.json 一致）
#[tauri::command]
fn get_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
fn open_external_page(page: String) -> Result<(), String> {
    platform::open_external_page(&page)
}

/// 开机自启是否已启用
#[tauri::command]
async fn get_autostart() -> bool {
    tauri::async_runtime::spawn_blocking(platform::autostart_enabled)
        .await
        .unwrap_or(false)
}

/// 设置开机自启
#[tauri::command]
async fn set_autostart(enabled: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || platform::set_autostart(enabled))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

#[tauri::command]
async fn get_displays() -> Vec<DisplayInfo> {
    tauri::async_runtime::spawn_blocking(platform::get_displays)
        .await
        .unwrap_or_default()
}

/// 获取单屏可用分辨率与刷新率（macOS displayplacer）。
#[tauri::command]
async fn get_display_modes(display_id: String) -> Result<Vec<platform::DisplayMode>, String> {
    tauri::async_runtime::spawn_blocking(move || platform::get_display_modes(&display_id))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 显示模式 / 缩放变化后强制重设主窗口尺寸并居中主屏。
/// WebView2 的毛病：屏幕分辨率或 DPI 缩放变了之后，窗口物理尺寸与页面渲染
/// 缩放不同步 —— 内容按旧 DPI 排版，右半边被裁掉（实测：铺满双屏「先对齐两屏
/// 清晰度」改了分辨率，回来主窗口内容就溢出，用户截图反馈）。凡是会改显示
/// 模式/缩放的命令结束后都调一次，把窗口拉回默认 780x760（屏幕太小则夹进屏内）。
/// B8 收敛式：尺寸已经对就不再动；位置不强制定位居中（保留用户摆放的位置），
/// 只有窗口跑出主屏可视区才拉回屏内 —— 每次都无条件 set_size+居中会与 DPI
/// 迟到传播互搏，多条归一化序列并行时窗口被反复搬动 = 桌面上跳动。
/// 归一化目标物理尺寸：780x760 逻辑按当前 DPI 折算，目标屏太小则夹进屏内。
/// normalize_main_window 与视口守卫（window_geometry/resync_main_window）共用，
/// 保证两处期望值永远一致。
fn norm_target_phys(w: &tauri::WebviewWindow, scale: f64) -> (i32, i32) {
    let (sw, sh) = match w.primary_monitor() {
        Ok(Some(m)) => (m.size().width as i32, m.size().height as i32),
        _ => (0, 0),
    };
    let mut ww = (780.0 * scale) as i32;
    let mut wh = (760.0 * scale) as i32;
    if sw > 0 {
        ww = ww.min(sw - 24).max(420);
        wh = wh.min(sh - 24).max(520);
    }
    (ww, wh)
}

/// B23：窗口所在显示的**实时** DPI 缩放。
///
/// 不能只信 `w.scale_factor()`：显示模式被 mode=2 的自动对齐改过（主屏切到
/// 2560x1440@100%）之后，Tauri 缓存的缩放可能停在中途值，而 Windows 早已回到
/// 150%。此时「物理宽 ÷ 缓存缩放」会把窗口的 CSS 宽算大，所有归一化/守卫判定
/// 都跟着错。全部改用 Win32 实时值（拿不到才回退 Tauri）。
fn live_scale_of(w: &tauri::WebviewWindow) -> f64 {
    let fallback = w.scale_factor().unwrap_or(1.0).max(0.2);
    #[cfg(target_os = "windows")]
    {
        let h = w.hwnd().map(|h| h.0 as isize).unwrap_or(0);
        let s = platform::dpi_scale_for(h);
        if s > 0.2 {
            s
        } else {
            fallback
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        fallback
    }
}

/// 缩放错位的起始时刻。B17：错位本来是「显示模式变更中的过渡态」，跳过是对的；
/// 但实测某些驱动/KVM 链路在「对齐→回滚」后会**长期**保持 mon 与 win 缩放不一致，
/// 此时守卫与归一化会永久 SKIP —— 宽态定格、自愈失效（用户反馈的「点了铺满就变宽
/// 再也回不去」）。因此错位持续超过 6 秒就认定它不是过渡态，改以窗口自身 DPI 为准
/// 强制收口；一旦两边一致立即清零。
static SCALE_MISMATCH_SINCE: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

fn scale_mismatch_persistent(mon_scale: f64, win_scale: f64) -> bool {
    let mism = (mon_scale - win_scale).abs() > 0.01;
    let mut g = match SCALE_MISMATCH_SINCE.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    if !mism {
        *g = None;
        return false;
    }
    match *g {
        None => {
            *g = Some(std::time::Instant::now());
            false
        }
        Some(t) => t.elapsed().as_millis() > 6000,
    }
}

/// 主窗口几何快照：铺满/还原这类会改显示模式的操作开始前记下，
/// 失败（含被拦截）时原样写回 —— 用户明确要求「没法铺满时主界面窗口不要变」。
#[derive(Clone, Copy)]
struct WinSnap {
    /// 逻辑（CSS）尺寸。B19：物理值在 DPI 往返期间不可靠 —— 快照 1174 物理写回
    /// 时若窗口 DPI 已从 1.5 掉到 1.0，就变成 1174 CSS（用户看到的「变宽」）。
    /// 记逻辑尺寸，写回时再乘当前 DPI，才是「尺寸不变」的真正语义。
    css: (f64, f64),
    pos: Option<tauri::PhysicalPosition<i32>>,
    maximized: bool,
}

fn snapshot_main_window(app: &tauri::AppHandle) -> Option<WinSnap> {
    use tauri::Manager;
    let w = app.get_webview_window("main")?;
    let size = w.inner_size().ok()?;
    let scale = live_scale_of(&w).max(0.5);
    Some(WinSnap {
        css: (size.width as f64 / scale, size.height as f64 / scale),
        pos: w.outer_position().ok(),
        maximized: w.is_maximized().unwrap_or(false),
    })
}

/// 把主窗口写回快照（最多 3 次、间隔递增）：显示模式的 DPI 传播会在操作返回后
/// 继续几百毫秒~数秒，一次写回可能被随后到达的 WM_DPICHANGED 覆盖掉。
fn restore_main_window(app: &tauri::AppHandle, snap: Option<WinSnap>) {
    let Some(snap) = snap else { return };
    use tauri::Manager;
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    if snap.maximized {
        return; // 最大化是用户意图，不干预
    }
    for (i, wait) in [0u64, 1200, 3500].iter().enumerate() {
        if *wait > 0 {
            std::thread::sleep(std::time::Duration::from_millis(*wait));
        }
        // B19：按逻辑尺寸判定/写回（物理值在 DPI 抖动期间不可靠，见 WinSnap 注释）
        let scale = live_scale_of(&w).max(0.5);
        let cur = w.inner_size().unwrap_or_default();
        let css_w = cur.width as f64 / scale;
        let css_h = cur.height as f64 / scale;
        if (css_w - snap.css.0).abs() > 2.0 || (css_h - snap.css.1).abs() > 2.0 {
            let tw = (snap.css.0 * scale).round().max(1.0) as u32;
            let th = (snap.css.1 * scale).round().max(1.0) as u32;
            eprintln!(
                "[span-guard] RESTORE#{} css={:.0}x{:.0}→{:.0}x{:.0} (phys {}x{} scale={scale:.2})",
                i, css_w, css_h, snap.css.0, snap.css.1, cur.width, cur.height
            );
            let _ = w.set_size(tauri::PhysicalSize::new(tw, th));
        }
        if let (Some(p), Ok(cp)) = (snap.pos, w.outer_position()) {
            if (cp.x - p.x).abs() > 2 || (cp.y - p.y).abs() > 2 {
                let _ = w.set_position(p);
            }
        }
    }
}

/// 「刚刚发生过显示相关的操作」时间戳。看门狗只在这个窗口期（90s）内接管窗口
/// 尺寸 —— 显示模式/DPI 传播只会在这段时间里把窗口写坏；平时用户自己拖动改变
/// 大小属于有意为之，不该每 3 秒被强行拉回。
static LAST_DISPLAY_EVENT: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

fn mark_display_event() {
    if let Ok(mut g) = LAST_DISPLAY_EVENT.lock() {
        *g = Some(std::time::Instant::now());
    }
}

fn display_event_active() -> bool {
    LAST_DISPLAY_EVENT
        .lock()
        .ok()
        .and_then(|g| *g)
        .map(|t| t.elapsed().as_secs() < 90)
        .unwrap_or(false)
}

/// B18 窗口尺寸常驻看门狗（后端侧，1.5s 一轮）。
/// 铺满/对齐/色彩同步这类操作会在几百毫秒到十几秒里反复触发 DPI 传播，任何
/// 一次 set_size 都可能被随后的 WM_DPICHANGED 覆盖成错误宽度；前端守卫（B15）
/// 依赖 JS 定时器与视口读数，视口本身失真时就无从判断。窗口客户区的物理尺寸
/// 是后端唯一权威事实，这里连续两轮（≈3s）偏离目标就强制归一化 —— 不管是谁、
/// 什么操作把窗口写宽/写窄，都会被拉回 780x760，且不依赖前端是否还活着。
fn spawn_main_size_watchdog(app: tauri::AppHandle) {
    use tauri::Manager;
    std::thread::spawn(move || {
        let mut bad = 0u32;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let Some(w) = app.get_webview_window("main") else {
                continue;
            };
            if !w.is_visible().unwrap_or(false)
                || w.is_maximized().unwrap_or(false)
                || w.is_fullscreen().unwrap_or(false)
                || !display_event_active()
            {
                bad = 0;
                continue;
            }
            let Ok(cur) = w.inner_size() else {
                continue;
            };
            let scale = live_scale_of(&w);
            let (tw, th) = norm_target_phys(&w, scale);
            // B19：同样按 CSS 逻辑尺寸判定（理由见 normalize_main_window）。
            // 物理值在 DPI 往返期间会被放大/缩小，拿它跟目标比会把「尺寸其实正常」
            // 的窗口反复写一遍，反而写坏。
            let css_w = cur.width as f64 / scale;
            let css_h = cur.height as f64 / scale;
            let ok_w = (css_w - tw as f64 / scale).abs() <= 12.0;
            let ok_h = (css_h - th as f64 / scale).abs() <= 40.0;
            if ok_w && ok_h {
                bad = 0;
                continue;
            }
            bad += 1;
            if bad < 2 {
                continue;
            }
            bad = 0;
            eprintln!(
                "[watch] 窗口被写成 {}x{}（css {:.0}x{:.0}，目标 {tw}x{th} phys）→ 强制归一化",
                cur.width, cur.height, css_w, css_h
            );
            normalize_main_window(&app);
        }
    });
}

fn restore_main_window_async(app: tauri::AppHandle, snap: Option<WinSnap>) {
    mark_display_event();
    std::thread::spawn(move || restore_main_window(&app, snap));
}

fn normalize_main_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    let Ok(Some(mon)) = w.primary_monitor() else {
        return;
    };
    let pos = mon.position();
    let size = mon.size();
    let scale = mon.scale_factor();
    // B23：窗口自身 DPI 用 Win32 实时值（Tauri 缓存值可能在显示模式改过后失真）
    let win_scale = live_scale_of(&w).max(0.2);
    // B12 交叉校验：主屏报告的缩放与窗口自身 DPI 上下文不一致 = 显示模式/DPI
    // 正在抖动（实测：对齐改分辨率瞬间 mon_scale=1.00 而 win_scale=1.50，还原
    // 时反向错位）。此时算出的目标尺寸是过渡值，set_size 会把窗口写死成错误
    // 宽度（用户看到的「一点功能就变宽」）——本轮直接放弃，交给序列后续轮次
    // 等传播结束再收口。
    // B17：但错位若持续 >6s 就不是过渡态了（驱动不广播归位），此时以窗口自身
    // DPI 为权威强制收口，避免宽态定格。
    if (scale - win_scale).abs() > 0.01 {
        if !scale_mismatch_persistent(scale, win_scale) {
            eprintln!("[norm] SKIP scale mismatch mon={scale:.2} win={win_scale:.2}");
            return;
        }
        eprintln!("[norm] FORCE mon={scale:.2} win={win_scale:.2} → 以窗口 DPI 收口");
    }
    // 错位持续时通常 mon 报的是过渡值，窗口自身的 win_scale 才是当前真实 DPI
    let scale = win_scale;
    let (ww, wh) = norm_target_phys(&w, scale);
    // B12 度量口径修正：set_size 设的是内容（客户区）尺寸，必须和 inner_size
    // 比；旧代码拿 outer_size（含边框标题栏，比内容大 16~56px）比，永远差一截
    // → 每次归一化都误判「尺寸不对」无条件 set_size，把 DPI 抖动放大成可见跳动。
    let cur = w.inner_size().unwrap_or_default();
    // B19 度量口径：以 **CSS 逻辑尺寸** 为准。DPI 正在抖动时同一块窗口的物理值
    // 会被放大/缩小，按物理值比对就会反复 set_size，而且每次都在错的缩放下写死
    // 一个错误的宽度 —— 日志里的 `2079x760` / `2130x760` / `3115x1143` 全是这么
    // 来的，正是用户看到的「点一下功能窗口就变宽」。布局是否正常只取决于 CSS
    // 尺寸，物理值只是它的宿主表示，所以先折算再比对；需要纠正时才按当前 scale
    // 换算成物理值写回去。
    let css_w = cur.width as f64 / scale;
    let css_h = cur.height as f64 / scale;
    let want_w = ww as f64 / scale;
    let want_h = wh as f64 / scale;
    // 宽度容差 6 CSS px（真被写坏时偏差都是几百 px）；高度容差 40 CSS px —— 用户
    // 可以手动拖高窗口，这里只纠正被显示模式写坏的高度，不跟手动调整较劲。
    if (css_w - want_w).abs() > 6.0 || (css_h - want_h).abs() > 40.0 {
        eprintln!(
            "[norm] SET_SIZE {ww}x{wh} css={css_w:.0}x{css_h:.0}→{want_w:.0}x{want_h:.0} (phys was {}x{}, scale={scale:.2})",
            cur.width, cur.height
        );
        let _ = w.set_size(tauri::PhysicalSize::new(ww.max(1) as u32, wh.max(1) as u32));
    }
    // 位置：仅当窗口基本不在主屏可视区时才拉回（居中），否则保留用户摆的位置
    let cw = cur.width as i32;
    let ch = cur.height as i32;
    if cw > 0 && ch > 0 {
        let p = w.outer_position().unwrap_or_default();
        let in_x = p.x + cw > pos.x + 40 && p.x < pos.x + size.width as i32 - 40;
        let in_y = p.y + ch > pos.y + 40 && p.y < pos.y + size.height as i32 - 40;
        if !(in_x && in_y) {
            let x = pos.x + ((size.width as i32 - ww) / 2).max(0);
            let y = pos.y + ((size.height as i32 - wh) / 2).max(0);
            let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
        }
    }
}

/// 立即归一化 + 延迟复归一化。
/// Windows 改显示模式/DPI 后，变更以 WM_DPICHANGED / WM_DISPLAYCHANGE 的形式
/// 在之后几百毫秒里陆续传播到 WebView2 —— 只在命令返回瞬间归一化一次，会被
/// 随后到达的 DPI 传播按旧缩放重新排版覆盖掉（用户反馈：还原窗口后主窗口
/// 仍然错乱）。因此再补 3 次延迟归一化，等传播彻底结束后收口。
/// B8 序列去重：多条序列并行时互相覆盖（旧序列按旧 DPI 搬窗口）——每条序列
/// 带代号，一旦有新序列进来，旧序列立即退出，只有最新一条在跑。
static NORM_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn normalize_main_window_settled(app: &tauri::AppHandle) {
    mark_display_event();
    normalize_main_window(app);
    let gen = NORM_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let app2 = app.clone();
    std::thread::spawn(move || {
        // B15：收口点延长到 12s —— 还原路径（对齐回滚）在本机 KVM 链路上实测
        // DPI/显示模式传播可超过 5s，旧序列跑完后再没人纠正窗口，宽态定格。
        // 多出的轮次在已收敛时是纯读比较（零动作），代价可忽略。
        for ms in [250u64, 700, 1600, 3000, 5000, 8000, 12000] {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            if NORM_GEN.load(std::sync::atomic::Ordering::Relaxed) != gen {
                return; // 更新的归一化序列已接管，本序列退出，不再搬窗口
            }
            normalize_main_window(&app2);
        }
    });
}

/// 主窗口几何快照（B15 视口守卫用）。
/// 前端拿 expect_css_w 与自己的 window.innerWidth 比较：
///   一致 → 窗口正常；偏差 >8px → 要么物理尺寸被写错，要么 WebView2 的
///   devicePixelRatio 与宿主失同步（内容「变宽/溢出」的直接来源），调 resync_main_window 修。
/// 缩放交叉校验不过（显示模式抖动中）时返回 suspicious=true，守卫本轮跳过，
/// 避免把 DPI 传播的合法过渡态当成异常去搬窗口。
#[tauri::command]
fn window_geometry(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    use tauri::Manager;
    let Some(w) = app.get_webview_window("main") else {
        return Err("主窗口不存在".into());
    };
    let inner = w.inner_size().map_err(|e| e.to_string())?;
    let scale = w.scale_factor().map_err(|e| e.to_string())?;
    // B23：Tauri 的 scale_factor 在显示模式被改过之后可能停在中途值（mode=2 的
    // 自动对齐会把主屏切到 2560x1440@100%，再切回来时缓存未必刷新）。用 Win32
    // GetDpiForWindow 拿实时值做权威口径 —— 否则「物理宽 ÷ 缓存缩放」把 CSS 宽
    // 算大，页面按大宽度排版，右边被窗口边缘裁掉。
    let live_scale = live_scale_of(&w);
    let mon_scale = w
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(scale);
    let (tw, th) = norm_target_phys(&w, live_scale);
    let maximized = w.is_maximized().unwrap_or(false);
    Ok(serde_json::json!({
        "phys_w": inner.width,
        "phys_h": inner.height,
        "scale": scale,
        "dpi_scale": live_scale,
        "expect_css_w": tw as f64 / live_scale,
        "expect_css_h": th as f64 / live_scale,
        // B17：错位只在「过渡态」时让守卫让路；持续 >6s 说明归位广播不会再来，
        // 不再算 suspicious —— 否则守卫会永久跳过，宽态再也修不回来。
        "suspicious": (mon_scale - live_scale).abs() > 0.01
            && !scale_mismatch_persistent(mon_scale, live_scale),
        "maximized": maximized,
    }))
}

/// 视口守卫的修复端（B15）。前端检测到 CSS 视口漂移后带实测值调用：
/// ① 物理尺寸与期望差 >4px → 按期望 set_size（与 normalize 同口径）；
/// ② 物理尺寸正确但 CSS 漂移 → WebView2 渲染缩放失同步，做一次 ±2 物理像素
///    的尺寸抖动强制重排（WM_SIZE 让合成器按宿主 DPI 重查）；
/// ③ attempt>=3（连抖两轮都没修好）→ 返回 reload，让前端自恢复——重载后
///    渲染器进程按宿主 DPI 重新取值，是失同步的最终解。
#[tauri::command]
async fn resync_main_window(
    app: tauri::AppHandle,
    css_width: f64,
    attempt: Option<u32>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let Some(w) = app.get_webview_window("main") else {
            return Err("主窗口不存在".into());
        };
        let scale = w.scale_factor().map_err(|e| e.to_string())?;
        // B23：以 Win32 实时 DPI 为准（Tauri 的缓存值在显示模式改过后可能停在旧值，
        // 那会把「物理宽 ÷ 缩放」算大 → 判定口径全错 → 越修越偏）。
        let live = live_scale_of(&w);
        let inner = w.inner_size().map_err(|e| e.to_string())?;
        let (tw, th) = norm_target_phys(&w, live);
        // B19：按 CSS 逻辑尺寸判定（物理值在 DPI 抖动期间会被放大/缩小，拿它比对
        // 会在错的缩放下写出错误宽度）。宽度 3 CSS px、高度 40 CSS px —— 高度不跟
        // 用户手动拖高较劲。
        let css_now_w = inner.width as f64 / live;
        let css_now_h = inner.height as f64 / live;
        let want_css_w = tw as f64 / live;
        let want_css_h = th as f64 / live;
        #[cfg(target_os = "windows")]
        platform::append_viewport_log(&format!(
            "resync#{:?} css={css_width:.0} phys={}x{} tauri_scale={scale:.2} live_scale={live:.2} expect={want_css_w:.0}",
            attempt.unwrap_or(0),
            inner.width,
            inner.height
        ));
        if (css_now_w - want_css_w).abs() > 3.0 || (css_now_h - want_css_h).abs() > 40.0 {
            eprintln!(
                "[guard] RESIZE phys={}x{} (css {css_now_w:.0}x{css_now_h:.0}) → {tw}x{th}",
                inner.width, inner.height
            );
            let _ = w.set_size(tauri::PhysicalSize::new(tw.max(1) as u32, th.max(1) as u32));
            return Ok("resized".to_string());
        }
        let expect_css = want_css_w;
        if (css_width - expect_css).abs() <= 8.0 {
            return Ok("ok".to_string());
        }
        let hard = (css_width - expect_css).abs() > 40.0;
        let n = attempt.unwrap_or(0);
        // B22：升级阶梯。硬失同步（布局视口比可见宽度大一大截）以前是直接重载，
        // 但实测重载并不总能修好——WebView2 重新加载文档时可能仍沿用缓存的
        // RasterizationScale / page zoom，重载完照样宽。现在改成：
        //   前两次抖动重排 → 第 3~4 次「强制重排」（zoom 归 1 + 尺寸先撑大再收回，
        //   强迫 Windows 发 WM_DPICHANGED、渲染器重算 DPR）→ 再不行才重载。
        if n >= 4 {
            eprintln!("[guard] RELOAD css={css_width} expect={expect_css}");
            return Ok("reload".to_string());
        }
        if hard || n >= 3 {
            eprintln!(
                "[guard] FORCE-REPAINT#{n} css={css_width:.0} expect={expect_css:.0} (hard={hard})"
            );
            let _ = w.set_zoom(1.0);
            let _ = w.set_size(tauri::PhysicalSize::new(
                (tw + 140).max(1) as u32,
                (th + 90).max(1) as u32,
            ));
            std::thread::sleep(std::time::Duration::from_millis(220));
            let _ = w.set_size(tauri::PhysicalSize::new(tw.max(1) as u32, th.max(1) as u32));
            std::thread::sleep(std::time::Duration::from_millis(120));
            let _ = w.as_ref().set_size(tauri::PhysicalSize::new(tw.max(1) as u32, th.max(1) as u32));
            return Ok("forced".to_string());
        }
        eprintln!("[guard] NUDGE#{attempt:?} css={css_width} expect={expect_css} phys={}x{}", inner.width, inner.height);
        let _ = w.set_size(tauri::PhysicalSize::new(inner.width + 2, inner.height));
        std::thread::sleep(std::time::Duration::from_millis(80));
        let _ = w.set_size(tauri::PhysicalSize::new(inner.width, inner.height));
        Ok("nudged".to_string())
    })
    .await
    .map_err(|e| format!("后台任务失败：{}", e))?
}

/// B22：手动「恢复窗口」——给用户一个自己按的兜底按钮。
///
/// 用户实测里出现过两类界面异常：窗口被写成宽态、内容被窗口右边缘裁掉。
/// 自动守卫在 DPI 反复变化的窗口期不一定每次都能及时收口，所以提供一个
/// 一键恢复：清掉最大化/全屏 → 窗口尺寸收回设计值 → **把窗口里的 WebView
/// 控件尺寸也重设一遍**（内容比窗口宽，常见根因就是控件尺寸没跟着窗口走）
/// → 前端随后重载渲染器，让 DPR 按当前 DPI 重新取值。
#[tauri::command]
async fn recover_main_window(app: tauri::AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let Some(w) = app.get_webview_window("main") else {
            return Err("主窗口不存在".into());
        };
        let before = w.inner_size().unwrap_or_default();
        if w.is_fullscreen().unwrap_or(false) {
            let _ = w.set_fullscreen(false);
        }
        if w.is_maximized().unwrap_or(false) {
            let _ = w.unmaximize();
        }
        std::thread::sleep(std::time::Duration::from_millis(120));

        // 一、窗口收回设计尺寸。先按当前 DPI 折算物理值，再补一次逻辑值 ——
        // 某些驱动下 scale_factor 还没定型，两次里总有一次是对的。
        let scale = live_scale_of(&w);
        let (tw, th) = norm_target_phys(&w, scale);
        // 关键一步：先把窗口撑大再收回。只做 set_size 到目标值时，如果尺寸本来
        // 就"看着对"，Windows 不会发 WM_DPICHANGED、WebView2 也不会重算
        // RasterizationScale —— 渲染器就一直停在错的 DPR 上（内容按更大宽度排版、
        // 右边被窗口边缘裁掉）。先变一个明显不同的尺寸，再回目标值，能强制
        // 走一遍完整的 DPI/重排流程。
        let _ = w.set_size(tauri::PhysicalSize::new(
            (tw + 160).max(1) as u32,
            (th + 100).max(1) as u32,
        ));
        std::thread::sleep(std::time::Duration::from_millis(260));
        let _ = w.set_size(tauri::PhysicalSize::new(tw.max(1) as u32, th.max(1) as u32));
        std::thread::sleep(std::time::Duration::from_millis(180));
        let _ = w.as_ref().set_size(tauri::PhysicalSize::new(tw.max(1) as u32, th.max(1) as u32));
        // 页面缩放也可能停在非 1.0（WebView2 在 DPI 切换后把 RasterizationScale 与
        // 页面 zoom 搞混时，内容会整体放大、右边被窗口边缘吃掉）。显式归 1.0。
        let _ = w.set_zoom(1.0);

        // 二、位置：窗口整个跑到屏幕外时拉回主屏居中（拖到别的屏上不动它）
        let on_screen = match (w.outer_position(), w.current_monitor()) {
            (Ok(p), Ok(Some(m))) => {
                let s = m.position();
                let sz = m.size();
                p.x >= s.x - 40
                    && p.y >= s.y - 40
                    && p.x < s.x + sz.width as i32 - 80
                    && p.y < s.y + sz.height as i32 - 80
            }
            _ => true,
        };
        if !on_screen {
            if let Ok(Some(m)) = w.primary_monitor() {
                let s = m.position();
                let sz = m.size();
                let x = s.x + (sz.width as i32 - tw) / 2;
                let y = s.y + (sz.height as i32 - th) / 2;
                let _ = w.set_position(tauri::PhysicalPosition::new(x.max(s.x), y.max(s.y)));
            }
        }

        // 三、把根容器宽度锚点清掉，交给重载后的守卫按新 DPI 重算
        let _ = w.eval("document.documentElement.style.removeProperty('--app-css-w')");

        eprintln!(
            "[recover] 手动恢复：{}x{} → {tw}x{th}（scale {scale}）",
            before.width, before.height
        );
        Ok(format!(
            "已恢复到设计尺寸 {}x{}（缩放 {:.2}）",
            tw, th, scale
        ))
    })
    .await
    .map_err(|e| format!("恢复失败：{}", e))?
}

/// 切换单屏分辨率/刷新率，并回读验证；不匹配时恢复原显示配置。
#[tauri::command]
async fn set_display_mode(app: tauri::AppHandle, display_id: String, mode: u32) -> Result<String, String> {
    let r = tauri::async_runtime::spawn_blocking(move || platform::set_display_mode(&display_id, mode))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    normalize_main_window_settled(&app);
    r
}

/// 执行 Screenguard 原生实现的 macOS 显示操作。
#[tauri::command]
async fn display_action(
    action: String,
    display_id: String,
    target_id: Option<String>,
    value: Option<String>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        platform::display_action(&action, &display_id, target_id.as_deref(), value.as_deref())
    })
    .await
    .map_err(|e| format!("后台任务失败：{}", e))?
}

#[tauri::command]
async fn set_brightness(display_id: String, value: u32) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || platform::set_brightness(&display_id, value))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

#[tauri::command]
async fn set_volume(display_id: String, value: u32) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || platform::set_volume(&display_id, value))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 系统默认输出设备信息（名称 / 音量 / 静音 / 是否可调 / 形态因子）
#[tauri::command]
async fn get_system_audio() -> Result<platform::SystemAudio, String> {
    tauri::async_runtime::spawn_blocking(platform::get_system_audio)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 设置系统音量 0-100（托盘滑块同源）。
/// 返回空串=写到选定/默认端点；非空=回退到了该端点名（B13，前端同步显示）。
#[tauri::command]
async fn set_system_volume(value: u32) -> Result<String, String> {
    let r = tauri::async_runtime::spawn_blocking(move || platform::set_system_volume_sel(value))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))??;
    Ok(r.unwrap_or_default())
}

/// 系统静音开关（返回值语义同 set_system_volume）
#[tauri::command]
async fn set_system_mute(on: bool) -> Result<String, String> {
    let r = tauri::async_runtime::spawn_blocking(move || platform::set_system_mute_sel(on))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))??;
    Ok(r.unwrap_or_default())
}

/// 电池设备列表（带电池的连接设备：笔记本内电池 / USB·无线键鼠 / 蓝牙）
#[tauri::command]
async fn get_batteries() -> Result<Vec<platform::BatteryInfo>, String> {
    tauri::async_runtime::spawn_blocking(platform::get_batteries)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

#[tauri::command]
async fn apply_color_space(space: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || platform::apply_color_space(&space))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

#[tauri::command]
async fn match_mac() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(platform::match_mac)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 以管理员身份重启（Windows）：UAC 同意后启动带 --elevated 的新实例，本实例随即退出。
/// 注意这里**不做**退出还原：用户提权重启就是为了改显示设置，
/// 且还原 ICC 需要的权限正是当前缺的，尝试只会产生一串无意义的警告。
#[tauri::command]
fn relaunch_as_admin(app: tauri::AppHandle) -> Result<(), String> {
    platform::relaunch_as_admin()?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
async fn match_ppi(app: tauri::AppHandle) -> Result<(), String> {
    let r = tauri::async_runtime::spawn_blocking(platform::match_ppi)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    normalize_main_window_settled(&app);
    r
}

#[tauri::command]
async fn rotate_secondary(app: tauri::AppHandle) -> Result<(), String> {
    let r = tauri::async_runtime::spawn_blocking(platform::rotate_secondary)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    normalize_main_window_settled(&app);
    r
}

/// 恢复副屏为竖屏
#[tauri::command]
async fn restore_secondary(app: tauri::AppHandle) -> Result<(), String> {
    let r = tauri::async_runtime::spawn_blocking(platform::restore_secondary)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    normalize_main_window_settled(&app);
    r
}

// ---------- 屏幕电源控制（方案A：纯本地） ----------

/// 系统级关闭所有屏幕（一次黑全部屏，含不支持 DDC 的显示器）
#[tauri::command]
async fn screen_off() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(platform::screen_off)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 唤醒屏幕（合成输入事件，解除待机）
#[tauri::command]
async fn screen_wake() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(platform::screen_wake)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 单台显示器电源模式（DDC 0xD6）：1=开 2=待机 4=软关
#[tauri::command]
async fn set_display_power(display_id: String, mode: u32) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || platform::set_display_power(&display_id, mode))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 读单台显示器电源模式（1=开 2=待机 4=软关 5=硬关）
#[tauri::command]
async fn get_display_power(display_id: String) -> Result<u32, String> {
    tauri::async_runtime::spawn_blocking(move || platform::get_display_power(&display_id))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 显示器在线快照（联动检测轮询用，轻量）
#[tauri::command]
async fn display_snapshot() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(platform::display_snapshot)
        .await
        .unwrap_or_default()
}

/// Per-display sleep state reported by macOS Quartz, exposed for power-link diagnostics.
#[tauri::command]
async fn get_display_sleep_states() -> Vec<platform::DisplaySleepState> {
    tauri::async_runtime::spawn_blocking(platform::display_sleep_states)
        .await
        .unwrap_or_default()
}

// ---------- 显示器健康：KVM/EDID 异常检测与一键修复（无需管理员权限） ----------

/// Whether this build includes the Windows event-log based display-health feature.
#[tauri::command]
fn health_supported() -> bool {
    cfg!(target_os = "windows")
}

/// 读取最近的显示链路异常事件（KVM 切换、显示器突然离线、EDID 协商失败等）
#[tauri::command]
async fn health_events(minutes: Option<u32>) -> String {
    let m = minutes.unwrap_or(1440);
    tauri::async_runtime::spawn_blocking(move || platform::health_events(m))
        .await
        .unwrap_or_default()
}

/// 每台在用屏幕的当前模式（分辨率/刷新率/方向）
#[tauri::command]
async fn mode_detail() -> String {
    tauri::async_runtime::spawn_blocking(platform::mode_detail)
        .await
        .unwrap_or_default()
}

/// 一键修复显示异常：level 1 = 强制重新协商（轻）；level 2 = 含链路重握手（深）
#[tauri::command]
async fn repair_display(level: Option<u32>) -> Result<String, String> {
    let l = level.unwrap_or(1);
    tauri::async_runtime::spawn_blocking(move || platform::repair_display(l))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

// ---------- 软件层总亮度 / HDR 同步 / 窗口铺满 / 全局快捷键 ----------

/// 各屏当前总亮度（gamma 反解）：行 G|dev|pct
#[tauri::command]
async fn gamma_get() -> String {
    tauri::async_runtime::spawn_blocking(platform::gamma_get)
        .await
        .unwrap_or_default()
}

/// 设置所有屏的总亮度（40~160，100 = 原始；三通道同曲线，不改色相）
#[tauri::command]
async fn gamma_set(pct: u32) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || platform::gamma_set(pct))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 各屏 HDR 状态：行 H|dev|支持|已启用
#[tauri::command]
async fn hdr_states() -> String {
    tauri::async_runtime::spawn_blocking(platform::hdr_states)
        .await
        .unwrap_or_default()
}

/// 把所有支持 HDR 的屏统一开/关
#[tauri::command]
async fn hdr_set(on: bool) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || platform::hdr_set(on))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 双屏铺满：把当前前台窗口铺满所有屏（不限播放器）
#[tauri::command]
async fn span_foreground(app: tauri::AppHandle, mode: Option<u32>) -> Result<String, String> {
    // 默认 mode=1「以主屏为基准」：本机两块屏分辨率不同，用包围盒会把主屏底部裁掉
    let m = mode.unwrap_or(1);
    // B19：前台是本应用自己 → 纯提示、立刻返回。这里连几何快照/写回/归一化都不做，
    // 因为用户要求的语义就是「弹个提示、屏幕不变、窗口也一个像素都不许动」。
    // （上一版在这条路径上改铺满「最近外部窗口」，mode=2 会先改分辨率，DPI 往返
    //  把主窗口写成 2079/2130/3115 这类错误宽度，就是「屏幕没变但窗口变宽」的真凶。）
    #[cfg(target_os = "windows")]
    if let Some(msg) = platform::foreground_self_block() {
        return Err(msg);
    }
    // B17：模式 2 的自动对齐会改显示模式（全桌面 DPI 1.5↔1.0 往返），窗口会被
    // 系统按物理像素重排。先把主窗口几何记下来，失败（含被拦截、铺满动画中途
    // 出错）一律原样写回 —— 用户要求「没法铺满时主界面窗口不要变」。
    let snap = snapshot_main_window(&app);
    let r = tauri::async_runtime::spawn_blocking(move || platform::span_foreground(m)).await;
    // B10 零副作用：铺满被拒（前台是本应用自己等）时一个像素都不动；
    // 只有真正执行了铺满（可能改了分辨率，mode=2）才归一化收口
    match &r {
        Ok(Ok(_)) => normalize_main_window_settled(&app),
        _ => restore_main_window_async(app.clone(), snap),
    }
    r.map_err(|e| format!("后台任务失败：{}", e))?
}

/// 还原上一次被铺满的窗口
#[tauri::command]
async fn restore_foreground(app: tauri::AppHandle) -> Result<(), String> {
    // B17：还原路径会回滚显示模式（全桌面 DPI 抖动），失败时同样把主窗口写回原样
    let snap = snapshot_main_window(&app);
    let window_restore = tauri::async_runtime::spawn_blocking(platform::restore_foreground)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;

    // macOS stores whether this specific window-span operation changed display modes
    // in its recovery record and restores them conditionally inside restore_foreground.
    // An unconditional second restore here could consume an unrelated pending alignment backup.
    #[cfg(target_os = "macos")]
    {
        normalize_main_window_settled(&app);
        return window_restore;
    }

    // Windows' foreground restore only restores the app window; screen alignment is a
    // separate operation and must be rolled back alongside it.
    #[cfg(target_os = "windows")]
    {
        let display_restore = tauri::async_runtime::spawn_blocking(platform::align_restore_modes)
            .await
            .map_err(|e| format!("后台任务失败：{}", e))?;
        let r = match (window_restore, display_restore) {
            (Ok(()), Ok(_)) => Ok(()),
            (Err(window), Err(display)) => Err(format!("窗口还原失败：{}；显示器还原失败：{}", window, display)),
            (Err(window), Ok(_)) => Err(window),
            (Ok(()), Err(display)) => Err(display),
        };
        // 还原把显示模式改回去 → 主窗口同样要重新归一化
        match &r {
            Ok(()) => normalize_main_window_settled(&app),
            Err(_) => restore_main_window_async(app.clone(), snap),
        }
        r
    }
}

/// 注册全局快捷键（Ctrl+Alt+L：全部屏幕待机/唤醒 切换）
#[tauri::command]
fn hotkey_start() -> Result<String, String> {
    platform::hotkey_start()
}

/// 当前生效的全局快捷键（空 = 未注册）
#[tauri::command]
fn hotkey_label() -> String {
    platform::hotkey_label()
}

/// 跨屏缩放对齐：真正写入逐屏缩放档（先备份）
#[tauri::command]
async fn match_dpi_apply(app: tauri::AppHandle) -> Result<String, String> {
    let r = tauri::async_runtime::spawn_blocking(platform::match_dpi_apply)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    normalize_main_window_settled(&app);
    r
}

#[tauri::command]
async fn match_dpi_restore(app: tauri::AppHandle) -> Result<String, String> {
    let r = tauri::async_runtime::spawn_blocking(platform::match_dpi_restore)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    normalize_main_window_settled(&app);
    r
}

// ---------- v0.3.3：窗口选择 / 音频端点选择 / 一键恢复默认 ----------

/// 可以被铺满的窗口列表："hwnd|宽x高|标题"（不含本程序自己的窗口）
#[tauri::command]
async fn list_windows() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(platform::list_windows)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 铺满指定窗口（按 hwnd，用户从列表里挑）
#[tauri::command]
async fn span_window(app: tauri::AppHandle, hwnd: i64, mode: Option<u32>) -> Result<String, String> {
    let m = mode.unwrap_or(1);
    // B17：同 span_foreground —— mode=2 会先改显示模式，失败一律把主窗口写回原样
    let snap = snapshot_main_window(&app);
    let r = tauri::async_runtime::spawn_blocking(move || platform::span_window(hwnd, m))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    match &r {
        Ok(_) => normalize_main_window_settled(&app),
        Err(_) => restore_main_window_async(app.clone(), snap),
    }
    r
}

/// 所有活动的音频输出端点（含是否可调音量），供「系统控制」里选设备
#[tauri::command]
async fn audio_endpoints() -> Result<Vec<platform::AudioEndpoint>, String> {
    tauri::async_runtime::spawn_blocking(platform::audio_endpoints)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 选定要控制的输出端点（空串 = 跟随系统默认）
#[tauri::command]
async fn set_audio_device(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || platform::set_audio_device(&id))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 一键恢复默认：把所有「软件层面」的改动还原成基线快照
#[tauri::command]
async fn restore_defaults(app: tauri::AppHandle) -> Result<String, String> {
    let r = tauri::async_runtime::spawn_blocking(platform::restore_defaults)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    // 分辨率也回到原始（用户要求：一键恢复默认要能恢复所有软件层改动）
    let _ = tauri::async_runtime::spawn_blocking(platform::align_restore_modes).await;
    normalize_main_window_settled(&app);
    r
}

/// 应用内更新：检查 GitHub 最新版本（不再跳转网页，用户要求）
#[tauri::command]
async fn update_check(app: tauri::AppHandle) -> Result<updater::UpdateInfo, String> {
    let current = app.package_info().version.to_string();
    tauri::async_runtime::spawn_blocking(move || updater::update_check_impl(current))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 应用内更新：流式下载安装包（每 2MiB 发一次 update-progress 事件）
#[tauri::command]
async fn update_download(
    app: tauri::AppHandle,
    url: String,
    size: u64,
) -> Result<String, String> {
    let h = app.clone();
    tauri::async_runtime::spawn_blocking(move || updater::update_download_impl(h, url, size))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 应用内更新：启动安装程序并退出本应用（先做退出还原）
#[tauri::command]
fn update_install(app: tauri::AppHandle, path: String) -> Result<(), String> {
    updater::update_install_impl(&path)?;
    let a = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        revert_before_exit();
        a.exit(0);
    });
    Ok(())
}

/// 应用状态（是否退出还原 / 选定的音频端点 / 基线摘要）
#[tauri::command]
fn app_state_json() -> String {
    platform::app_state_json()
}

#[tauri::command]
async fn export_user_settings_json() -> String {
    tauri::async_runtime::spawn_blocking(platform::user_settings_json)
        .await
        .unwrap_or_else(|_| "{}".into())
}

#[tauri::command]
async fn import_user_settings_json(settings_json: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || platform::import_user_settings_json(&settings_json))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 设置「退出软件时自动还原」
#[tauri::command]
fn set_revert_on_exit(on: bool) -> Result<(), String> {
    platform::set_revert_on_exit(on)
}

/// 一键检测：扫描所有与屏幕相关的状态，逐项判定 + 建议，并写入日志
#[tauri::command]
async fn diag_scan() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(platform::diag_scan)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 一键修复：把检测出的、能安全自动处理的问题修掉并自动复查
#[tauri::command]
async fn diag_fix() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(platform::diag_fix)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 结构化体检报告（P0-1）：逐屏逐检查项返回，取代自由文本 diag_scan 的显示部分
#[tauri::command]
async fn health_report() -> Result<platform::HealthReport, String> {
    tauri::async_runtime::spawn_blocking(platform::health_report_impl)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 体检报告 Markdown 导出（P0-3）：默认脱敏——不读也不输出序列号，路径/用户名一律隐藏；
/// include_serial=true 时才读取序列号，且报告顶部会附「请勿公开分享」提示
#[tauri::command]
async fn health_report_markdown(include_serial: Option<bool>) -> Result<String, String> {
    let inc = include_serial.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || platform::health_report_markdown_impl(inc))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 自愈修复动作（P0-4）：按动作 id 执行单个修复，返回统一的 RepairOutcome
/// （applied / not_applicable / failed + 人话 message + 回读核验到的 changed 列表）
#[tauri::command]
async fn repair_action(
    action: String,
    display_id: Option<String>,
    value: Option<String>,
) -> Result<platform::RepairOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        platform::repair_action_impl(&action, display_id.as_deref(), value.as_deref())
    })
    .await
    .map_err(|e| format!("后台任务失败：{}", e))?
}

// ---------- 场景档 Profile（P0-5） ----------

#[tauri::command]
async fn profiles_list() -> Vec<platform::Profile> {
    tauri::async_runtime::spawn_blocking(platform::profiles_list_impl)
        .await
        .unwrap_or_default()
}

/// 捕获当前全部显示状态为一个新档
#[tauri::command]
async fn profile_capture(name: String) -> Result<platform::Profile, String> {
    tauri::async_runtime::spawn_blocking(move || platform::profile_capture_impl(&name))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 应用一个档：失败可回滚，逐步回读核验
#[tauri::command]
async fn profile_apply(app: tauri::AppHandle, id: String) -> Result<platform::ApplyReport, String> {
    let r = tauri::async_runtime::spawn_blocking(move || platform::profile_apply_impl(&id))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    // 档位可能整体改写各屏模式/缩放 → 主窗口重新归一化
    normalize_main_window_settled(&app);
    r
}

/// 更新档位：改名 / 重捕获 targets（「更新为当前状态」）/ 开关自动触发标记 / 改匹配条件
/// （match 用双层 Option：不传 = 不动；传 null = 清空匹配条件）
#[tauri::command]
async fn profile_update(
    id: String,
    name: Option<String>,
    recapture: Option<bool>,
    auto: Option<bool>,
    r#match: Option<Option<platform::ProfileMatch>>,
) -> Result<platform::Profile, String> {
    tauri::async_runtime::spawn_blocking(move || {
        platform::profile_update_impl(&id, name.as_deref(), recapture.unwrap_or(false), auto, r#match)
    })
    .await
    .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 自动触发配置读取（P1-1）
#[tauri::command]
async fn auto_rules_get() -> platform::AutoConfig {
    tauri::async_runtime::spawn_blocking(platform::auto_rules_get_impl)
        .await
        .unwrap_or_default()
}

/// 自动触发配置保存
#[tauri::command]
async fn auto_rules_set(config: platform::AutoConfig) -> Result<platform::AutoConfig, String> {
    tauri::async_runtime::spawn_blocking(move || platform::auto_rules_set_impl(config))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 暂停自动切换 minutes 分钟（0 = 立即恢复）
#[tauri::command]
async fn auto_pause(minutes: u32) -> Result<platform::AutoConfig, String> {
    tauri::async_runtime::spawn_blocking(move || platform::auto_pause_impl(minutes))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

#[tauri::command]
async fn profile_delete(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || platform::profile_delete_impl(&id))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 调整档位顺序（拖拽排序用）；ids 未覆盖的档保持在末尾不丢
#[tauri::command]
async fn profile_reorder(ids: Vec<String>) -> Result<Vec<platform::Profile>, String> {
    tauri::async_runtime::spawn_blocking(move || platform::profile_reorder_impl(&ids))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 导出一个档的 JSON（不含机器特定路径，可直接分享）
#[tauri::command]
async fn profile_export_json(id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || platform::profile_export_json_impl(&id))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 导入一个档（JSON 字符串）；id 冲突自动重新生成
#[tauri::command]
async fn profile_import_json(json: String) -> Result<platform::Profile, String> {
    tauri::async_runtime::spawn_blocking(move || platform::profile_import_impl(&json))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

// ---------------- 软 KVM（P1-2） ----------------

/// 发起配对：生成 6 位配对码（5 分钟内有效），等对端输入
#[tauri::command]
async fn kvm_pair_start() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(kvm::pair_start_impl)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 输入对端的 6 位配对码完成配对
#[tauri::command]
async fn kvm_pair_join(code: String) -> Result<kvm::KvmPeer, String> {
    tauri::async_runtime::spawn_blocking(move || kvm::pair_join_impl(&code))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 已配对设备 + 局域网发现的设备
#[tauri::command]
fn kvm_devices() -> kvm::KvmDevices {
    kvm::devices_impl()
}

/// 解除与一台设备的配对
#[tauri::command]
async fn kvm_peer_remove(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || kvm::peer_remove_impl(&id))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 设置本机在共享屏上的输入口（对端把屏切回来时用）
#[tauri::command]
async fn kvm_peer_set_source(id: String, source: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || kvm::peer_set_source_impl(&id, &source))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 解除所有配对
#[tauri::command]
async fn kvm_unpair_all() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(kvm::unpair_all_impl)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 一键切换（peer_id 缺省 = 唯一对端；多个时必须指定）
#[tauri::command]
async fn kvm_switch(peer_id: Option<String>) -> Result<kvm::KvmSwitchReport, String> {
    tauri::async_runtime::spawn_blocking(move || kvm::switch_impl(peer_id.as_deref()))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

// ===== 配置跨设备迁移（P1-3）=====

/// 导出加密载荷：全部（或指定 id 的）场景档 + 通用偏好
#[tauri::command]
async fn migrate_export(profile_ids: Option<Vec<String>>, password: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        migrate::migrate_export_impl(profile_ids.as_deref(), &password)
    })
    .await
    .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 导入前预览：新增 / 同名覆盖 / 硬件适配提示，不落盘
#[tauri::command]
async fn migrate_preview(payload: String, password: String) -> Result<migrate::MigratePreview, String> {
    tauri::async_runtime::spawn_blocking(move || migrate::migrate_preview_impl(&payload, &password))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 导入落盘：同名覆盖（保留本地 id），其余新增
#[tauri::command]
async fn migrate_import(payload: String, password: String) -> Result<migrate::MigrateImportReport, String> {
    tauri::async_runtime::spawn_blocking(move || migrate::migrate_import_impl(&payload, &password))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

// ===== 排障助手（P2-1）：本地规则引擎优先，LLM 仅在用户同意后调用 =====

/// 排障诊断：本地意图识别 + 跑体检 + 生成原因卡片（第一、二层，完全离线）
#[tauri::command]
async fn troubleshoot_diag(text: String) -> Result<troubleshoot::DiagReply, String> {
    tauri::async_runtime::spawn_blocking(move || troubleshoot::troubleshoot_diag_impl(&text))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 展示「将要发送的内容」：返回脱敏后的完整 JSON（所见即所发），不发送任何请求
#[tauri::command]
async fn troubleshoot_llm_payload(text: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let report = platform::health_report_impl()?;
        let payload = troubleshoot::build_llm_payload(&text, &report);
        serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 联网分析（第三层）：仅在用户点「同意发送」后由前端调用；api_key 存用户本机
#[tauri::command]
async fn agent_llm_chat(text: String, api_key: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || troubleshoot::agent_llm_chat_impl(&text, &api_key))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 在资源管理器里打开日志目录
#[tauri::command]
fn open_log_dir() -> Result<(), String> {
    platform::open_log_dir()
}

/// 日志目录路径（界面显示）
#[tauri::command]
fn diag_log_path() -> String {
    platform::diag_log_path()
}

/// 双屏对齐体检：算 PPI 误差、丢画面比例，并推荐"高度能对上"的分辨率组合（只读）
#[tauri::command]
async fn align_report() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(platform::align_report)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 记录某块屏的物理尺寸（对角线英寸），供 EDID 未上报时计算 PPI
#[tauri::command]
fn set_screen_inches(manuf: String, inches: f64) -> Result<(), String> {
    platform::set_screen_inches(&manuf, inches)
}

/// 应用「双屏对齐」：自动挑可行的模式组合并切换（应用前自动记录原始分辨率）
#[tauri::command]
async fn align_apply(app: tauri::AppHandle) -> Result<String, String> {
    let r = tauri::async_runtime::spawn_blocking(platform::align_apply)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    normalize_main_window_settled(&app);
    r
}

/// 还原两块屏的原始分辨率（4K）
#[tauri::command]
async fn align_restore(app: tauri::AppHandle) -> Result<String, String> {
    let r = tauri::async_runtime::spawn_blocking(platform::align_restore_modes)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?;
    normalize_main_window_settled(&app);
    r
}

/// 小米显示器（REDMI G Pro 27U）当前音量：走它自己的 MiTV 接口（DDC 不通）
#[tauri::command]
async fn mitv_volume_get() -> Result<u32, String> {
    tauri::async_runtime::spawn_blocking(platform::mitv_volume_get)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 把小米显示器音量调到指定值（按键步进逼近，返回实际读回值）
#[tauri::command]
async fn mitv_volume_set(target: u32) -> Result<u32, String> {
    tauri::async_runtime::spawn_blocking(move || platform::mitv_volume_set(target))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 小米显示器音量接口是否可用
#[tauri::command]
async fn mitv_available() -> bool {
    tauri::async_runtime::spawn_blocking(platform::mitv_available)
        .await
        .unwrap_or(false)
}

// ---------- 方案B：ADB 联网精细控制（可选增强） ----------

/// ADB 能力探测（是否找到 adb / 已连接设备）
#[tauri::command]
async fn adb_status() -> platform::AdbStatus {
    tauri::async_runtime::spawn_blocking(platform::adb_status)
        .await
        .unwrap_or_else(|_| platform::AdbStatus {
            available: false,
            path: String::new(),
            devices: Vec::new(),
            tip: "后台任务失败，请重试".to_string(),
        })
}

/// 连接显示器的 ADB（addr 形如 192.168.31.216:5555）
#[tauri::command]
async fn adb_connect(addr: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || platform::adb_connect(&addr))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 电源键：待机/唤醒显示器（KEYCODE_POWER）
#[tauri::command]
async fn adb_power(serial: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || platform::adb_power(&serial))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 音量加减
#[tauri::command]
async fn adb_volume(serial: String, up: bool) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || platform::adb_volume(&serial, up))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 通用 adb shell（进阶用法）
#[tauri::command]
async fn adb_shell(serial: String, cmd: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || platform::adb_shell(&serial, &cmd))
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 一键下载 Android Platform Tools
#[tauri::command]
async fn adb_download() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(platform::adb_download)
        .await
        .map_err(|e| format!("后台任务失败：{}", e))?
}

/// 显示并聚焦主窗口（命令、托盘菜单共用）
fn show_main_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 菜单栏面板：打开主窗口（窗口操作，留在主线程）
#[tauri::command]
fn open_main(app: tauri::AppHandle) -> Result<(), String> {
    show_main_window(&app);
    Ok(())
}

/// B9：两条退出路径（应用内「退出软件」+ 托盘「退出（自动还原）」）统一走这里。
/// 秒响应：先隐藏所有窗口、移除托盘图标，用户点击的瞬间就看到「已退出」；
/// 还原挪到独立线程，12 秒硬超时兜底强退 —— 此前还原（重型 PowerShell、逐屏
/// DDC 探测）跑在主线程同步命令里，任何一步卡住进程就永远退不出去、托盘残留。
fn quit_with_restore(app: &tauri::AppHandle) {
    use tauri::Manager;
    // 1) 秒响应：藏窗口 + 收托盘图标
    for label in ["main", "panel"] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.hide();
        }
    }
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_visible(false);
    }
    // 2) 后台还原 + 超时兜底退出
    let handle = app.clone();
    std::thread::spawn(move || {
        let worker = std::thread::spawn(revert_before_exit);
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(12);
        while !worker.is_finished() {
            if start.elapsed() >= timeout {
                break; // 还原卡死：放弃剩余步骤，保底退出
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        handle.exit(0);
    });
}

/// 菜单栏面板：退出（窗口操作，留在主线程）
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    quit_with_restore(&app);
}

/// B3：把文本写到用户通过「另存为」对话框选定的路径（导出设置用）
#[tauri::command]
fn write_text_file(path: String, contents: String) -> Result<(), String> {
    std::fs::write(&path, contents.as_bytes()).map_err(|e| format!("写入文件失败：{}", e))
}

/// 退出前把「软件层面」的改动还原成基线（用户要求：退出软件后自动恢复原样）。
/// 可在状态文件里用 revert_on_exit=false 关掉。
fn revert_before_exit() {
    let js: serde_json::Value =
        serde_json::from_str(&platform::app_state_json()).unwrap_or(serde_json::Value::Null);
    let on = js
        .get("revert_on_exit")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if on {
        let _ = platform::restore_defaults();
        // 退出软件 → 两块屏分辨率也回到原始 4K
        let _ = platform::align_restore_modes();
    }
}

/// 菜单栏面板：隐藏浮窗（不退出应用；面板的 ✕ 按钮与 Esc 键调用）
#[tauri::command]
fn hide_panel(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("panel") {
        w.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Shared power-link preference for the main and menu-bar WebViews.
/// WKWebView localStorage is not a reliable cross-window source of truth.
#[tauri::command]
fn get_power_link_preference(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri::Manager;
    let path = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法读取应用数据目录：{}", e))?
        .join("power-link-enabled");
    match std::fs::read_to_string(path) {
        Ok(value) => {
            let on = value.trim() == "1";
            // 同步到进程级开关：全局快捷键 Ctrl+Alt+L 依赖它做门控
            platform::set_power_link_on(on);
            Ok(on)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            platform::set_power_link_on(false);
            Ok(false)
        }
        Err(error) => Err(format!("读取电源联动偏好失败：{}", error)),
    }
}

/// Persist the shared power-link preference in the app data directory.
#[tauri::command]
fn set_power_link_preference(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    use tauri::Manager;
    let path = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法读取应用数据目录：{}", e))?;
    std::fs::create_dir_all(&path).map_err(|e| format!("创建应用数据目录失败：{}", e))?;
    std::fs::write(
        path.join("power-link-enabled"),
        if enabled { "1" } else { "0" },
    )
    .map_err(|e| format!("保存电源联动偏好失败：{}", e))?;
    // 同步到进程级开关：全局快捷键 Ctrl+Alt+L 依赖它做门控
    platform::set_power_link_on(enabled);
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.eval(&format!("window.applyPowerLinkUiState({})", enabled));
    }
    Ok(())
}

/// 从主窗口打开/收起菜单栏控制面板（开关式：已可见则收起）。
/// 返回 "shown"（本次打开）或 "hidden"（本次收起），供前端提示。
#[tauri::command]
fn open_control_panel(app: tauri::AppHandle, power_link_enabled: bool) -> Result<String, String> {
    use tauri::Manager;
    let w = app
        .get_webview_window("panel")
        .ok_or_else(|| "控制面板窗口不可用".to_string())?;
    // 已在显示 → 本次点击是「收起」
    if w.is_visible().map_err(|e| format!("查询控制面板状态失败：{}", e))? {
        w.hide().map_err(|e| format!("收起控制面板失败：{}", e))?;
        return Ok("hidden".to_string());
    }
    if let Ok(Some(mon)) = w.primary_monitor() {
        let pos = mon.position();
        let size = mon.size();
        let scale = mon.scale_factor();
        let width = w
            .outer_size()
            .map(|s| s.width as i32)
            .unwrap_or((382.0 * scale) as i32);
        let gap = (10.0 * scale) as i32;
        w.set_position(tauri::PhysicalPosition::new(
            pos.x + size.width as i32 - width - gap,
            pos.y + gap,
        ))
        .map_err(|e| format!("定位控制面板失败：{}", e))?;
    }
    w.show().map_err(|e| format!("打开控制面板失败：{}", e))?;
    w.set_focus()
        .map_err(|e| format!("聚焦控制面板失败：{}", e))?;
    w.eval(&format!(
        "window.applyPowerLinkPreference({})",
        power_link_enabled
    ))
    .map_err(|e| format!("同步菜单栏电源联动状态失败：{}", e))?;
    Ok("shown".to_string())
}

// ================= 显示器健康：自动修复 + 右下角气泡提醒 =================
// 设计：后台线程每 45 秒查一次「设备突然离线」（事件 ID 1010）日志；
// 发现新的显示器掉线 → 弹右下角小窗提醒；若开了自动修复且屏幕没自己回来，
// 就强制重新协商一次显示配置。两个开关都存在 %APPDATA%\Screenguard\health.json。

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct HealthConfig {
    #[serde(default)]
    pub auto_repair: bool,
    #[serde(default = "hc_true")]
    pub notify: bool,
}
fn hc_true() -> bool {
    true
}

fn health_cfg_path() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    std::path::Path::new(&base)
        .join("Screenguard")
        .join("health.json")
}

pub(crate) fn health_cfg() -> HealthConfig {
    std::fs::read_to_string(health_cfg_path())
        .ok()
        .and_then(|s| serde_json::from_str::<HealthConfig>(&s).ok())
        .unwrap_or(HealthConfig {
            auto_repair: false,
            notify: true,
        })
}

fn save_health_cfg(c: &HealthConfig) {
    let p = health_cfg_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(c) {
        let _ = std::fs::write(p, s);
    }
}

#[tauri::command]
fn health_get_config() -> HealthConfig {
    health_cfg()
}

#[tauri::command]
fn health_set_config(auto_repair: bool, notify: bool) -> Result<(), String> {
    save_health_cfg(&HealthConfig {
        auto_repair,
        notify,
    });
    Ok(())
}

// ---- 气泡内容（notify 窗口创建后自己来取，或由 emit 推送）----
#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
pub struct NotifyPayload {
    pub icon: String,
    pub title: String,
    pub body: String,
}

fn notify_payload() -> &'static std::sync::Mutex<NotifyPayload> {
    static P: std::sync::OnceLock<std::sync::Mutex<NotifyPayload>> = std::sync::OnceLock::new();
    P.get_or_init(|| std::sync::Mutex::new(NotifyPayload::default()))
}

#[tauri::command]
fn get_notify_payload() -> NotifyPayload {
    notify_payload()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

#[tauri::command]
fn close_notify(app: tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("notify") {
        let _ = w.hide();
    }
}

/// 自检用：手动弹一条提醒（验证气泡通知是否正常）
#[tauri::command]
fn notify_test(app: tauri::AppHandle) {
    show_notify(
        &app,
        "💡",
        "提醒功能正常",
        "这是测试气泡：以后显示器链路掉线时，这里会第一时间告诉你。",
    );
}

/// 弹一条右下角气泡（不抢焦点，12 秒后自动消失）
fn show_notify(app: &tauri::AppHandle, icon: &str, title: &str, body: &str) {
    use tauri::{Emitter, Manager};
    let p = NotifyPayload {
        icon: icon.to_string(),
        title: title.to_string(),
        body: body.to_string(),
    };
    if let Ok(mut g) = notify_payload().lock() {
        *g = p.clone();
    }
    if let Some(w) = app.get_webview_window("notify") {
        // 紧贴主屏工作区右下角。
        // 关键：必须带上显示器自己的**原点**（position）—— 多屏布局里主屏原点常不是 (0,0)，
        // 旧代码只拿 size() 硬算「屏宽-窗宽」，结果算到别的屏/别处去（实测气泡跑到左上角）。
        // 用 Tauri 的显示器信息而不是 PowerShell 的 SPI_GETWORKAREA：
        // PowerShell 进程不是 DPI 感知的，取回的坐标会被虚拟化，反而对不上。
        let scale = w.scale_factor().unwrap_or(1.0);
        let place = |w: &tauri::WebviewWindow| {
            if let Ok(Some(mon)) = w.primary_monitor() {
                let pos = mon.position();
                let size = mon.size();
                let ww = (360.0 * scale) as i32;
                let wh = (104.0 * scale) as i32;
                let pad = (10.0 * scale) as i32;
                let taskbar = (48.0 * scale) as i32; // 主屏任务栏
                let x = pos.x + size.width as i32 - ww - pad;
                let y = pos.y + size.height as i32 - wh - pad - taskbar;
                let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
            } else if let Ok((l, t, r, b)) = platform::primary_work_area() {
                let ww = (360.0 * scale) as i32;
                let wh = (104.0 * scale) as i32;
                let pad = (10.0 * scale) as i32;
                let _ = w.set_position(tauri::PhysicalPosition::new(
                    (r - ww - pad).max(l),
                    (b - wh - pad).max(t),
                ));
            }
        };
        place(&w);
        let _ = w.emit("notify", p.clone());
        let _ = w.show();
        // 窗口首次创建时页面还没加载完（收不到上面那次 emit），
        // 稍后再补发一次，保证气泡里一定有内容；位置也再确认一次。
        std::thread::sleep(std::time::Duration::from_millis(260));
        place(&w);
        let _ = w.emit("notify", p);
    }
}

/// 后台监控：链路掉线 → 提醒（+ 可选自动修复）
#[cfg(target_os = "windows")]
fn spawn_health_watch(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        use std::collections::HashSet;
        use std::time::{Duration, Instant};

        let count_displays = || -> usize {
            platform::mode_detail()
                .lines()
                .filter(|l| l.starts_with("M|"))
                .count()
        };

        // 基线：当前几台屏在用（用于判断掉线后有没有自己回来）
        let mut baseline = count_displays();
        if baseline == 0 {
            std::thread::sleep(Duration::from_secs(20));
            baseline = count_displays();
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut first = true; // 首次只登记已有事件，避免开机就报历史
        let mut last_fix = Instant::now() - Duration::from_secs(600);

        loop {
            std::thread::sleep(Duration::from_secs(45));
            let cfg = health_cfg();
            let raw = platform::health_events(6); // 近 6 分钟
            let mut fresh: Vec<String> = Vec::new();
            for line in raw.lines() {
                let p: Vec<&str> = line.split('|').collect();
                if p.len() < 6 || p[0] != "E" || p[4] != "1010" {
                    continue; // 只看「设备突然离线」
                }
                let key = format!("{}|{}", p[1], p[5]);
                if seen.insert(key) && !first {
                    fresh.push(p[5].to_string());
                }
            }
            if first {
                first = false;
                continue;
            }
            if fresh.is_empty() {
                continue;
            }
            let disp = fresh.iter().filter(|m| m.contains("DISPLAY")).count();
            if disp == 0 {
                continue; // 非显示设备的事件（USB 存储等）不打扰
            }

            if cfg.notify {
                show_notify(
                    &app,
                    "⚠️",
                    "显示器链路掉线",
                    &format!(
                        "{} 台显示器从系统里消失（同批共 {} 个设备事件）。这类抖动多由 KVM／线材／供电引起。",
                        disp,
                        fresh.len()
                    ),
                );
            }

            // 我们主动让屏幕待机时**绝不**自动修复：待机会产生「显示器掉线」事件，
            // 而深度修复内部含 screen_off()，会把刚被鼠标唤醒的屏又关回去（用户实测）。
            if cfg.auto_repair
                && !platform::standby_state()
                && !platform::recently_changed(120)
                && last_fix.elapsed() > Duration::from_secs(150)
            {
                std::thread::sleep(Duration::from_secs(6)); // 给系统 6 秒自己恢复的机会
                let now = count_displays();
                if now < baseline {
                    let ok = platform::repair_display(1).is_ok();
                    last_fix = Instant::now();
                    if cfg.notify {
                        show_notify(
                            &app,
                            if ok { "🩺" } else { "❗" },
                            if ok {
                                "已自动修复显示配置"
                            } else {
                                "自动修复未成功"
                            },
                            if ok {
                                "检测到显示器没有自行恢复，已强制重新协商显示配置。若画面仍异常，可在面板点「深度修复」。"
                            } else {
                                "自动重新协商失败，建议在面板手动点「深度修复」，或检查 KVM／线材连接。"
                            },
                        );
                    }
                }
            }
        }
    });
}

/// macOS 电源联动使用原生后台轮询，不依赖隐藏菜单栏 WebView 的 JavaScript 定时器。
/// macOS 优先用 DDC VCP 0xD6 检测单屏电源变化，无法读到时回退到连接/断开事件。
#[cfg(any(target_os = "macos", test))]
fn macos_power_transition(
    previous_online: &[String],
    current_online: &[String],
    previous_power: &std::collections::HashMap<String, u32>,
    current_power: &std::collections::HashMap<String, u32>,
) -> Option<(String, &'static str)> {
    let mut ddc_changes = Vec::new();
    for (id, previous_mode) in previous_power {
        let Some(current_mode) = current_power.get(id) else {
            continue;
        };
        // MCCS VCP 0xD6: 2=standby, 3=suspend, 4=soft off, 5=hard off.
        // Every reported non-on power state is an off transition for screen-linking.
        let direction = if *previous_mode == 1 && matches!(*current_mode, 2 | 3 | 4 | 5) {
            Some("off")
        } else if matches!(*previous_mode, 2 | 3 | 4 | 5) && *current_mode == 1 {
            Some("on")
        } else {
            None
        };
        if let Some(direction) = direction {
            ddc_changes.push((id.clone(), direction));
        }
    }

    match ddc_changes.len() {
        1 => return ddc_changes.pop(),
        count if count > 1 => return None,
        _ => {}
    }

    let removed = previous_online
        .iter()
        .any(|id| !current_online.contains(id));
    let added = current_online
        .iter()
        .any(|id| !previous_online.contains(id));
    match (removed, added) {
        (true, false) => Some((String::new(), "off")),
        (false, true) => Some((String::new(), "on")),
        _ => None,
    }
}

/// Use macOS Quartz sleep state as a second per-display sensor when VCP 0xD6 is unreadable.
#[cfg(any(target_os = "macos", test))]
fn macos_display_sleep_transition(
    previous: &std::collections::HashMap<String, bool>,
    current: &std::collections::HashMap<String, bool>,
) -> Option<(String, &'static str)> {
    let mut changes = Vec::new();
    for (id, was_asleep) in previous {
        let Some(is_asleep) = current.get(id) else {
            continue;
        };
        if was_asleep != is_asleep {
            changes.push((id.clone(), if *is_asleep { "off" } else { "on" }));
        }
    }
    if changes.len() == 1 {
        changes.pop()
    } else {
        None
    }
}

#[cfg(any(target_os = "macos", test))]
fn combine_macos_power_transitions(
    ddc: Option<(String, &'static str)>,
    system_sleep: Option<(String, &'static str)>,
) -> Option<(String, &'static str)> {
    match (ddc, system_sleep) {
        (Some(ddc), Some(system_sleep)) if ddc == system_sleep => Some(ddc),
        (Some(_), Some(_)) => None,
        (Some(transition), None) | (None, Some(transition)) => Some(transition),
        (None, None) => None,
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_should_fallback_to_system_power(source_id: &str, ddc_failures: usize) -> bool {
    source_id.is_empty() || ddc_failures > 0
}

#[cfg(test)]
mod macos_power_transition_tests {
    use super::{
        combine_macos_power_transitions, macos_display_sleep_transition,
        macos_power_transition, macos_should_fallback_to_system_power,
    };
    use std::collections::HashMap;

    fn readings(values: &[(&str, u32)]) -> HashMap<String, u32> {
        values
            .iter()
            .map(|(id, mode)| ((*id).to_string(), *mode))
            .collect()
    }

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn detects_single_ddc_off_and_wake_transitions_for_all_off_modes() {
        let online = ids(&["main", "secondary"]);
        let before = readings(&[("main", 1), ("secondary", 1)]);
        for mode in [2, 3, 4, 5] {
            let off = readings(&[("main", mode), ("secondary", 1)]);
            assert_eq!(
                macos_power_transition(&online, &online, &before, &off),
                Some(("main".into(), "off")),
                "mode {mode} should trigger off"
            );
            assert_eq!(
                macos_power_transition(&online, &online, &off, &before),
                Some(("main".into(), "on")),
                "mode {mode} should trigger wake"
            );
        }
    }

    #[test]
    fn ignores_unreadable_and_ambiguous_ddc_changes() {
        let online = ids(&["main", "secondary"]);
        let before = readings(&[("main", 1), ("secondary", 1)]);
        let unreadable = readings(&[("main", 1)]);
        let conflicting_before = readings(&[("main", 1), ("secondary", 4)]);
        let conflicting = readings(&[("main", 4), ("secondary", 1)]);
        let two_changes = readings(&[("main", 4), ("secondary", 2)]);

        assert_eq!(
            macos_power_transition(&online, &online, &before, &unreadable),
            None
        );
        assert_eq!(
            macos_power_transition(&online, &online, &conflicting_before, &conflicting),
            None
        );
        assert_eq!(
            macos_power_transition(&online, &online, &before, &two_changes),
            None
        );
    }

    #[test]
    fn uses_only_unambiguous_display_disconnect_or_reconnect_events() {
        let power = HashMap::new();
        let before = ids(&["main", "secondary"]);
        assert_eq!(
            macos_power_transition(&before, &ids(&["main"]), &power, &power),
            Some((String::new(), "off"))
        );
        assert_eq!(
            macos_power_transition(&ids(&["main"]), &before, &power, &power),
            Some((String::new(), "on"))
        );
        assert_eq!(
            macos_power_transition(&before, &ids(&["secondary", "new"]), &power, &power),
            None
        );
    }

    #[test]
    fn falls_back_to_system_power_when_source_disappears_or_ddc_control_fails() {
        assert!(macos_should_fallback_to_system_power("", 0));
        assert!(macos_should_fallback_to_system_power("main", 1));
        assert!(!macos_should_fallback_to_system_power("main", 0));
    }

    #[test]
    fn detects_single_coregraphics_display_sleep_and_wake() {
        let awake = readings_bool(&[("main", false), ("secondary", false)]);
        let main_asleep = readings_bool(&[("main", true), ("secondary", false)]);
        assert_eq!(
            macos_display_sleep_transition(&awake, &main_asleep),
            Some(("main".into(), "off"))
        );
        assert_eq!(
            macos_display_sleep_transition(&main_asleep, &awake),
            Some(("main".into(), "on"))
        );
    }

    #[test]
    fn ignores_ambiguous_or_new_coregraphics_sleep_states() {
        let awake = readings_bool(&[("main", false), ("secondary", false)]);
        let both_asleep = readings_bool(&[("main", true), ("secondary", true)]);
        let new_display = readings_bool(&[("main", false), ("new", true)]);
        assert_eq!(macos_display_sleep_transition(&awake, &both_asleep), None);
        assert_eq!(macos_display_sleep_transition(&awake, &new_display), None);
    }

    #[test]
    fn rejects_conflicting_ddc_and_coregraphics_transitions() {
        assert_eq!(
            combine_macos_power_transitions(
                Some(("main".into(), "off")),
                Some(("secondary".into(), "on")),
            ),
            None
        );
        assert_eq!(
            combine_macos_power_transitions(
                Some(("main".into(), "off")),
                Some(("main".into(), "off")),
            ),
            Some(("main".into(), "off"))
        );
    }

    fn readings_bool(values: &[(&str, bool)]) -> std::collections::HashMap<String, bool> {
        values
            .iter()
            .map(|(id, asleep)| ((*id).to_string(), *asleep))
            .collect()
    }
}

#[cfg(test)]
mod display_sleep_state_serialization_tests {
    use crate::platform::DisplaySleepState;

    #[test]
    fn serializes_sleep_state_fields_for_the_webview() {
        let encoded = serde_json::to_value(vec![DisplaySleepState {
            id: "external-1".to_string(),
            asleep: true,
        }])
        .unwrap();
        assert_eq!(
            encoded,
            serde_json::json!([{ "id": "external-1", "asleep": true }])
        );
    }
}

#[cfg(target_os = "macos")]
fn spawn_macos_power_link_watch(app: tauri::AppHandle) {
    use std::collections::HashMap;
    use std::time::Duration;
    use tauri::{Emitter, Manager};

    std::thread::spawn(move || {
        let mut was_enabled = false;
        let mut previous_online: Vec<String> = Vec::new();
        let mut previous_power: HashMap<String, u32> = HashMap::new();
        let mut previous_sleep: HashMap<String, bool> = HashMap::new();
        let mut pending: Option<(String, u8)> = None;

        loop {
            std::thread::sleep(Duration::from_millis(1200));

            let enabled = app
                .path()
                .app_data_dir()
                .ok()
                .and_then(|path| std::fs::read_to_string(path.join("power-link-enabled")).ok())
                .is_some_and(|value| value.trim() == "1");

            if !enabled {
                was_enabled = false;
                previous_online.clear();
                previous_power.clear();
                previous_sleep.clear();
                pending = None;
                continue;
            }

            let mut current = platform::display_snapshot();
            current.sort();
            if !was_enabled {
                was_enabled = true;
                previous_online = current.clone();
                previous_power.clear();
                previous_sleep = platform::display_sleep_states()
                    .into_iter()
                    .map(|state| (state.id, state.asleep))
                    .collect();
                for id in &current {
                    if let Ok(mode) = platform::get_display_power(id) {
                        previous_power.insert(id.clone(), mode);
                    }
                }
                pending = None;
                let readable = previous_power.len();
                let status = if current.is_empty() {
                    "macOS 电源联动已开启，但当前没有在线显示器；连接显示器后会自动开始检测。"
                        .to_string()
                } else if readable == current.len() {
                    format!(
                        "macOS 后台联动已启动；{}/{} 台显示器可读取 DDC 电源状态。请保持 Screenguard 运行。",
                        readable,
                        current.len(),
                    )
                } else {
                    format!(
                        "macOS 后台联动已启动；{}/{} 台显示器可读取 DDC 电源状态，另监测 macOS Quartz 单屏睡眠状态。请检查显示器 OSD 的 DDC/CI 设置和连接链路。若显示器物理断电后仍在线、DDC 不可读且 macOS 仍报告唤醒，则系统没有可用信号识别这次物理关屏。",
                        readable,
                        current.len(),
                    )
                };
                let _ = app.emit("power-link-status", status);
                continue;
            }

            let mut readings = HashMap::new();
            for id in &current {
                let Ok(mode) = platform::get_display_power(id) else {
                    continue;
                };
                readings.insert(id.clone(), mode);
            }
            let sleep_readings: HashMap<String, bool> = platform::display_sleep_states()
                .into_iter()
                .map(|state| (state.id, state.asleep))
                .collect();

            // 多屏同时出现相反/并发电源变化时不猜测来源，避免把其它屏幕也反向切换。
            // 旧显示器关机后从在线列表消失时，helper 会回退到稳定的断开/重连事件。
            let ddc_transition =
                macos_power_transition(&previous_online, &current, &previous_power, &readings);
            let sleep_transition =
                macos_display_sleep_transition(&previous_sleep, &sleep_readings);
            let transition = combine_macos_power_transitions(ddc_transition, sleep_transition);

            let Some((source_id, direction)) = transition else {
                previous_online = current.clone();
                previous_sleep = sleep_readings;
                for (id, mode) in readings {
                    previous_power.insert(id, mode);
                }
                previous_power.retain(|id, _| current.contains(id));
                pending = None;
                continue;
            };

            let signature = format!("{}:{}:{}", direction, source_id, current.join(","));
            match pending.as_mut() {
                Some((seen, count)) if *seen == signature => *count = count.saturating_add(1),
                _ => pending = Some((signature.clone(), 1)),
            }
            let stable = pending
                .as_ref()
                .is_some_and(|(seen, count)| *seen == signature && *count >= 2);
            if !stable {
                continue;
            }

            pending = None;
            previous_online = current.clone();
            previous_sleep = sleep_readings;
            for (id, mode) in readings {
                previous_power.insert(id, mode);
            }
            previous_power.retain(|id, _| current.contains(id));
            let desired_mode = if direction == "off" { 2 } else { 1 };
            let mut applied = 0usize;
            let mut errors = Vec::new();
            for id in &current {
                if id == &source_id {
                    continue;
                }
                match platform::set_display_power(id, desired_mode) {
                    Ok(()) => {
                        if platform::get_display_power(id).ok() == Some(desired_mode) {
                            previous_power.insert(id.clone(), desired_mode);
                            applied += 1;
                        } else {
                            errors.push(format!("{}：电源模式回读未确认", id));
                        }
                    }
                    Err(error) => errors.push(format!("{}：{}", id, error)),
                }
            }

            // If the source vanished or per-display DDC control/read-back failed,
            // use the OS-wide fallback. This covers displays whose power state is readable
            // but whose peer rejects DDC writes; the system request affects all connected displays.
            let fallback_requested =
                macos_should_fallback_to_system_power(&source_id, errors.len());
            let fallback = if fallback_requested {
                if direction == "off" {
                    platform::screen_off()
                } else {
                    platform::screen_wake()
                }
            } else {
                Ok(())
            };
            let message = if let Err(error) = fallback {
                format!("macOS 电源联动失败：{}", error)
            } else if fallback_requested && !errors.is_empty() {
                format!(
                    "逐屏 DDC 控制未全部确认（{}）；已发送 macOS 系统级{}请求，作用于全部当前显示器，请观察屏幕状态。",
                    errors.join("；"),
                    if direction == "off" { "待机" } else { "唤醒" }
                )
            } else if fallback_requested {
                format!(
                    "检测到显示器{}，已发送 macOS 系统级{}请求。",
                    if direction == "off" {
                        "断开"
                    } else {
                        "重新连接"
                    },
                    if direction == "off" {
                        "待机"
                    } else {
                        "唤醒"
                    }
                )
            } else if errors.is_empty() && applied > 0 {
                format!(
                    "检测到一台显示器{}，已通过 DDC 同步并回读确认{}台其他显示器。",
                    if direction == "off" {
                        "进入待机"
                    } else {
                        "已唤醒"
                    },
                    applied
                )
            } else {
                format!("macOS 电源联动未完成：{}", errors.join("；"))
            };
            eprintln!("[screenguard] {}", message);
            let _ = app.emit("power-link-status", message);
        }
    });
}

/// Windows 电源联动在 Rust 后台线程采样，避免菜单栏 WebView 隐藏后定时器被节流。
#[cfg(target_os = "windows")]
fn spawn_windows_power_link_watch(app: tauri::AppHandle) {
    use std::collections::{HashMap, HashSet};
    use std::time::{Duration, Instant};
    use tauri::{Emitter, Manager};

    std::thread::spawn(move || {
        let mut was_enabled = false;
        let mut previous_online: Vec<String> = Vec::new();
        let mut previous_power: HashMap<String, bool> = HashMap::new();
        let mut pending: Option<(String, u8)> = None;
        let mut expected: HashMap<String, (bool, Instant)> = HashMap::new();

        loop {
            std::thread::sleep(Duration::from_millis(2500));

            let enabled = app
                .path()
                .app_data_dir()
                .ok()
                .and_then(|path| std::fs::read_to_string(path.join("power-link-enabled")).ok())
                .is_some_and(|value| value.trim() == "1");
            if !enabled {
                was_enabled = false;
                previous_online.clear();
                previous_power.clear();
                pending = None;
                expected.clear();
                continue;
            }

            // 屏幕处于「我们主动待机」（快捷键/总览按钮关屏）期间跳过联动：
            // 待机中 DDC 读数会失败或抖动，联动可能误判状态变化、在用户唤醒的
            // 过程中抢发关屏命令，跟快捷键与手动唤醒打架。
            if platform::standby_state() {
                continue;
            }

            let mut online = platform::display_snapshot();
            online.sort();
            online.dedup();
            if online.is_empty() {
                continue;
            }

            let mut power = HashMap::new();
            for id in &online {
                let state = platform::get_display_power(id)
                    .ok()
                    .and_then(|mode| match mode {
                        1 => Some(true),
                        2 | 3 | 4 | 5 => Some(false),
                        _ => None,
                    });
                if let Some(state) = state {
                    power.insert(id.clone(), state);
                } else if let Some(known) = previous_power.get(id) {
                    // DDC 查询失败只表示未知；保留已知状态，不把通信故障推断成关屏。
                    power.insert(id.clone(), *known);
                }
            }

            if !was_enabled {
                was_enabled = true;
                previous_online = online;
                previous_power = power;
                pending = None;
                expected.clear();
                let _ = app.emit(
                    "power-link-status",
                    "Windows 原生后台联动已启动；检测 VCP 0xD6 与显示器连接状态。",
                );
                continue;
            }

            let mut changes: Vec<(String, bool)> = Vec::new();
            for (id, state) in &power {
                if let Some(old) = previous_power.get(id) {
                    if old != state {
                        changes.push((id.clone(), *state));
                    }
                }
            }
            for id in previous_online.iter().filter(|id| !online.contains(id)) {
                if !changes.iter().any(|(changed_id, _)| changed_id == id) {
                    changes.push((id.clone(), false));
                }
            }
            for id in online.iter().filter(|id| !previous_online.contains(id)) {
                if !changes.iter().any(|(changed_id, _)| changed_id == id) {
                    changes.push((id.clone(), true));
                }
            }
            changes.sort_by(|a, b| a.0.cmp(&b.0));

            if changes.is_empty() {
                previous_online = online;
                previous_power = power;
                pending = None;
                expected.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(20));
                continue;
            }

            let signature = changes
                .iter()
                .map(|(id, on)| format!("{}:{}", id, if *on { "on" } else { "off" }))
                .collect::<Vec<_>>()
                .join("|");
            match pending.as_mut() {
                Some((seen, count)) if *seen == signature => *count = count.saturating_add(1),
                _ => pending = Some((signature.clone(), 1)),
            }
            if !pending
                .as_ref()
                .is_some_and(|(seen, count)| *seen == signature && *count >= 2)
            {
                continue;
            }

            previous_online = online.clone();
            previous_power = power;
            pending = None;
            expected.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(20));

            let mut external = Vec::new();
            for (id, on) in changes {
                let is_expected = expected.get(&id).is_some_and(|(wanted, at)| {
                    *wanted == on && at.elapsed() < Duration::from_secs(20)
                });
                if is_expected {
                    expected.remove(&id);
                } else {
                    external.push((id, on));
                }
            }
            if external.is_empty() {
                continue;
            }

            let directions: HashSet<bool> = external.iter().map(|(_, on)| *on).collect();
            if directions.len() != 1 {
                let _ = app.emit(
                    "power-link-status",
                    "检测到多块屏同时切换到不同电源状态；为避免互相覆盖，本次未联动。",
                );
                continue;
            }

            let turn_on = *directions.iter().next().expect("one direction");
            let sources: HashSet<&str> = external.iter().map(|(id, _)| id.as_str()).collect();
            let targets: Vec<String> = online
                .iter()
                .filter(|id| !sources.contains(id.as_str()))
                .cloned()
                .collect();
            if targets.is_empty() {
                let _ = app.emit(
                    "power-link-status",
                    "已检测到显示器状态变化；没有其它在线显示器可联动。",
                );
                continue;
            }

            let mode = if turn_on { 1 } else { 4 };
            let mut succeeded = 0usize;
            let mut first_error = None;
            for id in targets {
                expected.insert(id.clone(), (turn_on, Instant::now()));
                match platform::set_display_power(&id, mode) {
                    Ok(()) => succeeded += 1,
                    Err(error) => {
                        expected.remove(&id);
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }

            let message = if let Some(error) = first_error {
                format!(
                    "Windows 屏幕电源联动完成 {} 台，部分失败：{}",
                    succeeded, error
                )
            } else {
                format!(
                    "检测到一块屏{}，已发送 DDC 联动命令至其余 {} 台屏。",
                    if turn_on { "打开" } else { "关闭" },
                    succeeded
                )
            };
            eprintln!("[screenguard] {}", message);
            let _ = app.emit("power-link-status", message);
        }
    });
}

// ===== 场景自动触发引擎（P1-1）=====
// 2 秒一轮：便宜指纹（原生 API）查变化 → 3 秒防抖 → 匹配档位 → 静默期检查 → 应用。
// 纯自动路径直接调 profile_apply_platform（不标手动变更）；应用前后拉开回声窗口，
// 把自己造成的指纹变化吞掉，避免「应用→检测到变化→再应用」死循环。

fn auto_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn auto_watch_loop(app: tauri::AppHandle) {
    use std::time::{Duration, Instant};
    use tauri::Emitter;

    let emit_auto = |kind: &str, p: Option<&platform::Profile>, reason: &str, message: &str| {
        let payload = serde_json::json!({
            "kind": kind,
            "profile_id": p.map(|x| x.id.clone()).unwrap_or_default(),
            "profile_name": p.map(|x| x.name.clone()).unwrap_or_default(),
            "reason": reason,
            "message": message,
        });
        let _ = app.emit("screens://auto-applied", payload);
    };

    // 启动先等显示栈稳定再采基线，避免开机瞬间把「初始化」当「插拔」
    std::thread::sleep(Duration::from_secs(10));
    let mut baseline = platform::auto_display_fingerprint();
    let mut prev_ac = platform::auto_on_ac();
    let mut change_at: Option<Instant> = None;
    let mut range_in: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut last_range_check = Instant::now();
    let mut echo_until = Instant::now();

    loop {
        std::thread::sleep(Duration::from_secs(2));
        let cfg = platform::auto_cfg_load();

        let paused = cfg
            .paused_until_epoch
            .map(|until| auto_epoch_secs() < until)
            .unwrap_or(false);
        if !cfg.enabled || paused {
            // 关闭/暂停期间只刷基线，保证重新开启后不会拿陈旧基线立刻误触发
            let fp = platform::auto_display_fingerprint();
            if !fp.is_empty() {
                baseline = fp;
            }
            prev_ac = platform::auto_on_ac();
            change_at = None;
            continue;
        }
        if Instant::now() < echo_until {
            // 回声窗口：刚应用过，等显示栈稳定后直接把新状态当基线
            let fp = platform::auto_display_fingerprint();
            if !fp.is_empty() {
                baseline = fp;
            }
            prev_ac = platform::auto_on_ac();
            change_at = None;
            continue;
        }

        let mut trigger: Option<String> = None;

        // ① 显示器集合变化 → 3 秒防抖（防抖期内再变就重新计时）
        if cfg.sources.displays_changed {
            let fp = platform::auto_display_fingerprint();
            if fp != baseline {
                if !fp.is_empty() {
                    baseline = fp;
                }
                change_at = Some(Instant::now());
                continue;
            }
            if let Some(t) = change_at {
                if t.elapsed() >= Duration::from_secs(3) {
                    change_at = None;
                    trigger = Some("显示器连接变化".to_string());
                } else {
                    continue; // 防抖未满，等下一轮
                }
            }
        }

        // ② 电源接入/断开（笔记本插拔适配器）
        if trigger.is_none() && cfg.sources.on_ac {
            let ac = platform::auto_on_ac();
            if let (Some(a), Some(p)) = (ac, prev_ac) {
                if a != p {
                    trigger = Some(if a { "接通电源" } else { "切换到电池" }.to_string());
                }
            }
            prev_ac = ac;
        }

        // ③ 时间段进出：每 30 秒查一次，刚进入时间段的档位作为候选
        let mut range_hits: Vec<String> = Vec::new();
        if cfg.sources.time_range && last_range_check.elapsed() >= Duration::from_secs(30) {
            last_range_check = Instant::now();
            if let Some(hhmm) = platform::auto_local_hhmm() {
                for p in platform::load_profiles() {
                    let Some(m) = p.match_.as_ref() else { continue };
                    let Some(tr) = m.time_range.as_deref() else { continue };
                    if tr.trim().is_empty() || !p.auto {
                        continue;
                    }
                    let now_in = platform::time_in_range(tr, hhmm);
                    let was_in = range_in.get(&p.id).copied().unwrap_or(false);
                    range_in.insert(p.id.clone(), now_in);
                    if now_in && !was_in {
                        range_hits.push(p.id);
                    }
                }
            }
        }

        if trigger.is_none() && range_hits.is_empty() {
            continue;
        }

        // 静默期：用户最近手动改过 → 跳过本次并轻提示
        let quiet_secs = u64::from(cfg.quiet_minutes.max(1)) * 60;
        if platform::manual_recently(quiet_secs) {
            emit_auto(
                "skipped",
                None,
                trigger.as_deref().unwrap_or("到达时间段"),
                "检测到手动调整，本次未自动切换",
            );
            continue;
        }

        // 匹配：列表顺序取第一个命中的
        let profiles = platform::load_profiles();
        let hhmm = platform::auto_local_hhmm();
        let on_ac = platform::auto_on_ac();
        let keys = platform::auto_current_keys();
        let Some((p, reason)) = platform::auto_pick_profile(
            &profiles,
            &keys,
            on_ac,
            hhmm,
            trigger.as_deref(),
            &range_hits,
        ) else {
            continue; // 没有档位命中：常见情况，不打扰
        };

        // 应用（拉开回声窗口吞掉自己造成的变化）
        echo_until = Instant::now() + Duration::from_secs(15);
        let last = match platform::profile_apply_platform(&p) {
            Ok(r) => {
                let rolled = r.rolled_back;
                let ok = r.ok && !rolled;
                if rolled {
                    // 安全阀：自动应用回滚 → 停用该档 auto，防止反复乱改
                    let mut list = platform::load_profiles();
                    if let Some(x) = list.iter_mut().find(|x| x.id == p.id) {
                        x.auto = false;
                    }
                    let _ = platform::save_profiles(&list);
                    show_notify(
                        &app,
                        "⚠️",
                        "场景自动应用失败",
                        &format!(
                            "「{}」自动应用失败已回滚。为避免反复改动，该档自动触发已停用，可重新开启。",
                            p.name
                        ),
                    );
                }
                emit_auto(
                    if rolled { "rolled_back" } else { "applied" },
                    Some(&p),
                    &reason,
                    if rolled {
                        "自动应用失败，已整体回滚；该档自动触发已停用"
                    } else if r.ok {
                        "自动应用完成"
                    } else {
                        "部分成功：个别项这台显示器不支持"
                    },
                );
                platform::AutoLastApplied {
                    profile_id: p.id.clone(),
                    profile_name: p.name.clone(),
                    at: platform::now_str(),
                    reason,
                    ok,
                }
            }
            Err(e) => {
                let mut list = platform::load_profiles();
                if let Some(x) = list.iter_mut().find(|x| x.id == p.id) {
                    x.auto = false;
                }
                let _ = platform::save_profiles(&list);
                show_notify(
                    &app,
                    "⚠️",
                    "场景自动应用失败",
                    &format!("「{}」自动应用出错（{}）。该档自动触发已停用。", p.name, e),
                );
                emit_auto("rolled_back", Some(&p), &reason, &format!("自动应用出错：{}", e));
                platform::AutoLastApplied {
                    profile_id: p.id.clone(),
                    profile_name: p.name.clone(),
                    at: platform::now_str(),
                    reason,
                    ok: false,
                }
            }
        };
        let mut c = platform::auto_cfg_load();
        c.last_applied = Some(last);
        let _ = platform::auto_cfg_save(&c);
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn spawn_auto_watch(app: tauri::AppHandle) {
    std::thread::spawn(move || auto_watch_loop(app));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 「以管理员身份重启」的新实例带 --elevated 参数：旧实例会自行退出，
    // 这里先等它退干净，再进入 single-instance 插件的互斥量检查 —— 顺序
    // 不能反，否则提权新实例会被互斥量拒之门外（回调反而唤出旧窗口）。
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if std::env::args().any(|a| a == "--elevated") {
        for _ in 0..50 {
            if !platform::another_instance_running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    tauri::Builder::default()
        // 单实例必须最先注册：第二次启动 exe 时，本实例立刻退出，由
        // 已运行实例的回调唤出主窗口（用户要求：双击图标 = 打开主窗口，
        // 不再静默退出像「闪退」、也不在托盘积压一堆实例）。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        // B3：导出设置用的系统「另存为」对话框
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
            use tauri::Manager;

            // 右键菜单
            let open_main = MenuItem::with_id(app, "open_main", "打开主窗口", true, None::<&str>)?;
            // 「不依赖界面」的还原入口：铺满之后窗口可能盖住界面上的按钮（实测踩过）
            let unspan = MenuItem::with_id(app, "unspan", "还原铺满的窗口", true, None::<&str>)?;
            let defaults = MenuItem::with_id(app, "defaults", "恢复默认状态", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出（自动还原）", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_main, &unspan, &defaults, &quit])?;

            // 显示/隐藏面板浮窗（右上角、菜单栏下方）
            let toggle_panel = |app_handle: &tauri::AppHandle| {
                if let Some(pw) = app_handle.get_webview_window("panel") {
                    if pw.is_visible().unwrap_or(false) {
                        let _ = pw.hide();
                    } else {
                        if let Ok(Some(mon)) = pw.primary_monitor() {
                            let scale = mon.scale_factor();
                            let pos = mon.position();
                            let size = mon.size();
                            // 关键：必须加上显示器自己的原点（position）。旧代码只用 size()，
                            // 多屏布局下主屏原点不是 (0,0) 时，右边缘就会溢到隔壁屏上（用户实测）。
                            // 上/右各留 10px 间隙，贴着主屏右上角内侧，但不贴死、不越界。
                            // 用窗口真实宽度（而不是硬编码），保证右边一定不越界。
                            let ow = pw
                                .outer_size()
                                .map(|s| s.width as i32)
                                .unwrap_or((382.0 * scale) as i32);
                            let gap = (10.0 * scale) as i32;
                            let x = pos.x + size.width as i32 - ow - gap;
                            let y = pos.y + gap;
                            let _ = pw.set_position(tauri::PhysicalPosition::new(x, y));
                        }
                        let _ = pw.show();
                        let _ = pw.set_focus();
                    }
                }
            };

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().expect("no icon").clone())
                .tooltip("屏幕守护")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app_handle, event| match event.id.as_ref() {
                    "open_main" => show_main_window(app_handle),
                    "unspan" => {
                        let _ = platform::restore_foreground();
                    }
                    "defaults" => {
                        let _ = platform::restore_defaults();
                    }
                    "quit" => {
                        // B9：与应用内退出同一套逻辑（秒响应 + 后台还原 + 超时兜底）
                        quit_with_restore(app_handle)
                    }
                    _ => {}
                })
                .on_tray_icon_event(move |tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_panel(tray.app_handle());
                    }
                })
                .build(app)?;

            // B8：启动归一化收口 —— 提权重启（--elevated）等场景下启动瞬间读到的
            // DPI 缩放可能还没定型，主窗口会停在错误宽度；启动 600ms 后跑一次
            // 归一化序列，把窗口尺寸收到正确值（收敛式：已正确则不动）。
            {
                let h = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    normalize_main_window_settled(&h);
                });
            }

            // B18：尺寸看门狗 —— 任何操作把主窗口写宽/写窄，3s 内自动拉回
            spawn_main_size_watchdog(app.handle().clone());

            // B20：记住最近一个外部前台窗口 —— 主窗口里点「铺满前台窗口」时前台
            // 必然是自己，此时铺满「上次使用的那个窗口」（用户设计的流程）；
            // 没有候选才弹提示，且提示路径不改分辨率、不动窗口。
            #[cfg(target_os = "windows")]
            platform::track_foreground_start();

            // Windows Event Log based health monitor (disconnect alerts + optional repair).
            #[cfg(target_os = "windows")]
            spawn_health_watch(app.handle().clone());

            // macOS 电源联动由原生后台线程执行，面板隐藏或关闭时仍持续监测。
            #[cfg(target_os = "macos")]
            spawn_macos_power_link_watch(app.handle().clone());

            // Windows DDC 0xD6 联动也必须从原生后台线程启动，不能依赖 WebView 定时器。
            #[cfg(target_os = "windows")]
            spawn_windows_power_link_watch(app.handle().clone());

            // 场景自动触发引擎（P1-1）：显示器变化/电源/时间段 → 自动应用场景档
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            spawn_auto_watch(app.handle().clone());

            // 软 KVM（P1-2）：UDP 发现 + TCP 协议服务（配对/切换）
            kvm::set_app_handle(app.handle().clone());
            kvm::ensure_started();

            // v0.3.3：启动后采集一次「基线快照」（供「一键恢复默认」和「退出自动还原」）
            // 延后几秒，等显示/音频栈就绪再读，避免采到空值
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(7));
                let _ = platform::capture_baseline();
                // Windows 一键检测依赖 Windows 事件日志；macOS 上该卡片隐藏且不后台调用。
                #[cfg(target_os = "windows")]
                let _ = platform::diag_scan();
            });

            // 全局快捷键：Ctrl+Alt+L 切换「全部屏幕待机 / 唤醒」（失败只记日志，不影响启动）
            if let Err(e) = platform::hotkey_start() {
                eprintln!("[screenguard] 全局快捷键未注册：{}", e);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // 主窗口点 X = 隐藏到托盘（程序常驻，托盘左键呼出面板）
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_platform,
            get_version,
            update_check,
            update_download,
            update_install,
            write_text_file,
            open_external_page,
            get_autostart,
            set_autostart,
            get_displays,
            get_display_modes,
            set_display_mode,
            display_action,
            window_geometry,
            resync_main_window,
            recover_main_window,
            set_brightness,
            set_volume,
            get_system_audio,
            set_system_volume,
            set_system_mute,
            get_batteries,
            apply_color_space,
            match_mac,
            relaunch_as_admin,
            match_ppi,
            rotate_secondary,
            restore_secondary,
            screen_off,
            screen_wake,
            set_display_power,
            get_display_power,
            display_snapshot,
            get_display_sleep_states,
            health_events,
            health_supported,
            mode_detail,
            repair_display,
            health_report,
            health_report_markdown,
            repair_action,
            profiles_list,
            profile_capture,
            profile_apply,
            profile_update,
            profile_delete,
            profile_reorder,
            profile_export_json,
            profile_import_json,
            auto_rules_get,
            auto_rules_set,
            auto_pause,
            kvm_pair_start,
            kvm_pair_join,
            kvm_devices,
            kvm_peer_remove,
            kvm_peer_set_source,
            kvm_unpair_all,
            kvm_switch,
            migrate_export,
            migrate_preview,
            migrate_import,
            troubleshoot_diag,
            troubleshoot_llm_payload,
            agent_llm_chat,
            health_get_config,
            health_set_config,
            get_notify_payload,
            close_notify,
            notify_test,
            gamma_get,
            gamma_set,
            hdr_states,
            hdr_set,
            span_foreground,
            restore_foreground,
            hotkey_start,
            hotkey_label,
            match_dpi_apply,
            match_dpi_restore,
            list_windows,
            span_window,
            audio_endpoints,
            set_audio_device,
            restore_defaults,
            app_state_json,
            export_user_settings_json,
            import_user_settings_json,
            set_revert_on_exit,
            mitv_volume_get,
            mitv_volume_set,
            mitv_available,
            diag_scan,
            diag_fix,
            open_log_dir,
            diag_log_path,
            align_report,
            set_screen_inches,
            align_apply,
            align_restore,
            adb_status,
            adb_connect,
            adb_power,
            adb_volume,
            adb_shell,
            adb_download,
            open_main,
            quit_app,
            hide_panel,
            open_control_panel,
            get_power_link_preference,
            set_power_link_preference
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
