/**
 * DDC 写入串行化 + 合并最新值。
 * 经验来自旧版踩坑：拖动滑块时如果每个中间值都发起一次后端调用，
 * Windows 下每次都会起 PowerShell 子进程，拖动即卡死。
 * 这里保证每类写入同一时刻最多 1 个在跑，中间值被覆盖、最终值保证送达。
 */
type Pending = { latest: number | null; busy: boolean };

const queues = new Map<string, Pending>();

export function sendVCP(
  kind: 'brightness' | 'volume',
  displayId: string,
  value: number,
  send: (cmd: string, args: Record<string, unknown>) => Promise<unknown>,
): void {
  const key = kind + ':' + displayId;
  let st = queues.get(key);
  if (!st) {
    st = { latest: null, busy: false };
    queues.set(key, st);
  }
  st.latest = value;
  if (st.busy) return;
  st.busy = true;
  void (async () => {
    const cmd = kind === 'brightness' ? 'set_brightness' : 'set_volume';
    while (st!.latest !== null) {
      const v = st!.latest;
      st!.latest = null;
      try {
        await send(cmd, { displayId, value: v });
      } catch {
        // 忽略瞬时失败：DDC 短暂超时不应打断用户拖动
      }
    }
    st!.busy = false;
  })();
}
