/**
 * 浏览器预览用的模拟后端。
 * 在 Tauri WebView 内不生效；`npm run dev` 直接在浏览器打开时可预览全部界面与交互。
 */
import type { AudioEndpoint, BatteryInfo, DisplayInfo } from '../types';

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

const displays: DisplayInfo[] = [
  {
    id: 'mock-1',
    name: '27 英寸 4K 主屏',
    resolution: '3840 x 2160',
    hz: 60,
    main: true,
    connected: true,
    ddc: true,
    brightness: 65,
    volume: 28,
    color_profile: 'sRGB',
  },
  {
    id: 'mock-2',
    name: '16 英寸便携副屏',
    resolution: '1920 x 1080',
    hz: 60,
    main: false,
    connected: true,
    ddc: true,
    brightness: 80,
    volume: null,
    color_profile: 'Display P3',
  },
];

const batteries: BatteryInfo[] = [
  { name: '内置电池', percent: 82, charging: false, on_ac: true, conn: '内置', health: 93, cap_mwh: 5200, full_mwh: 5600 },
  { name: 'MX Master 3S', percent: 55, charging: false, on_ac: false, conn: '蓝牙', health: -1, cap_mwh: 0, full_mwh: 0 },
];

const endpoints: AudioEndpoint[] = [
  { id: 'ep-0', name: '扬声器 (Realtek Audio)', volume: 40, mute: false, adjustable: true, form_factor: 1, selected: true, is_default: true },
  { id: 'ep-1', name: 'NE160QDM (NVIDIA HD Audio)', volume: 30, mute: false, adjustable: true, form_factor: 9, selected: false, is_default: false },
];

const windows = [
  'Microsoft Edge - 设计文档',
  'Visual Studio Code - App.vue',
  'PotPlayer - movie.mkv',
];

const state = {
  gamma: 100,
  powerLink: false,
  hotkey: 'Ctrl+Alt+D',
  version: '0.1.0 (预览数据)',
  platform: 'windows',
  mitv: false,
  autostart: false,
  revertOnExit: true,
};

const results: Record<string, string> = {
  screen_off: '已待机（动一下鼠标或按键盘即可唤醒）',
  screen_wake: '已唤醒屏幕',
  align_apply: '已应用推荐组合：副屏 1080P 60Hz，两屏逻辑缩放 150% / 100%',
  align_restore: '已还原两屏原始分辨率',
  match_ppi: '副屏已对齐 2x 逻辑档位，窗口跨屏不变小',
  match_dpi_restore: '已还原系统缩放设置',
  restore_defaults: '已恢复默认状态：亮度、色彩、缩放全部复位',
};

/** 模拟命令分发：签名与真实 invoke 一致 */
export async function mockInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  await wait(120 + Math.random() * 180);
  switch (cmd) {
    case 'get_platform':
      return state.platform as T;
    case 'get_version':
      return state.version as T;
    case 'get_displays':
      return displays.filter((d) => d.connected) as T;
    case 'set_brightness': {
      const d = displays.find((x) => x.id === args?.displayId);
      if (d) d.brightness = Number(args?.value);
      return undefined as T;
    }
    case 'gamma_get':
      return String(state.gamma) as T;
    case 'gamma_set':
      state.gamma = Number(args?.pct);
      return `总亮度已调整到 ${state.gamma}%` as T;
    case 'hdr_states':
      return '1:off;2:off' as T;
    case 'apply_color_space':
      return `已把全部屏幕对齐到 ${args?.space ?? 'sRGB'}` as T;
    case 'get_power_link_preference':
      return state.powerLink as T;
    case 'set_power_link_preference':
      state.powerLink = Boolean(args?.enabled);
      return undefined as T;
    case 'hotkey_start':
      return state.hotkey as T;
    case 'audio_endpoints':
      return endpoints as T;
    case 'get_system_audio':
      return { name: '扬声器 (Realtek Audio)', volume: 40, mute: false, adjustable: true, form_factor: 1 } as T;
    case 'set_system_volume': {
      const ep = endpoints.find((e) => e.selected) ?? endpoints[0];
      ep.volume = Number(args?.value);
      return undefined as T;
    }
    case 'mitv_available':
      return state.mitv as T;
    case 'get_batteries':
      return batteries as T;
    case 'list_windows':
      return windows.join('\n') as T;
    case 'span_window':
    case 'span_foreground':
      return '已铺满：PotPlayer - movie.mkv（模式：自动对齐两屏清晰度）' as T;
    case 'restore_foreground':
      return '已还原上一个被铺满的窗口' as T;
    case 'align_report':
      return '主屏 3840x2160 @60Hz（150%），副屏 1920x1080 @60Hz（100%）。\n建议：副屏对齐 2x 逻辑档，应用后窗口跨屏等大。' as T;
    case 'diag_scan':
      return 'DDC/CI 通道：正常\n电源联动服务：正常\nADB 桥接：未安装（可选）\n结论：2 项通过，1 项可增强' as T;
    case 'diag_log_path':
      return 'C:\\Users\\demo\\AppData\\Roaming\\ScreenConnection\\logs' as T;
    case 'export_user_settings_json':
      return JSON.stringify({ version: '0.1.0', displays: displays.map((d) => d.brightness) }) as T;
    case 'import_user_settings_json':
      return '已导入设置' as T;
    case 'get_autostart':
      return state.autostart as T;
    case 'set_autostart':
      state.autostart = Boolean(args?.on);
      return undefined as T;
    case 'app_state_json':
      return JSON.stringify({ revert_on_exit: state.revertOnExit }) as T;
    case 'set_revert_on_exit':
      state.revertOnExit = Boolean((args as { on?: boolean })?.on);
      return undefined as T;
    default:
      if (cmd in results) return results[cmd] as T;
      // 未模拟的命令一律成功返回，保证界面可走通
      return undefined as T;
  }
}
