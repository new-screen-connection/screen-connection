/** 与 Rust 端 serde 结构一一对应的类型定义（勿随意改动字段名） */

export interface DisplayInfo {
  id: string;
  name: string;
  resolution: string;
  hz: number | null;
  main: boolean;
  connected: boolean;
  ddc: boolean;
  brightness: number | null;
  volume: number | null;
  color_profile: string | null;
}

export interface DisplayMode {
  mode: number;
  resolution: string;
  hz: number;
  color_depth: number;
  scaled: boolean;
  current: boolean;
}

export interface BatteryInfo {
  name: string;
  percent: number;
  charging: boolean;
  on_ac: boolean;
  conn: string;
  health: number;
  cap_mwh: number;
  full_mwh: number;
}

export interface AudioEndpoint {
  id: string;
  name: string;
  volume: number;
  mute: boolean;
  adjustable: boolean;
  form_factor: number;
  selected: boolean;
  is_default: boolean;
}

export interface SystemAudio {
  name: string;
  volume: number;
  mute: boolean;
  adjustable: boolean;
  form_factor: number;
}

export interface AlignReport {
  text: string;
  suggestion?: string;
}

export interface DiagReport {
  ok: boolean;
  text: string;
}

/** 体检检查项严重程度：ok / info / warn / error */
export type Severity = 'ok' | 'info' | 'warn' | 'error';

/** 一条检查结果（与 Rust HealthCheck 对应；id 稳定，如 refresh_below_max） */
export interface HealthCheck {
  id: string;
  severity: Severity;
  title: string;
  detail: string;
  evidence: string[];
  fix_ids: string[];
}

/** 单块屏体检结果 */
export interface DisplayHealth {
  display_id: string;
  name: string;
  native_resolution: string | null;
  current_resolution: string;
  max_hz: number | null;
  current_hz: number | null;
  ddc_capable: boolean;
  ddc_readable: boolean;
  color_profile: string | null;
  hdr: boolean | null;
  connected: boolean;
  checks: HealthCheck[];
  /** 显示链路拓扑（GPU→接口→线缆→扩展坞→显示器）；未在活动路径上为 null */
  topology: Topology | null;
}

/** 拓扑跳类别：gpu / port / cable / hub / display */
export type HopKind = 'gpu' | 'port' | 'cable' | 'hub' | 'display';

/** 拓扑跳状态：ok=确认无问题 / suspect=疑似问题 / unknown=系统未提供 */
export type HopStatus = 'ok' | 'suspect' | 'unknown';

/** 链路上的一跳 */
export interface TopologyHop {
  kind: HopKind;
  label: string;
  detail: string | null;
  status: HopStatus;
}

/** 单屏链路拓扑 */
export interface Topology {
  hops: TopologyHop[];
  note: string | null;
}

export interface HealthSummary {
  errors: number;
  warnings: number;
  infos: number;
  headline: string;
}

/** 完整体检报告 */
export interface HealthReport {
  generated_at: string;
  platform: string;
  app_version: string;
  os_version: string;
  displays: DisplayHealth[];
  summary: HealthSummary;
}

/** 修复动作结果：applied=已执行且回读核验 / not_applicable=此平台或此屏做不到 / failed=试了但没成 */
export type RepairResult = 'applied' | 'not_applicable' | 'failed';

/** 一个修复动作的执行结果（后端统一返回） */
export interface RepairOutcome {
  action: string;
  result: RepairResult;
  message: string;
  /** 回读核验后实际发生变化的项（如「显示器数量 1 → 2」） */
  changed: string[];
}

/** 会临时改变显示状态、需要前端二次确认的动作（与后端 REPAIR_NEEDS_CONFIRM 对应） */
export const REPAIR_NEEDS_CONFIRM: ReadonlySet<string> = new Set([
  'reapply_mode',
  'switch_input',
  'ddc_power_cycle',
]);

