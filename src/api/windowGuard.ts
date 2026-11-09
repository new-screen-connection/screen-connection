// B15 主窗口视口守卫：任何操作把窗口「撑宽/写错」后，这里在数秒内自愈。
// 原理：主窗口设计逻辑宽度固定 780（tauri.conf.json）。若 window.innerWidth
// 持续偏离按 DPI 折算的期望值——无论根因是物理尺寸被写错，还是 WebView2 的
// devicePixelRatio 与宿主失同步（用户看到的「画面变宽/内容溢出」）——都属异常，
// 调 resync_main_window 修复；修不好逐级升级（抖动重排 → 重载渲染器）。
import { invoke } from './tauri';

interface WindowGeometry {
  phys_w: number;
  phys_h: number;
  scale: number;
  /** B23：Win32 GetDpiForWindow 拿到的实时缩放（Tauri 的 scale 可能停在旧值）。 */
  dpi_scale?: number;
  expect_css_w: number;
  expect_css_h: number;
  suspicious: boolean;
  maximized: boolean;
}

/** 重载节流：两次重载至少间隔 5s，整个会话最多 6 次（防死循环）。
 *  B21：硬失同步（布局视口比可见宽度大一大截）时页面会被窗口边缘裁掉，重载是
 *  唯一可靠解法，所以节流不能像旧版那样 10 分钟只给 1 次 —— 那样用户连点两次
 *  功能就把额度用完，之后只能看着内容被裁。 */
const RELOAD_KEY = 'sc_vg_reload_ts';
const RELOAD_COUNT_KEY = 'sc_vg_reload_n';

function canReload(): boolean {
  try {
    const last = Number(sessionStorage.getItem(RELOAD_KEY) || 0);
    const n = Number(sessionStorage.getItem(RELOAD_COUNT_KEY) || 0);
    if (n >= 6) return false;
    if (Date.now() - last < 5000) return false;
    return true;
  } catch {
    return false;
  }
}

function markReload(): void {
  try {
    sessionStorage.setItem(RELOAD_KEY, String(Date.now()));
    sessionStorage.setItem(RELOAD_COUNT_KEY, String(Number(sessionStorage.getItem(RELOAD_COUNT_KEY) || 0) + 1));
  } catch { /* 隐私模式等：放弃节流记录，仍允许重载 */ }
}

/** B23：界面排版错乱（内容比窗口宽/高）时，侧栏底部那个「恢复窗口」按钮会被推到
 *  窗口外面根本点不到 —— 用户遇到的就是这种情况。这里在左上角补一个浮层按钮：
 *  左上角在任何错位下都落在可见区内（内容总是从左上开始排），保证一定能点到。 */
function showEmergencyRestore(): void {
  if (document.getElementById('sc-emergency-restore')) return;
  const b = document.createElement('button');
  b.id = 'sc-emergency-restore';
  b.textContent = '恢复窗口';
  b.title = '界面显示异常时点这里：把窗口收回原尺寸并重新加载界面';
  b.style.cssText = [
    'position:fixed',
    'left:10px',
    'top:10px',
    'z-index:2147483647',
    'padding:5px 10px',
    'font:600 12px/1.5 "Segoe UI","Microsoft YaHei",sans-serif',
    'color:#ffffff',
    'background:#e05c5c',
    'border:1px solid #b94b4b',
    'border-radius:7px',
    'cursor:pointer',
    'box-shadow:0 2px 10px rgba(0,0,0,.35)',
  ].join(';');
  b.onclick = () => {
    b.textContent = '正在恢复…';
    invoke<string>('recover_main_window')
      .then(() => window.setTimeout(() => location.reload(), 260))
      .catch(() => {
        b.textContent = '恢复窗口';
      });
  };
  document.body.appendChild(b);
}

function hideEmergencyRestore(): void {
  const el = document.getElementById('sc-emergency-restore');
  if (el) el.remove();
}

