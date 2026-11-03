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
