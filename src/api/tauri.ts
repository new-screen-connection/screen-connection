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
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  return String(e);
}
