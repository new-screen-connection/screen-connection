/**
 * 解析 hdr_states 输出（真实后端格式，C# HDRStates 生成）：
 *   行格式 H|<dev>|<是否支持(0/1)>|<是否已启用(0/1)>
 * 历史教训：早期解析按 mock 的「1:on」格式写，真实后端永远解析成「未开启」，
 * 用户开了 HDR 界面也显示「HDR 未开启」（2026-11-07 用户真机反馈）。
 */
export function parseHdrStates(raw: string): { supported: number; on: number } {
  let supported = 0;
  let on = 0;
  for (const line of String(raw).split('\n')) {
    const p = line.trim().split('|');
    if (p.length < 4 || p[0] !== 'H') continue;
    if (p[2] === '1') supported++;
    if (p[2] === '1' && p[3] === '1') on++;
  }
  return { supported, on };
}