/** 修复动作的中文名与说明（体检页修复按钮用） */
export const REPAIR_LABELS: Record<string, { label: string; desc: string }> = {
  rescan_displays: { label: '重新枚举显示器', desc: '让系统重新找一遍所有显示器，新接的屏没出现时先试这个' },
  force_redetect: { label: '强制重识别', desc: '按当前拓扑重新应用显示配置并重新枚举模式，花屏/黑屏常用' },
  reapply_mode: { label: '重设当前模式', desc: '把当前分辨率/刷新率原样重放一遍，完成一次完整信号握手' },
  reset_graphics_driver: { label: '重置显卡驱动', desc: '官方快捷键软重置显卡驱动，不改任何设置，屏幕会黑几秒' },
  switch_input: { label: '切换输入源', desc: '通过 DDC/CI 让显示器切换到指定输入源' },
  ddc_power_cycle: { label: '显示器软关再开', desc: '让显示器软关再开，强制面板重新握手，屏幕会黑几秒' },
  reopen_ddc_channel: { label: '重建 DDC/CI 通道', desc: '重新探测 DDC/CI 通道，亮度/音量控制失灵时用' },
};

/** 输入源可选值（switch_input 的 value），编号与 Windows/macOS 共用 */
export const INPUT_SOURCES: { value: string; label: string }[] = [
  { value: 'displayport', label: 'DisplayPort' },
  { value: 'hdmi1', label: 'HDMI 1' },
  { value: 'hdmi2', label: 'HDMI 2' },
  { value: 'usbc', label: 'USB-C' },
];

/** 场景档（P0-5）：一组显示相关配置的命名快照 */
export interface Profile {
  id: string;
  name: string;
  created_at: string;
  updated_at: string;
  targets: ProfileTargets;
  match?: ProfileMatch | null;
  auto: boolean;
}

export interface ProfileTargets {
  displays: DisplayTarget[];
  main_display: string | null;
  audio_device_id: string | null;
  hdr: boolean | null;
}

/** 单屏目标；所有字段可选，应用时存了什么应用什么 */
export interface DisplayTarget {
  edid_key: string | null;
  captured_id: string | null;
  resolution: string | null;
  hz: number | null;
  rotation: number | null;
  brightness: number | null;
  volume: number | null;
  color_space: string | null;
}

export interface ProfileMatch {
  require_all_displays: boolean;
  docking: boolean | null;
  on_ac: boolean | null;
  /** 时间段触发，形如 "09:00-19:00"（支持跨零点；空/非法 = 不限） */
  time_range: string | null;
}

/** 应用过程的一步（want/got 都是人话） */
export interface ApplyStep {
  target: string;
  field: string;
  want: string;
  got: string;
  ok: boolean;
  error: string | null;
}

/** 应用结果：ok=false 且 rolled_back=true 表示已整体回滚 */
export interface ApplyReport {
  ok: boolean;
  rolled_back: boolean;
  steps: ApplyStep[];
}

/** 自动触发源开关（P1-1） */
export interface AutoSources {
  displays_changed: boolean;
  on_ac: boolean;
  time_range: boolean;
}

/** 最近一次自动应用记录 */
export interface AutoLastApplied {
  profile_id: string;
  profile_name: string;
  at: string;
  reason: string;
  ok: boolean;
}

/** 自动触发配置（后端 2 秒轮询引擎读这份配置） */
export interface AutoConfig {
  enabled: boolean;
  sources: AutoSources;
  quiet_minutes: number;
  /** 暂停截止时间（Unix 秒）；null = 未暂停 */
  paused_until_epoch: number | null;
  last_applied: AutoLastApplied | null;
}

/** 自动应用事件（screens://auto-applied 推送载荷） */
export interface AutoEvent {
  kind: 'applied' | 'rolled_back' | 'skipped';
  profile_id: string;
  profile_name: string;
  reason: string;
  message: string;
}

/** 软 KVM 已配对的对端（P1-2） */
export interface KvmPeer {
  id: string;
  name: string;
  addr: string;
  /** sha256(psk) 前 8 字节 hex，供两端人工比对 */
  fingerprint: string;
  /** 本机在共享屏上的输入口 */
  my_source: string;
  paired_at: string;
}

/** 局域网里发现的未配对设备 */
export interface KvmDevice {
  name: string;
  ip: string;
}

export interface KvmDevices {
  me: string;
  peers: KvmPeer[];
  discovered: KvmDevice[];
}

/** 一键切换结果 */
export interface KvmSwitchReport {
  ack_ok: boolean;
  ack_detail: string;
  input_ok: boolean;
  input_msg: string;
  audio_msg: string;
  clip_msg: string;
}

/** kvm-status 事件载荷 */
export interface KvmEvent {
  stage: 'progress' | 'done' | 'partial' | 'failed' | 'info';
  message: string;
}

