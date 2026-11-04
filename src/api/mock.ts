/**
 * 浏览器预览用的模拟后端。
 * 在 Tauri WebView 内不生效；`npm run dev` 直接在浏览器打开时可预览全部界面与交互。
 */
import type { AudioEndpoint, BatteryInfo, DisplayInfo, HealthReport, Topology } from '../types';

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

/** 拓扑假数据：主屏 4K@144 过 HDMI → 线缆带宽反推为 suspect（含算式）；副屏全链路健康 */
const topoMain: Topology = {
  hops: [
    {
      kind: 'gpu',
      label: 'NVIDIA GeForce RTX 4070',
      detail: '驱动版本 552.44',
      status: 'ok',
    },
    {
      kind: 'port',
      label: 'HDMI',
      detail: '连接类型来自系统 DisplayConfig API（权威报告）',
      status: 'ok',
    },
    {
      kind: 'cable',
      label: '线缆',
      detail:
        '当前 3840x2160@144 所需带宽约 31.0 Gbps：HDMI 1.4、HDMI 2.0、DP 1.2、DP 1.4 承载不了，HDMI 2.1 及以上可以。所需带宽已超出该接口常见版本的上限：若出现黑屏、闪屏或最高刷新率档缺失，优先怀疑线缆/扩展坞带宽不足。算式：3840×2160×144×24bit×1.08 ≈ 31.0 Gbps',
      status: 'suspect',
    },
    {
      kind: 'hub',
      label: '扩展坞/转接',
      detail:
        '系统未提供中转设备（扩展坞/转接头）信息。设备路径：\\\\?\\DISPLAY#MIA27B3#5&2b34f0d&0&UID4353#{e6f07b5f-ee97-4a90-b076-a03e01ec6953}',
      status: 'unknown',
    },
    {
      kind: 'display',
      label: 'Mi Monitor 27',
      detail: 'EDID 厂商代码 MIA · 产品代码 0x27B3',
      status: 'ok',
    },
  ],
  note: null,
};

const topoSecond: Topology = {
  hops: [
    { kind: 'gpu', label: 'Intel Iris Xe Graphics', detail: '驱动版本 31.0.101.5333', status: 'ok' },
    { kind: 'port', label: 'DisplayPort', detail: '连接类型来自系统 DisplayConfig API（权威报告）', status: 'ok' },
    {
      kind: 'cable',
      label: '线缆',
      detail:
        '当前 1920x1080@60 所需带宽约 3.2 Gbps，低于最保守的 HDMI 1.4 上限（8.16 Gbps），常见线缆都能满足。线材规格本身系统不可读。算式：1920×1080×60×24bit×1.08 ≈ 3.2 Gbps',
      status: 'ok',
    },
    {
      kind: 'hub',
      label: '扩展坞/转接',
      detail: '系统未提供中转设备（扩展坞/转接头）信息，无法判断是否经过转接。',
      status: 'unknown',
    },
    {
      kind: 'display',
      label: '16 英寸便携副屏',
      detail: 'EDID 厂商代码 HPC · 产品代码 0x1A2C',
      status: 'ok',
    },
  ],
  note: '设备路径含 USB 节点，信号可能经过扩展坞/转接头；DDC/CI 通道可能被阻断。',
};

