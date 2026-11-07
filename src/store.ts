import { reactive } from 'vue';
import { errText } from './api/tauri';

/** 全局状态：状态栏消息、平台、版本、视图切换 */
export const store = reactive({
  status: '就绪',
  statusOk: true,
  platform: '',
  version: '',
  busy: false,
  // 错误横幅：状态栏一行放不下长错误（会被省略号截断，关键操作提示看不见），
  // 出错时弹完整横幅；action 是横幅里可选的修复按钮（如「以管理员身份重启」）
  errorOpen: false,
  action: null as null | { label: string; run: () => void },
});

export function setStatus(msg: string, ok = true): void {
  store.status = msg;
  store.statusOk = ok;
  if (ok) {
    store.errorOpen = false;
    store.action = null;
  } else {
    store.errorOpen = true;
    store.action = null; // 新错误不残留旧动作按钮；UAC 取消的重试由 run 内部恢复
  }
}

export function dismissError(): void {
  store.errorOpen = false;
  store.action = null;
}

/** 执行一个动作并自动维护状态栏与 busy 标记 */
export async function act<T>(fn: () => Promise<T>, okMsg: string | ((r: T) => string)): Promise<T | undefined> {
  if (store.busy) return undefined;
  store.busy = true;
  try {
    const r = await fn();
    setStatus(typeof okMsg === 'function' ? okMsg(r) : okMsg, true);
    return r;
  } catch (e) {
    setStatus(errText(e), false);
    return undefined;
  } finally {
    store.busy = false;
  }
}

/** 动作失败且原因指向权限不足时，在错误横幅里追加「以管理员身份重启」按钮（Windows） */
export function offerAdminRelaunch(): void {
  if (store.statusOk || !store.status.includes('管理员')) return;
  store.action = {
    label: '以管理员身份重启',
    run: async () => {
      const retry = store.action; // setStatus 成功消息会清掉 action；取消授权时恢复按钮
      setStatus('正在请求管理员权限：请在弹出的授权窗口中点「是」…', true);
      const { invoke } = await import('./api/tauri');
      try {
        await invoke('relaunch_as_admin'); // 成功后后端直接退出本实例，由提权实例接管
      } catch (e) {
        setStatus(errText(e), false);
        store.action = retry;
      }
    },
  };
}
