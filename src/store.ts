import { reactive } from 'vue';
import { errText } from './api/tauri';

/** 全局状态：状态栏消息、平台、版本、视图切换 */
export const store = reactive({
  status: '就绪',
  statusOk: true,
  platform: '',
  version: '',
  busy: false,
});

export function setStatus(msg: string, ok = true): void {
  store.status = msg;
  store.statusOk = ok;
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