export function startWindowGuard(): void {
  // 只在主窗口跑（panel/notify 是独立入口，不挂本守卫）
  let bad = 0;
  let attempt = 0;
  let busy = false;

  /** B21/B22：把根容器宽度钉在窗口「真实」CSS 宽度上。
   *  byDpi = 窗口物理宽度 ÷ 缩放，是权威值；但渲染器自己的 innerWidth 可能比它
   *  更大（DPR 没跟上窗口 DPI）—— 那正是内容越过窗口右边缘被裁掉的情形。
   *  取两者较小值：任何一侧算错，内容都只会「偏窄地排完」，绝不会被裁。
   *  下限 0.55×byDpi 防止过渡态里的异常小值把界面挤成一条。 */
  const pinContentWidth = (physW: number, scale: number): void => {
    if (!(physW > 0) || !(scale > 0)) return;
    const byDpi = Math.round(physW / scale);
    // 三个候选都取最小：「物理宽 ÷ 实时 DPI」是权威值，innerWidth 是渲染器自己
    // 认为的视口，visualViewport.width 是视觉视口。任何一个偏大都会让内容越过
    // 窗口边缘被裁，所以取最小的那个 —— 宁可偏窄排完，也不要被切掉。
    const cands = [byDpi, window.innerWidth, Math.round(window.visualViewport?.width ?? 0)].filter(
      (v) => v > 0,
    );
    const css = Math.max(Math.min(...cands), Math.round(byDpi * 0.55));
    document.documentElement.style.setProperty('--app-css-w', `${css}px`);
  };

  // 启动即钉一次，并在窗口尺寸变化后 120ms 内重钉（尺寸抖动发生时立刻生效，
  // 不用等下一轮 2s 轮询）。
  const pinNow = async (): Promise<void> => {
    try {
      const g = await invoke<WindowGeometry>('window_geometry');
      pinContentWidth(g.phys_w, g.dpi_scale || g.scale);
    } catch { /* 非 Tauri 环境：忽略 */ }
  };
  void pinNow();
  let pinTimer = 0;
  window.addEventListener('resize', () => {
    window.clearTimeout(pinTimer);
    pinTimer = window.setTimeout(() => void pinNow(), 120);
  });

  setInterval(async () => {
    if (busy || document.hidden) return;
    let g: WindowGeometry;
    try {
      g = await invoke<WindowGeometry>('window_geometry');
    } catch {
      return; // 非 Tauri 环境或窗口不在：静默跳过
    }
    pinContentWidth(g.phys_w, g.dpi_scale || g.scale);
    // 最大化是用户意图，不干预
    if (g.maximized) {
      bad = 0;
      hideEmergencyRestore();
      return;
    }
    const drift = window.innerWidth - g.expect_css_w;
    // 硬失同步：布局视口比可见宽度大 >40 CSS px。此时页面按「宽布局」排版，
    // 右边被窗口边缘裁掉（用户看到的「内容被遮挡」）。它比小幅漂移更严重，所以
    // 不等 suspicious 让路；但仍然要求「连续两轮」——DPI 切换瞬间 dpr/innerWidth
    // 会短暂错位 1~2 秒就自愈，立刻就重载反而会把正常操作打断（实测铺满过程里
    // 会连着重载两次）。CSS 兜底（--app-css-w）保证这两轮里内容也不会被裁。
    const hard = Math.abs(drift) > 40;
    if (g.suspicious && !hard) {
      bad = 0;
      return;
    }
    if (Math.abs(drift) <= 8) {
      bad = 0;
      attempt = 0;
      hideEmergencyRestore();
      return;
    }
    // B23：界面已经错位时，侧栏底部那个按钮会被推到窗口外点不到 —— 补左上角浮层。
    if (hard) showEmergencyRestore();
    bad += 1;
    if (bad < 2) return; // 连续两轮超差才动手（躲过单次过渡态）
    bad = 0;
    attempt += 1;
    busy = true;
    try {
      const r = await invoke<string>('resync_main_window', {
        cssWidth: window.innerWidth,
        attempt,
      });
      if (r === 'reload' && canReload()) {
        markReload();
        location.reload();
      }
    } catch { /* 下轮再试 */ } finally {
      busy = false;
    }
  }, 2000);
}
