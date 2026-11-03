/**
 * Agent 对话通道。
 * 契约：后端将来实现 `agent_chat` 命令（入参 history + text，返回回复文本），
 * 前端只依赖这一个函数；命令未实现或浏览器预览时回落到本地规则引擎，
 * 规则引擎直接调用现有命令，保证对话能真正操控软件。
 */
import { invoke, isTauri } from './tauri';

export interface ChatMsg {
  role: 'user' | 'agent';
  text: string;
  ts: number;
}

/** 预置的快捷提问（展示在对话窗空态） */
export const CHAT_SUGGESTIONS = [
  '把两个屏幕亮度调到 60',
  '现在显示器什么状态',
  '帮我把窗口铺满两个屏幕',
  '1 分钟后熄屏',
];

async function ruleEngine(text: string): Promise<string> {
  const t = text.trim();

  // 亮度：抓取 0-100 数字，支持"调到 xx / 设为 xx / xx 亮度"
  const briMatch = t.match(/(?:亮度|调到|设为|设置成)\s*(\d{1,3})|(\d{1,3})\s*(?:的)?(?:亮度)/);
  if (/亮度/.test(t) && briMatch) {
    const v = Math.min(100, Math.max(0, Number(briMatch[1] ?? briMatch[2])));
    const ds = await invoke<{ id: string; name: string; ddc: boolean }[]>('get_displays');
    const targets = (ds ?? []).filter((d) => d.ddc);
    if (!targets.length) return '当前没有支持亮度调节的显示器（需要 DDC/CI 支持）。';
    for (const d of targets) await invoke('set_brightness', { displayId: d.id, value: v });
    return `已把 ${targets.length} 台显示器亮度调整到 ${v}%。`;
  }

  // 音量
  const volMatch = t.match(/(?:音量|调到|设为)\s*(\d{1,3})|(\d{1,3})\s*(?:的)?(?:音量)/);
  if (/音量/.test(t) && volMatch) {
    const v = Math.min(100, Math.max(0, Number(volMatch[1] ?? volMatch[2])));
    await invoke('set_system_volume', { value: v });
    return `系统音量已调整到 ${v}%。`;
  }
  if (/静音/.test(t)) {
    await invoke('set_system_mute', { on: true });
    return '已静音。';
  }

  // 显示器状态
  if (/状态|怎么样|几台|信息|分辨率/.test(t)) {
    const ds = await invoke<{ name: string; resolution: string; hz: number | null; main: boolean; brightness: number | null }[]>(
      'get_displays',
    );
    if (!ds?.length) return '没有检测到显示器。';
    const lines = ds.map(
      (d) => `${d.main ? '主屏' : '副屏'} ${d.name}：${d.resolution}${d.hz ? ` @${d.hz}Hz` : ''}${d.brightness !== null ? `，亮度 ${d.brightness}%` : ''}`,
    );
    return `当前共 ${ds.length} 台显示器：\n${lines.join('\n')}`;
  }

  // 铺满
  if (/铺满|跨屏|双屏看/.test(t)) {
    return String(await invoke('span_foreground'));
  }
  if (/还原窗口|取消铺满/.test(t)) {
    await invoke('restore_foreground');
    return '已还原窗口。';
  }

  // 电源
  if (/熄屏|关闭屏幕|休眠/.test(t)) {
    await invoke('screen_off');
    return '好的，屏幕已熄灭，动一下鼠标即可唤醒。';
  }
  if (/唤醒|点亮/.test(t)) {
    await invoke('screen_wake');
    return '已唤醒屏幕。';
  }
  if (/旋转|横竖/.test(t)) {
    await invoke('rotate_secondary');
    return '副屏已旋转。';
  }
  if (/HDR/.test(t.toUpperCase())) {
    const s = await invoke<string>('hdr_states');
    const on = /:on/i.test(s);
    await invoke('hdr_set', { on: !on });
    return on ? '已关闭全部屏幕 HDR。' : '已开启全部屏幕 HDR。';
  }
  if (/电源联动/.test(t)) {
    const cur = await invoke<boolean>('get_power_link_preference');
    await invoke('set_power_link_preference', { enabled: !cur });
    return cur ? '电源联动已关闭。' : '电源联动已开启。';
  }

  // 兜底：说明能力
  return '我可以帮你控制这台软件的所有功能，例如：\n- "把亮度调到 50"\n- "系统音量 30" / "静音"\n- "现在显示器什么状态"\n- "把窗口铺满两个屏幕"\n- "熄屏" / "唤醒"\n- "旋转副屏"\n- "打开 HDR" / "打开电源联动"';
}

export async function sendAgentMessage(history: ChatMsg[], text: string): Promise<string> {
  // 真实后端实现后走命令通道；未注册时回落本地规则引擎
  if (isTauri()) {
    try {
      const reply = await invoke<string>('agent_chat', { history, text });
      if (reply) return reply;
    } catch {
      /* 命令未注册时回落规则引擎 */
    }
  }
  return ruleEngine(text);
}