/** KVM 输入口选项 */
export const KVM_SOURCES = [
  { value: 'dp', label: 'DisplayPort' },
  { value: 'hdmi1', label: 'HDMI 1' },
  { value: 'hdmi2', label: 'HDMI 2' },
  { value: 'usbc', label: 'USB-C' },
] as const;

/** 所有前端会调用到的后端命令名，集中登记避免拼写漂移 */
export const CMD = {
  getPlatform: 'get_platform',
  getVersion: 'get_version',
  getDisplays: 'get_displays',
  getDisplayModes: 'get_display_modes',
  setDisplayMode: 'set_display_mode',
  displayAction: 'display_action',
  setBrightness: 'set_brightness',
  gammaGet: 'gamma_get',
  gammaSet: 'gamma_set',
  hdrStates: 'hdr_states',
  hdrSet: 'hdr_set',
  applyColorSpace: 'apply_color_space',
  alignReport: 'align_report',
  alignApply: 'align_apply',
  alignRestore: 'align_restore',
  matchPpi: 'match_ppi',
  matchDpiApply: 'match_dpi_apply',
  matchDpiRestore: 'match_dpi_restore',
  rotateSecondary: 'rotate_secondary',
  restoreSecondary: 'restore_secondary',
  listWindows: 'list_windows',
  spanWindow: 'span_window',
  spanForeground: 'span_foreground',
  restoreForeground: 'restore_foreground',
  screenOff: 'screen_off',
  screenWake: 'screen_wake',
  getDisplayPower: 'get_display_power',
  setDisplayPower: 'set_display_power',
  setSystemMute: 'set_system_mute',
  adbStatus: 'adb_status',
  adbConnect: 'adb_connect',
  adbDownload: 'adb_download',
  repairDisplay: 'repair_display',
  displaySnapshot: 'display_snapshot',
  openMain: 'open_main',
  hidePanel: 'hide_panel',
  quitApp: 'quit_app',
  agentChat: 'agent_chat',
  getPowerLinkPreference: 'get_power_link_preference',
  setPowerLinkPreference: 'set_power_link_preference',
  hotkeyStart: 'hotkey_start',
  openControlPanel: 'open_control_panel',
  audioEndpoints: 'audio_endpoints',
  getSystemAudio: 'get_system_audio',
  setSystemVolume: 'set_system_volume',
  setAudioDevice: 'set_audio_device',
  mitvAvailable: 'mitv_available',
  mitvVolumeGet: 'mitv_volume_get',
  mitvVolumeSet: 'mitv_volume_set',
  getBatteries: 'get_batteries',
  diagScan: 'diag_scan',
  diagFix: 'diag_fix',
  healthReport: 'health_report',
  healthReportMarkdown: 'health_report_markdown',
  repairAction: 'repair_action',
  profilesList: 'profiles_list',
  profileCapture: 'profile_capture',
  profileApply: 'profile_apply',
  profileUpdate: 'profile_update',
  profileDelete: 'profile_delete',
  profileReorder: 'profile_reorder',
  profileExportJson: 'profile_export_json',
  profileImportJson: 'profile_import_json',
  autoRulesGet: 'auto_rules_get',
  autoRulesSet: 'auto_rules_set',
  autoPause: 'auto_pause',
  kvmPairStart: 'kvm_pair_start',
  kvmPairJoin: 'kvm_pair_join',
  kvmDevices: 'kvm_devices',
  kvmPeerRemove: 'kvm_peer_remove',
  kvmPeerSetSource: 'kvm_peer_set_source',
  kvmUnpairAll: 'kvm_unpair_all',
  kvmSwitch: 'kvm_switch',
  diagLogPath: 'diag_log_path',
  openLogDir: 'open_log_dir',
  appStateJson: 'app_state_json',
  exportUserSettingsJson: 'export_user_settings_json',
  importUserSettingsJson: 'import_user_settings_json',
  restoreDefaults: 'restore_defaults',
  setRevertOnExit: 'set_revert_on_exit',
  openExternalPage: 'open_external_page',
  getAutostart: 'get_autostart',
  setAutostart: 'set_autostart',
  healthGetConfig: 'health_get_config',
  healthSetConfig: 'health_set_config',
  healthSupported: 'health_supported',
  getState: 'get_state',
} as const;

export type CmdName = (typeof CMD)[keyof typeof CMD];
