/** 统一命令通道：Tauri 内走真实后端，浏览器预览走 mock。 */
import { mockInvoke } from './mock';

type TauriGlobal = {
  core: { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };
  event: { listen: (event: string, handler: (e: { payload: unknown }) => void) => Promise<() => void> };
};

function tauri(): TauriGlobal | null {
  const g = (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__;
  return g ?? null;
}

export const isTauri = (): boolean => tauri() !== null;

export async function invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const t = tauri();
  if (t) return t.core.invoke(cmd, args) as Promise<T>;
  return mockInvoke<T>(cmd, args);
}

/** 监听后端事件（浏览器预览下无事件源，静默跳过） */
export async function listenEvent(event: string, handler: (payload: unknown) => void): Promise<void> {
  const t = tauri();
  if (!t) return;
  await t.event.listen(event, (e) => handler(e.payload));
}

export function errText(e: unknown): string {
  const raw = typeof e === 'string' ? e : e instanceof Error ? e.message : String(e);
  const msg = raw.trim();
  // B2 兜底：把底层技术性英文报错翻译成用户能懂的中文
  if (/invalid args/.test(msg)) return '内部参数错误：请重启应用；若反复出现请更新到最新版本';
  if (/command .* not found/i.test(msg)) return '该功能未注册：请更新到最新版本后重试';
  if (!/[\u4e00-\u9fff]/.test(msg)) return `操作失败：${msg}`;
  return msg;
}
