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
  expect_css_w: number;
  expect_css_h: number;
  suspicious: boolean;
  maximized: boolean;
}

/** 重载节流：10 分钟内最多自愈重载 1 次，整个会话最多 2 次，防死循环。 */
const RELOAD_KEY = 'sc_vg_reload_ts';
const RELOAD_COUNT_KEY = 'sc_vg_reload_n';

function canReload(): boolean {
  try {
    const last = Number(sessionStorage.getItem(RELOAD_KEY) || 0);
    const n = Number(sessionStorage.getItem(RELOAD_COUNT_KEY) || 0);
    if (n >= 2) return false;
    if (Date.now() - last < 10 * 60 * 1000) return false;
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

export function startWindowGuard(): void {
  // 只在主窗口跑（panel/notify 是独立入口，不挂本守卫）
  let bad = 0;
  let attempt = 0;
  let busy = false;
  setInterval(async () => {
    if (busy || document.hidden) return;
    let g: WindowGeometry;
    try {
      g = await invoke<WindowGeometry>('window_geometry');
    } catch {
      return; // 非 Tauri 环境或窗口不在：静默跳过
    }
    // 最大化是用户意图，不干预；缩放交叉校验不过（DPI 抖动中）本轮跳过
    if (g.maximized || g.suspicious) {
      bad = 0;
      return;
    }
    const drift = window.innerWidth - g.expect_css_w;
    if (Math.abs(drift) <= 8) {
      bad = 0;
      attempt = 0;
      return;
    }
    bad += 1;
    if (bad < 2) return; // 连续两轮超差才动手（防抖：躲过单次过渡态）
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
