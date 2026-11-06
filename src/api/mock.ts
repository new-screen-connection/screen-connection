/**
 * 浏览器预览用的模拟后端。
 * 在 Tauri WebView 内不生效；`npm run dev` 直接在浏览器打开时可预览全部界面与交互。
 */
import type { AudioEndpoint, BatteryInfo, DisplayInfo, HealthReport, PortableSection, Topology } from '../types';
import type { ApplyReport, AutoConfig, KvmPeer, MigrateBundle, Profile } from '../types';

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** 自动触发配置假数据（浏览器预览） */
const mockAuto: AutoConfig = {
  enabled: false,
  sources: { displays_changed: true, on_ac: false, time_range: false },
  quiet_minutes: 5,
  paused_until_epoch: null,
  last_applied: null,
};

/** 软 KVM 假对端（浏览器预览） */
const mockKvmPeers: KvmPeer[] = [
  {
    id: 'mock-peer-1',
    name: 'MacBook Pro',
    addr: '192.168.1.23:47822',
    fingerprint: '9f8e7d6c5b4a3210',
    my_source: 'dp',
    paired_at: '2026-11-05 21:02:14',
  },
];

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

/** 场景档假数据：两个档位（浏览器预览用） */
const mockProfiles: Profile[] = [
  {
    id: 'mock-p1',
    name: '工作模式',
    created_at: '2026-11-03 09:12:00',
    updated_at: '2026-11-03 09:12:00',
    targets: {
      displays: [
        { edid_key: 'JKA27B3', captured_id: 'mock-1', resolution: '3840x2160', hz: 60, rotation: 0, brightness: 65, volume: null, color_space: 'sRGB IEC61966-2.1' },
        { edid_key: null, captured_id: 'mock-2', resolution: '1920x1080', hz: 60, rotation: 3, brightness: 90, volume: 20, color_space: null },
      ],
      main_display: 'mock-1',
      audio_device_id: 'ep-0',
      hdr: false,
    },
    auto: false,
  },
  {
    id: 'mock-p2',
    name: '观影模式',
    created_at: '2026-11-03 21:40:00',
    updated_at: '2026-11-03 21:40:00',
    targets: {
      displays: [
        { edid_key: 'JKA27B3', captured_id: 'mock-1', resolution: '3840x2160', hz: 60, rotation: 0, brightness: 30, volume: null, color_space: 'Display P3' },
      ],
      main_display: 'mock-1',
      audio_device_id: 'ep-1',
      hdr: true,
    },
    auto: false,
  },
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
        {
          id: 'portable_power_budget',
          severity: 'warn',
          title: '亮度已到上限，屏幕仍偏暗（供电可能不足）',
          detail:
            '亮度调到最大仍然偏暗，常见于一线通供电不足：背光功率上不去，驱动板主动压低亮度来保证供电稳定。这是供电问题，软件改不了，建议改接线方式。',
          evidence: [
            '当前亮度 = 100（已到上限）',
            '面板 VCP 0x10 上报范围 0–100',
            '识别依据：EDID 上报物理尺寸约 16.0 英寸（< 18 英寸）',
          ],
          fix_ids: [],
        },
        {
          id: 'usbc_single_cable',
          severity: 'info',
          title: 'USB-C 一线通同时供电与传视频',
          detail:
            '一根 C 线同时取电和传视频时，开启 HDR 或高刷会挤占背光功率，出现亮度下降或闪屏。',
          evidence: ['连接类型 = DisplayPort（USB-C 一线通通常呈现为 DP）'],
          fix_ids: [],
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
  portable: [
    {
      display_id: 'mock-health-2',
      name: '16 英寸便携副屏',
      conclusion: '这块屏按便携屏识别：亮度已到上限仍偏暗，多半是一线通供电不足限制了背光。',
      advice: [
        '便携屏接专用电源适配器，只用 C 线传视频',
        '改用带独立供电的 USB-C 扩展坞',
        '换用支持 DP Alt Mode 的全功能 C 线（当前线材可能只有 USB 2.0 数据能力）',
      ],
      evidence: [
        '识别依据：EDID 上报物理尺寸约 16.0 英寸（< 18 英寸）',
        '当前亮度 100（面板上报上限 100）',
      ],
    },
  ] satisfies PortableSection[],
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
    case 'repair_action': {
      // 假数据：演示三种结果——reopen 返回成功+变化，其余动作返回通用的已执行
      const action = String(args?.action ?? '');
      const MESSAGES: Record<string, { result: 'applied' | 'not_applicable' | 'failed'; message: string; changed: string[] }> = {
        rescan_displays: { result: 'applied', message: '已触发系统重新枚举显示器，当前 2 台在活动路径上。新接的屏没出现的话，检查线缆两端是否插紧。', changed: [] },
        force_redetect: { result: 'applied', message: '已强制系统按当前拓扑重新应用显示配置并重新枚举模式。花屏/黑屏通常在这一步恢复。', changed: [] },
        reapply_mode: { result: 'applied', message: '已按当前模式 3840x2160@60 强制重新应用一遍，回读核验一致。信号链路完成了一次完整握手。', changed: ['模式重设为 3840x2160@60'] },
        reset_graphics_driver: { result: 'applied', message: '已发送显卡驱动重置（屏幕黑几秒属正常）。花屏/色彩异常/抖动应已恢复；若没有改善，改试「重设当前模式」。', changed: [] },
        switch_input: { result: 'applied', message: `已切到 ${String(args?.value ?? 'displayport')} 并经显示器确认。`, changed: [`输入源 → ${String(args?.value ?? 'displayport')}`] },
        ddc_power_cycle: { result: 'applied', message: '已让显示器软关再开，面板完成了一次重新握手。', changed: ['显示器电源状态 → 开'] },
        reopen_ddc_channel: { result: 'applied', message: '已重建 DDC/CI 通道并回读成功（当前亮度 65）。屏幕亮度/音量控制现在应该可用了。', changed: ['DDC/CI 通道 不可用 → 可用'] },
      };
      const m = MESSAGES[action] ?? { result: 'not_applicable' as const, message: `未知的修复动作 ${action}`, changed: [] };
      return { action, ...m } as T;
    }
    // ---------- 场景档（P0-5）：两个假档位，可点、可看结果 ----------
    case 'profiles_list':
      return JSON.parse(JSON.stringify(mockProfiles)) as T;
    case 'profile_capture': {
      const name = String(args?.name ?? '新档位');
      const p: Profile = {
        id: 'mock-' + Date.now(),
        name,
        created_at: '刚刚',
        updated_at: '刚刚',
        targets: {
          displays: [
            { edid_key: 'JKA27B3', captured_id: 'mock-1', resolution: '3840x2160', hz: 60, rotation: 0, brightness: 65, volume: null, color_space: 'sRGB IEC61966-2.1' },
            { edid_key: null, captured_id: 'mock-2', resolution: '1920x1080', hz: 60, rotation: 0, brightness: 80, volume: 30, color_space: null },
          ],
          main_display: 'mock-1',
          audio_device_id: 'ep-0',
          hdr: false,
        },
        auto: false,
      };
      mockProfiles.push(JSON.parse(JSON.stringify(p)));
      return p as T;
    }
    case 'profile_apply': {
      const id = String(args?.id ?? '');
      const p = mockProfiles.find((x) => x.id === id);
      const report: ApplyReport = {
        ok: true,
        rolled_back: false,
        steps: [
          { target: p?.name ?? '场景档', field: '分辨率', want: '3840x2160@60', got: '3840x2160@60', ok: true, error: null },
          { target: p?.name ?? '场景档', field: '亮度', want: '65%', got: '65%', ok: true, error: null },
          { target: '全局', field: '音频设备', want: '扬声器 (Realtek Audio)', got: '扬声器 (Realtek Audio)', ok: true, error: null },
        ],
      };
      return report as T;
    }
    case 'profile_update': {
      const id = String(args?.id ?? '');
      const p = mockProfiles.find((x) => x.id === id);
      if (!p) throw new Error(`未找到场景档 ${id}`);
      if (args?.name) p.name = String(args.name);
      if (args?.recapture) {
        for (const d of p.targets.displays) d.brightness = 60 + Math.floor(Math.random() * 30);
      }
      if (typeof args?.auto === 'boolean') p.auto = args.auto;
      if (args?.match !== undefined) p.match = args.match as Profile['match'];
      p.updated_at = '刚刚';
      return JSON.parse(JSON.stringify(p)) as T;
    }
    case 'profile_delete': {
      const id = String(args?.id ?? '');
      const idx = mockProfiles.findIndex((x) => x.id === id);
      if (idx < 0) throw new Error(`未找到场景档 ${id}`);
      mockProfiles.splice(idx, 1);
      return undefined as T;
    }
    case 'profile_reorder': {
      const ids = (args?.ids as string[]) ?? [];
      const ordered = ids
        .map((id) => mockProfiles.find((x) => x.id === id))
        .filter((x): x is Profile => Boolean(x));
      mockProfiles.length = 0;
      mockProfiles.push(...ordered);
      return JSON.parse(JSON.stringify(mockProfiles)) as T;
    }
    case 'profile_export_json': {
      const p = mockProfiles.find((x) => x.id === String(args?.id ?? ''));
      if (!p) throw new Error('未找到场景档');
      return JSON.stringify(p, null, 2) as T;
    }
    case 'profile_import_json': {
      const p = JSON.parse(String(args?.json ?? '{}')) as Profile;
      p.id = 'mock-' + Date.now();
      mockProfiles.push(JSON.parse(JSON.stringify(p)));
      return p as T;
    }
    // ---------- 场景自动触发（P1-1）：假配置可改、可暂停 ----------
    case 'auto_rules_get':
      return JSON.parse(JSON.stringify(mockAuto)) as T;
    case 'auto_rules_set': {
      const cfg = args?.config as AutoConfig;
      mockAuto.enabled = cfg.enabled;
      mockAuto.sources = { ...cfg.sources };
      mockAuto.quiet_minutes = cfg.quiet_minutes;
      if (!mockAuto.enabled) mockAuto.paused_until_epoch = null;
      return JSON.parse(JSON.stringify(mockAuto)) as T;
    }
    case 'auto_pause': {
      const minutes = Number(args?.minutes ?? 0);
      mockAuto.enabled = true;
      mockAuto.paused_until_epoch =
        minutes > 0 ? Math.floor(Date.now() / 1000) + minutes * 60 : null;
      return JSON.parse(JSON.stringify(mockAuto)) as T;
    }
    // ---------- 配置迁移（P1-3）：假载荷，走完 预览→导入 流程 ----------
    case 'migrate_export': {
      const pw = String(args?.password ?? '');
      if (pw.length < 6 || pw.length > 32) {
        throw new Error('口令需要 6–32 位字符');
      }
      const bundle: MigrateBundle = {
        schema: 1,
        exported_at: new Date().toLocaleString(),
        app_version: '0.3.0',
        platform: 'windows',
        profiles: JSON.parse(JSON.stringify(mockProfiles)) as Profile[],
        prefs: { revert_on_exit: true },
      };
      return ('SCMIG1:MOCK' + btoa(unescape(encodeURIComponent(JSON.stringify(bundle))))) as T;
    }
    case 'migrate_preview': {
      const payload = String(args?.payload ?? '');
      if (!payload.startsWith('SCMIG1:')) throw new Error('口令不正确或文件已损坏');
      if (String(args?.password ?? '').length < 6) throw new Error('口令不正确或文件已损坏');
      return {
        schema: 1,
        exported_at: new Date().toLocaleString(),
        app_version: '0.3.0',
        platform: 'macos',
        new_profiles: mockProfiles.slice(0, 1),
        overwrite_profiles: [],
        mismatched: ['观影模式（本机没有它使用的显示器）'],
      } as T;
    }
    case 'migrate_import': {
      if (!String(args?.payload ?? '').startsWith('SCMIG1:')) throw new Error('口令不正确或文件已损坏');
      if (String(args?.password ?? '').length < 6) throw new Error('口令不正确或文件已损坏');
      return {
        added: 1,
        overwritten: 0,
        added_names: ['工作模式'],
        overwritten_names: [],
        mismatched: ['观影模式（本机没有它使用的显示器）'],
        prefs_applied: true,
      } as T;
    }
    // ---------- 软 KVM（P1-2）：假配对可走通、假切换有进度 ----------
    case 'kvm_devices':
      return {
        me: '演示机',
        peers: mockKvmPeers,
        discovered: [
          { name: '客厅 Mac mini', ip: '192.168.1.44' },
        ],
      } as T;
    case 'kvm_pair_start':
      return String(Math.floor(100000 + Math.random() * 900000)) as T;
    case 'kvm_pair_join': {
      const peer: KvmPeer = {
        id: 'mock-' + Date.now(),
        name: '新配对的设备',
        addr: '192.168.1.50:47822',
        fingerprint: 'a1b2c3d4e5f6a7b8',
        my_source: 'hdmi1',
        paired_at: '刚刚',
      };
      mockKvmPeers.push(JSON.parse(JSON.stringify(peer)));
      return peer as T;
    }
    case 'kvm_peer_remove': {
      const id = String(args?.id ?? '');
      const i = mockKvmPeers.findIndex((p) => p.id === id);
      if (i >= 0) mockKvmPeers.splice(i, 1);
      return undefined as T;
    }
    case 'kvm_peer_set_source': {
      const p = mockKvmPeers.find((x) => x.id === String(args?.id ?? ''));
      if (p) p.my_source = String(args?.source ?? 'hdmi1');
      return undefined as T;
    }
    case 'kvm_unpair_all':
      mockKvmPeers.length = 0;
      return undefined as T;
    case 'kvm_switch':
      await wait(600);
      return {
        ack_ok: true,
        ack_detail: '音频：默认输出已切到 Mi Monitor；剪贴板：已写入',
        input_ok: true,
        input_msg: '已切到 HDMI 1 并经显示器确认。',
        audio_msg: '音频：本机默认输出已切回 扬声器 (Realtek)',
        clip_msg: '',
      } as T;
    case 'health_report_markdown':
      return (
        '# 屏幕连接助手 · 显示体检报告\n\n' +
        '| 项目 | 内容 |\n| --- | --- |\n' +
        '| 生成时间 | 2026-11-04 17:40:00 |\n' +
        '| 平台 | Windows 11 专业版 10.0.26100 |\n' +
        '| 应用版本 | v0.3.0 |\n' +
        '| 显示器数量 | 2（活动中 2） |\n' +
        '| 结论 | 2 块屏已连接，其中 1 项可优化 |\n\n' +
        '## 显示器 1：Mi Monitor\n\n' +
        '| 项目 | 内容 |\n| --- | --- |\n' +
        '| 状态 | 活动中 |\n| 当前模式 | 3840x2160 @60Hz |\n' +
        '| 原生分辨率 | 3840x2160 |\n| DDC/CI | 可识别但读不到数据 |\n' +
        '| 序列号 | 未输出（不含设备序列号） |\n\n' +
        '### 发现的问题\n\n' +
        '- **[建议] DDC/CI 通道读不到数据**\n  - 细节：亮度/音量控制依赖 DDC/CI。\n'
      ) as T;
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