/** 体检假数据：主屏一条提示，副屏一条可优化 + 一条建议，另有一台掉线残留的显示器 */
const healthReport: HealthReport = {
  generated_at: '2026-11-04 16:20:00',
  platform: 'windows',
  app_version: '0.3.0',
  os_version: 'Windows 11 专业版 10.0.26100',
  displays: [
    {
      display_id: 'mock-health-1',
      name: '27 英寸 4K 主屏',
      native_resolution: '3840x2160',
      current_resolution: '3840x2160',
      max_hz: 144,
      current_hz: 144,
      ddc_capable: true,
      ddc_readable: true,
      color_profile: 'sRGB IEC61966-2.1',
      hdr: false,
      connected: true,
      topology: topoMain,
      checks: [
        {
          id: 'color_profile_generic',
          severity: 'info',
          title: '色彩配置为系统通用配置',
          detail: '当前使用通用 sRGB 配置，未针对这块屏校准。对色彩要求高时可用「画质 → 色彩同步」对齐到标准色域。',
          evidence: ['色彩配置：sRGB IEC61966-2.1'],
          fix_ids: [],
        },
      ],
    },
    {
      display_id: 'mock-health-2',
      name: '16 英寸便携副屏',
      native_resolution: '2560x1600',
      current_resolution: '1920x1080',
      max_hz: 120,
      current_hz: 60,
      ddc_capable: true,
      ddc_readable: true,
      color_profile: 'Display P3',
      hdr: null,
      connected: true,
      topology: topoSecond,
      checks: [
        {
          id: 'refresh_below_max',
          severity: 'info',
          title: '刷新率低于该分辨率下支持的最高值',
          detail: '常见原因是线材带宽不足（HDMI 1.4 / 非 DP 线）或系统里选了低档。确认线材支持后，在系统显示设置里把刷新率调到最高档。',
          evidence: ['当前 1920x1080 @ 60Hz', '该分辨率下支持最高 120Hz'],
          fix_ids: [],
        },
        {
          id: 'resolution_not_native',
          severity: 'warn',
          title: '当前分辨率不是面板原生分辨率',
          detail: '非原生分辨率会让画面经历一次缩放，文字发虚。建议切回原生分辨率。',
          evidence: [
            '当前 1920x1080@60',
            '原生 2560x1600（按支持模式列表面积最大者推断）',
            '宽高比 16:10 vs 16:10',
          ],
          fix_ids: ['rescan_displays'],
        },
      ],
    },
    {
      display_id: 'DISPLAY\\MON3A4B1',
      name: 'DELL U2415',
      native_resolution: null,
      current_resolution: '',
      max_hz: null,
      current_hz: null,
      ddc_capable: false,
      ddc_readable: false,
      color_profile: null,
      hdr: null,
      connected: false,
      topology: null,
      checks: [
        {
          id: 'display_not_active',
          severity: 'error',
          title: '显示器未在活动路径上',
          detail: '系统枚举到了这台显示器，但它不在当前活动显示路径上（常见于掉线后未恢复、KVM 切换残留）。尝试重新插拔，或点修复动作强制重新枚举。',
          evidence: ['WmiMonitorID.InstanceName = DISPLAY\\MON3A4B1（Active = false）'],
          fix_ids: ['rescan_displays', 'force_redetect'],
        },
      ],
    },
  ],
  summary: { errors: 1, warnings: 1, infos: 2, headline: '2 块屏已连接，其中 1 项需要处理' },
};

const state = {
  gamma: 100,
  powerLink: false,
  hotkey: 'Ctrl+Alt+D',
  version: 'v0.2.0',
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
    case 'health_report':
      return JSON.parse(JSON.stringify(healthReport)) as T;
    case 'diag_log_path':
      return 'C:\\Users\\demo\\AppData\\Roaming\\ScreenConnection\\logs' as T;
    case 'export_user_settings_json':
      return JSON.stringify({ version: '0.1.0', displays: displays.map((d) => d.brightness) }) as T;
    case 'import_user_settings_json':
      return '已导入设置' as T;
    case 'get_autostart':
      return state.autostart as T;
    case 'set_autostart':
      state.autostart = Boolean(args?.enabled);
      return undefined as T;
    case 'get_display_power':
      return 1 as T;
    case 'set_display_power':
      return undefined as T;
    case 'hdr_set':
      return '已把全部屏幕 HDR 切换' as T;
    case 'set_system_mute':
      return undefined as T;
    case 'adb_status':
      return { available: false, path: '', devices: [] as string[], tip: 'ADB 未安装，下载后可增强联网屏控制' } as T;
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
