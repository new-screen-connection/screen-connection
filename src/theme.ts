/**
 * 主题模式：白天 / 夜晚。
 * 偏好存 localStorage（主窗口与托盘面板同源共享，重新打开时生效）；
 * 首次使用跟随系统偏好。
 */
export type ThemeMode = 'dark' | 'light';

const KEY = 'sc-theme';

export function currentTheme(): ThemeMode {
  const t = document.documentElement.dataset.theme;
  return t === 'light' ? 'light' : 'dark';
}

function applyTheme(mode: ThemeMode): void {
  document.documentElement.dataset.theme = mode;
}

/** 应用入口调用一次：恢复已存偏好，否则跟随系统 */
export function initTheme(): void {
  let saved: string | null = null;
  try {
    saved = localStorage.getItem(KEY);
  } catch {
    /* 隐私模式下忽略 */
  }
  if (saved === 'light' || saved === 'dark') {
    applyTheme(saved);
    return;
  }
  applyTheme(window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark');
}

/** 切换并持久化，返回切换后的模式 */
export function toggleTheme(): ThemeMode {
  const next: ThemeMode = currentTheme() === 'dark' ? 'light' : 'dark';
  applyTheme(next);
  try {
    localStorage.setItem(KEY, next);
  } catch {
    /* 忽略存储失败 */
  }
  return next;
}
