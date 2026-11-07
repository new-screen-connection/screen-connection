/**
 * Agent 对话通道（P2-1 排障助手）。
 * 分层顺序不能反：
 * ① 排障类输入（"副屏又黑了"）→ 先调 troubleshoot_diag 跑体检，直接给结构化回答（本地、离线）；
 * ② 控制类输入（调亮度/音量/铺满/熄屏）→ 本地规则引擎直接执行；
 * ③ 可选 LLM 兜底由 ChatPanel 在本地无法定位时征得用户同意后单独调用（agent_llm_chat）。
 * 命令未实现或浏览器预览时回落到本地模拟（mock.ts 的 troubleshoot_diag）。
 */
import { invoke, isTauri } from './tauri';
import { parseHdrStates } from './hdr';
import { CMD, type DiagReply } from '../types';

export interface ChatMsg {
  role: 'user' | 'agent';
  text: string;
  ts: number;
  /** 结构化排障回答（有则渲染原因卡片） */
  diag?: DiagReply;
  /** 联网分析同意卡（有则渲染「同意发送/取消」按钮） */
  llmAsk?: { payload: string; question: string };
}

/** 预置的快捷提问（展示在对话窗空态，控制类） */
export const CHAT_SUGGESTIONS = [
  '把两个屏幕亮度调到 60',
  '现在显示器什么状态',
  '帮我把窗口铺满两个屏幕',
  '1 分钟后熄屏',
];

/** 排障快捷入口（输入框上方常驻，排障类） */
export const TROUBLE_QUICK_ENTRIES = ['屏幕检测', '为什么只有 60Hz', '亮度调不了'] as const;

/** 排障意图的本地预判（与后端 classify 同源的关键词；命中就走排障通道，宁可多走一次体检） */
export function looksLikeTrouble(text: string): boolean {
  const t = text.trim().toLowerCase();
  const has = (...keys: string[]) => keys.some((k) => t.includes(k));
  // 控制命令优先排除：「亮度 60」「调到 60」这类带数字目标的不是抱怨
  const isSetCmd =
    /\d{1,3}\s*(%|的亮度|$)/.test(t) && !has('调不了', '没反应', '无效', '失灵', '不起作用', '不生效');
  if (isSetCmd) return false;
  return (
    has('黑屏', '不亮', '黑了', '点不亮', '没信号', '无信号', '画面没了', '一片黑', '全黑') ||
    has('不识别', '识别不到', '识别不出', '检测不到', '检测不出', '找不到', '认不到', '掉屏', '掉了', '消失', '不见了', '没出现', '连不上', '连接不上', '少一个屏', '少一块屏') ||
    has('分辨率', '字糊', '模糊', '发虚', '锯齿', '拉伸', '变形', '比例不对') ||
    t.includes('刷新率') || t.includes('hz') || t.includes('赫兹') ||
    (has('亮度') && has('调不了', '不能调', '没反应', '无效', '失灵', '不起作用', '不生效', '不管用', '锁死')) ||
    has('偏色', '偏黄', '偏蓝', '偏绿', '偏红', '发黄', '发蓝', '发绿', '色差', '色准', '泛白', '颜色', '色彩') ||
    has('切换', '切回', '切过去', '换了电脑', '换电脑', '另一台', '对端', 'kvm', '接回来') ||
    t === '屏幕检测' || has('体检', '诊断', '排障')
  );
}

/** 排障诊断：跑一次真实体检并给出原因卡片（第一、二层，完全离线） */
export async function troubleshootDiag(text: string): Promise<DiagReply> {
  return invoke<DiagReply>(CMD.troubleshootDiag, { text });
}

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
    const on = parseHdrStates(s).on > 0;
    await invoke('hdr_set', { on: !on });
    return on ? '已关闭全部屏幕 HDR。' : '已开启全部屏幕 HDR。';
  }
  if (/电源联动/.test(t)) {
    const cur = await invoke<boolean>('get_power_link_preference');
    await invoke('set_power_link_preference', { enabled: !cur });
    return cur ? '电源联动已关闭。' : '电源联动已开启。';
  }

  // 兜底：说明能力边界（不做通用 AI 聊天）
  return '我只管屏幕这件事。\n排障可以直接说，比如「副屏又黑了」「为什么只有 60Hz」「亮度调不了」，我会先跑体检再给结论。\n控制类指令：\n- "把亮度调到 50"\n- "系统音量 30" / "静音"\n- "现在显示器什么状态"\n- "把窗口铺满两个屏幕"\n- "熄屏" / "唤醒"';
}

/** 一条消息的处理结果：纯文本回复 或 结构化排障回答 */
export type AgentReply = { text: string; diag?: DiagReply };

export async function sendAgentMessage(history: ChatMsg[], text: string): Promise<AgentReply> {
  // ① 排障通道：不反问，直接跑体检（产品性格是"先动手"）
  if (looksLikeTrouble(text)) {
    try {
      const diag = await troubleshootDiag(text);
      return { text: '', diag };
    } catch {
      // 排障命令失败时继续走控制通道兜底
    }
  }

  // ② 控制通道：将来后端实现 agent_chat 命令即自动切换；未注册时回落本地规则引擎
  if (isTauri()) {
    try {
      const reply = await invoke<string>('agent_chat', { history, text });
      if (reply) return { text: reply };
    } catch {
      /* 命令未注册时回落规则引擎 */
    }
  }
  return { text: await ruleEngine(text) };
}
