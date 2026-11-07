//! Windows 平台实现：全部通过 PowerShell + 内嵌 C# (Win32 P/Invoke) 落地，
//! 延续项目「零额外 Rust 依赖」的风格。每个命令独立 powershell 进程，
//! C# 代码用单引号包裹传入 Add-Type（C# 内不含单引号字符）。

use super::{run_cmd, AudioEndpoint, BatteryInfo, DisplayInfo, SystemAudio};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub fn platform_name() -> &'static str {
    "windows"
}

/// 是否已有更早启动的同名实例在跑（防双开 / 防开出多个托盘图标）。
/// 用 PID 比较而不是单纯计数：两个实例几乎同时启动时会「各自都看到 2 个进程」，
/// 若只判断数量会双双退出；改为「PID 更小的那个留下」，保证恰好一个存活。
pub fn another_instance_running() -> bool {
    let me = std::process::id();
    let script = format!(
        "$p = Get-Process -Name screenguard -ErrorAction SilentlyContinue | Where-Object {{ $_.Id -lt {} }}; if ($p) {{ '1' }} else {{ '0' }}",
        me
    );
    run_cmd(PS, &["-NoProfile", "-Command", script.as_str()])
        .map(|o| o.trim() == "1")
        .unwrap_or(false)
}

const PS: &str = "powershell";

// ===== 开机自启（HKCU Run 注册表项）=====
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "Screenguard";

fn current_exe_path() -> Result<String, String> {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("无法定位程序自身路径：{}", e))
}

pub fn autostart_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：否则每次启动软件查注册表时都会闪一个终端黑框（用户反馈）
        std::process::Command::new("reg")
            .args(["query", RUN_KEY, "/v", RUN_NAME])
            .creation_flags(0x08000000)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let exe = current_exe_path()?;
    if enabled {
        // 路径必须带引号：Run 键的值含空格时（如 C:\Program Files\...）无引号会被
        // Windows 按空格切分，可能误去启动 C:\Program.exe 导致自启静默失效
        let quoted = format!("\"{}\"", exe);
        run_cmd(
            "reg",
            &["add", RUN_KEY, "/v", RUN_NAME, "/t", "REG_SZ", "/d", quoted.as_str(), "/f"],
        )
        .map(|_| ())
        .map_err(|e| format!("写入开机自启失败：{}", e.trim()))
    } else {
        // 删除注册表项（不存在时 reg delete 会报错，忽略之）
        let _ = run_cmd("reg", &["delete", RUN_KEY, "/v", RUN_NAME, "/f"]);
        Ok(())
    }
}

fn ps(script: &str) -> Result<String, String> {
    run_cmd(PS, &["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script])
}

/// 统一的 Win32 桥接 C#（显示器枚举 / DEVMODE / DDC/CI / 窗口 / 旋转）
/// 注意：此常量内禁止出现单引号字符，否则会截断 Add-Type 的引号包裹。
const CORE_CS: &str = r#"using System;
using System.Text;
using System.Runtime.InteropServices;

// DEVMODEW 不再用结构体映射，改为按固定偏移直接读写（Marshal.Read/WriteInt32）。
// 原因：① Explicit 布局只声明需要的字段时，StructureToPtr 会把未声明区间（dmDeviceName /
// dmFormName / dmICMIntent 等）清零，读改写回不干净；② Sequential 布局的字段对齐依赖
// marshaller 实现，无法在本机验证。按偏移直读直写既确定又无损。
// 偏移见 SGCore 内的 OFF_* 常量（本机 Win11 24H2 用 ctypes 实测校准，DEVMODEW 共 220 字节）。

[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct SGDISPLAY_DEVICE {
  public int cb;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string DeviceName;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceString;
  // 文档布局（DISPLAY_DEVICEW，共 840 字节）：StateFlags 紧跟 DeviceString，之后才是
  // DeviceID / DeviceKey。旧实现的「_pad 隐藏字段」是误诊——实测（ctypes 与 .NET 双路验证）
  // Win11 24H2 起本机 EnumDisplayDevicesW 对「适配器上挂的监视器」的枚举直接失败
  // （返回 FALSE），与 cb 取 840/844 无关；uid 因此改由调用方做回退处理（见 ListDisplays）。
  public int StateFlags;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceID;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceKey;
}

public struct SGRECT { public int Left, Top, Right, Bottom; }

[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct SGMONITORINFO {
  public int cb;
  public SGRECT rcMonitor;
  public SGRECT rcWork;
  public int dwFlags;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string szDevice;
}

[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct SGPHYS_MON { public IntPtr h; [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string name; }

// ---------- DisplayConfig（CCD）路径查询：GDI 设备名 → 显示器友好名 ----------
// Win11 24H2 实测：EnumDisplayDevicesW 的监视器枚举与 WMI 监视器类可能整体失效，
// DisplayConfig 是与驱动栈状态无关的权威来源。结构布局已对照本机 SDK wingdi.h 核实：
//   SOURCE_INFO=20(无 reserved！) TARGET_INFO=48 PATH_INFO=72
//   SOURCE_NAME=84(头20+WCHAR[32]) TARGET_NAME=420(头20+flags4+outputTechnology4
//   +edidManufactureId2+edidProductCodeId2+connectorInstance4+名WCHAR[64]+路径WCHAR[128])
// 路径数组按偏移直读（每条 72 字节）：
//   +0 源adapterId.Lo +4 源adapterId.Hi +8 源id +20 目标adapterId.Lo +24 目标adapterId.Hi
//   +28 目标id +68 flags(1=ACTIVE)
[StructLayout(LayoutKind.Sequential)]
public struct SGLUID { public uint LowPart; public int HighPart; }

[StructLayout(LayoutKind.Sequential)]
public struct SGDC_HEADER { public uint type; public uint size; public SGLUID adapterId; public uint id; }

[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct SGDC_SRC_NAME { public SGDC_HEADER header; [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string viewGdiDeviceName; }

[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct SGDC_DST_NAME {
  public SGDC_HEADER header; public uint flags; public uint outputTechnology;
  public ushort edidManufactureId; public ushort edidProductCodeId; public uint connectorInstance;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string monitorFriendlyDeviceName;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string monitorDevicePath;
}

public class SGCore {
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool EnumDisplayDevicesW(string dev, uint i, ref SGDISPLAY_DEVICE d, uint f);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool EnumDisplaySettingsW(string dev, int mode, IntPtr dm);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int ChangeDisplaySettingsExW(string dev, IntPtr dm, IntPtr nul, uint flags, IntPtr p);
  [DllImport("user32.dll")] static extern int GetSystemMetrics(int idx);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool GetMonitorInfo(IntPtr h, ref SGMONITORINFO mi);
  [DllImport("user32.dll")] static extern bool EnumDisplayMonitors(IntPtr h, IntPtr rc, EnumMonProc cb, IntPtr lp);
  delegate bool EnumMonProc(IntPtr h, IntPtr hdc, IntPtr rc, IntPtr lp);

  [DllImport("dxva2.dll")] static extern bool GetNumberOfPhysicalMonitorsFromHMONITOR(IntPtr h, out uint n);
  // 关键：arr 含 string 字段(非 blittable)，必须标 [Out] 才会把 API 写入的句柄/描述复制回托管端。
  // 缺 [Out] 时 .NET 只传临时副本且不回传 → 拿到无效句柄 → DDC 读写全部失败
  // （曾因此误判"显示器不支持 DDC/CI"，同机 monitorcontrol 却能正常读）。
  [DllImport("dxva2.dll")] static extern bool GetPhysicalMonitorsFromHMONITOR(IntPtr h, uint n, [Out] SGPHYS_MON[] arr);
  [DllImport("dxva2.dll")] static extern bool DestroyPhysicalMonitors(uint n, SGPHYS_MON[] arr);
  [DllImport("dxva2.dll")] static extern bool SetVCPFeature(IntPtr h, byte code, uint val);
  // 注意：真实签名是 (h, code, pvct, pdwCurrentValue, pdwMaximumValue)——两个独立 DWORD 输出，
  // 不是结构体。写成结构体会导致输出参数错位、DDC 读写必然全部失败。
  [DllImport("dxva2.dll")] static extern bool GetVCPFeatureAndVCPFeatureReply(IntPtr h, byte code, IntPtr pvct, ref uint cur, ref uint max);

  [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll", SetLastError = true)] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out SGRECT r);
  [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] static extern IntPtr GetWindowLongPtr(IntPtr h, int idx);
  [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")] static extern IntPtr SetWindowLongPtr(IntPtr h, int idx, IntPtr v);
  [DllImport("user32.dll")] static extern bool IsWindow(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool SystemParametersInfo(uint action, uint p0, ref SGRECT p1, uint f);

  // 色彩配置（ICC）：GDI 设备上下文路径
  [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] static extern IntPtr CreateDCW(string drv, string dev, string port, IntPtr pdm);
  [DllImport("gdi32.dll")] static extern bool DeleteDC(IntPtr hdc);
  [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] static extern bool GetICMProfileW(IntPtr hdc, ref uint size, StringBuilder name);
  [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] static extern bool SetICMProfileW(IntPtr hdc, string file);
  [DllImport("mscms.dll", CharSet = CharSet.Unicode)] static extern bool InstallColorProfileW(IntPtr h, string prof);

  // DisplayConfig（CCD）：活动路径与设备名查询（与 PnP/WMI 监视器状态无关）。
  // 路径/模式数组必须走原始指针：实测 .NET 结构体数组经 marshaller 进出后数据全零
  // （与 dxva2 GetPhysicalMonitorsFromHMONITOR 必须 [Out] 是同一类坑，这里干脆绕开 marshaller）
  [DllImport("user32.dll")] static extern int GetDisplayConfigBufferSizes(uint flags, out uint nPaths, out uint nModes);
  [DllImport("user32.dll", EntryPoint = "QueryDisplayConfig")] static extern int QueryDisplayConfigRaw(uint flags, ref uint nPaths, IntPtr paths, ref uint nModes, IntPtr modes, IntPtr topo);
  [DllImport("user32.dll")] static extern int DisplayConfigGetDeviceInfo(ref SGDC_SRC_NAME pkt);
  [DllImport("user32.dll")] static extern int DisplayConfigGetDeviceInfo(ref SGDC_DST_NAME pkt);

  // ---- 软件层总亮度（gamma ramp）：不依赖 DDC/CI，对任何屏都生效 ----
  [DllImport("gdi32.dll")] static extern bool SetDeviceGammaRamp(IntPtr hdc, ushort[] ramp);
  [DllImport("gdi32.dll")] static extern bool GetDeviceGammaRamp(IntPtr hdc, ushort[] ramp);
  // ---- 前台窗口铺满 / 恢复（不限于播放器）----
  [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] static extern IntPtr GetShellWindow();
  [DllImport("user32.dll")] static extern IntPtr GetDesktopWindow();
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int max);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] static extern bool IsZoomed(IntPtr h);
  [DllImport("user32.dll")] static extern bool IsIconic(IntPtr h);
  // ---- HDR（advanced color）状态读写 ----
  [DllImport("user32.dll")] static extern int DisplayConfigGetDeviceInfo(ref SGDC_ACI pkt);
  [DllImport("user32.dll")] static extern int DisplayConfigGetDeviceInfo(ref SGDC_ACS pkt);
  [DllImport("user32.dll")] static extern int DisplayConfigSetDeviceInfo(ref SGDC_ACS pkt);
  [DllImport("user32.dll", EntryPoint = "SetDisplayConfig")] static extern int SetDisplayConfigApply(uint np, IntPtr p, uint nm, IntPtr m, uint flags);
  [DllImport("kernel32.dll")] static extern uint GetCurrentProcessId();
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumWinProc cb, IntPtr p);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
  public delegate bool EnumWinProc(IntPtr h, IntPtr p);

  /// DISPLAYCONFIG_DEVICE_INFO_HEADER（20 字节；LUID 拆两个 32 位，避免 .NET 对齐补白）
  [StructLayout(LayoutKind.Sequential)]
  struct SGDC_HDR2 { public uint type; public uint size; public uint adapterLow; public int adapterHigh; public uint id; }
  /// DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO（32 字节）。value 位 0=支持, 位 1=已启用
  [StructLayout(LayoutKind.Sequential)]
  struct SGDC_ACI { public SGDC_HDR2 header; public uint value; public uint colorEncoding; public uint bits; }
  /// DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE（24 字节）
  [StructLayout(LayoutKind.Sequential)]
  struct SGDC_ACS { public SGDC_HDR2 header; public uint enable; }

  const int GWL_STYLE = -16;
  const uint WS_CAPTION = 0x00C00000, WS_THICKFRAME = 0x00040000, WS_POPUP = 0x80000000;
  const uint SWP_NOZORDER = 0x0004, SWP_NOACTIVATE = 0x0010, SWP_FRAMECHANGED = 0x0020, SWP_SHOWWINDOW = 0x0040, SWP_NOSENDCHANGING = 0x0400;
  const uint DM_DISPLAYORIENTATION = 0x00000080;
  const uint DM_PELSWIDTH = 0x00080000, DM_PELSHEIGHT = 0x00100000, DM_DISPLAYFREQUENCY = 0x00400000;
  const uint CDS_UPDATEREGISTRY = 0x0001, CDS_GLOBAL = 0x0008;
  const uint SPI_GETWORKAREA = 0x0030;

  // DEVMODEW（220 字节）关键字段偏移，本机 Win11 24H2 用 ctypes 实测校准
  const int DEVMODE_SIZE = 220;
  const int OFF_DMSIZE = 68;
  const int OFF_DMFIELDS = 72;
  const int OFF_ORIENTATION = 84;
  const int OFF_PELSWIDTH = 172;
  const int OFF_PELSHEIGHT = 176;
  const int OFF_FREQUENCY = 184;

  static IntPtr AllocDevMode() { return Marshal.AllocHGlobal(DEVMODE_SIZE); }

  /// 枚举已连接的显示器（EnumDisplayMonitors 路径，兼容各会话/混合显卡）。
  /// 行: D|<dev>|<uid>|<w>|<h>|<hz>|<main>|<ori>
  public static string ListDisplays() {
    var sb = new StringBuilder();
    EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, (h, hdc, rc, lp) => {
      var mi = new SGMONITORINFO();
      mi.cb = Marshal.SizeOf(typeof(SGMONITORINFO));
      if (GetMonitorInfo(h, ref mi)) {
        string dev = mi.szDevice;
        var b = AllocDevMode();
        try {
          if (EnumDisplaySettingsW(dev, -1, b)) {
            // 按偏移直读，不做结构体映射
            uint dw = (uint)Marshal.ReadInt32(b, OFF_PELSWIDTH);
            uint dh = (uint)Marshal.ReadInt32(b, OFF_PELSHEIGHT);
            uint dhz = (uint)Marshal.ReadInt32(b, OFF_FREQUENCY);
            uint dori = (uint)Marshal.ReadInt32(b, OFF_ORIENTATION);
            string uid = "";
            uint j = 0;
            while (true) {
              var m = new SGDISPLAY_DEVICE();
              m.cb = Marshal.SizeOf(typeof(SGDISPLAY_DEVICE));
              if (!EnumDisplayDevicesW(dev, j, ref m, 0)) break;
              uid = m.DeviceID;
              j++;
            }
            // Win11 24H2 起 EnumDisplayDevicesW 不再枚举适配器上挂的监视器（实测恒返回
            // FALSE，cb=840/844 均如此）→ uid 取不到。回退用 dev 名作会话内唯一 id：
            // get_displays 每次全量重建 id↔dev 映射，dev 名在会话内稳定，功能等价。
            if (string.IsNullOrEmpty(uid)) uid = dev;
            int main = (mi.dwFlags & 1) != 0 ? 1 : 0;
            sb.Append("D|").Append(dev).Append("|").Append(uid).Append("|")
              .Append(dw).Append("|").Append(dh).Append("|")
              .Append(dhz).Append("|").Append(main).Append("|")
              .Append(dori).Append("\n");
          }
        } finally { Marshal.FreeHGlobal(b); }
      }
      return true;
    }, IntPtr.Zero);
    return sb.ToString();
  }

  /// 活动显示路径映射：GDI 设备名(\\.\DISPLAYn) → 显示器友好名。
  /// 行: M|<dev>|<name>。失败/无路径返回空串（调用方回退 WMI → 默认命名）。
  public static string DisplayNameMap() {
    uint nPaths, nModes;
    // 注意：QDC_ONLY_ACTIVE_PATHS=2（QDC_ALL_PATHS 才是 1）——拿全部路径会得到几十条
    // 非活动路径，其源设备名是 WinDisc 占位、目标名查询直接报 87
    if (GetDisplayConfigBufferSizes(2 /*QDC_ONLY_ACTIVE_PATHS*/, out nPaths, out nModes) != 0 || nPaths == 0) return "";
    IntPtr pa = Marshal.AllocHGlobal((int)(nPaths * 72));
    IntPtr ma = Marshal.AllocHGlobal((int)(nModes * 64));
    try {
      uint p = nPaths, m = nModes;
      if (QueryDisplayConfigRaw(2, ref p, pa, ref m, ma, IntPtr.Zero) != 0) return "";
      var sb = new StringBuilder();
      var seen = new System.Collections.Generic.HashSet<string>();
      for (int i = 0; i < p; i++) {
        int off = i * 72;
        var src = new SGDC_SRC_NAME();
        src.header.type = 1; src.header.size = (uint)Marshal.SizeOf(typeof(SGDC_SRC_NAME)); // GET_SOURCE_NAME
        src.header.adapterId.LowPart = (uint)Marshal.ReadInt32(pa, off);
        src.header.adapterId.HighPart = Marshal.ReadInt32(pa, off + 4);
        src.header.id = (uint)Marshal.ReadInt32(pa, off + 8);
        if (DisplayConfigGetDeviceInfo(ref src) != 0) continue;
        var dst = new SGDC_DST_NAME();
        dst.header.type = 2; dst.header.size = (uint)Marshal.SizeOf(typeof(SGDC_DST_NAME)); // GET_TARGET_NAME
        dst.header.adapterId.LowPart = (uint)Marshal.ReadInt32(pa, off + 20);
        dst.header.adapterId.HighPart = Marshal.ReadInt32(pa, off + 24);
        dst.header.id = (uint)Marshal.ReadInt32(pa, off + 28);
        if (DisplayConfigGetDeviceInfo(ref dst) != 0) continue;
        string dev = src.viewGdiDeviceName, nm = dst.monitorFriendlyDeviceName;
        if (string.IsNullOrEmpty(dev) || string.IsNullOrEmpty(nm)) continue;
        if (!seen.Add(dev.ToUpperInvariant())) continue;
        sb.Append("M|").Append(dev).Append("|").Append(nm).Append("\n");
      }
      return sb.ToString();
    } finally { Marshal.FreeHGlobal(pa); Marshal.FreeHGlobal(ma); }
  }

  /// 显示链路拓扑事实（P0-2）：每条活动路径一行。
  /// 行: T|<GDI dev>|<outputTechnology>|<edidManufactureId>|<edidProductCodeId>|<connectorInstance>|<monitorDevicePath>
  /// 全部来自 DisplayConfig 权威来源；输出技术枚举值见 Rust 侧 map_output_tech。
  public static string TopologyInfo() {
    uint nPaths, nModes;
    if (GetDisplayConfigBufferSizes(2 /*QDC_ONLY_ACTIVE_PATHS*/, out nPaths, out nModes) != 0 || nPaths == 0) return "";
    IntPtr pa = Marshal.AllocHGlobal((int)(nPaths * 72));
    IntPtr ma = Marshal.AllocHGlobal((int)(nModes * 64));
    try {
      uint p = nPaths, m = nModes;
      if (QueryDisplayConfigRaw(2, ref p, pa, ref m, ma, IntPtr.Zero) != 0) return "";
      var sb = new StringBuilder();
      var seen = new System.Collections.Generic.HashSet<string>();
      for (int i = 0; i < p; i++) {
        int off = i * 72;
        var src = new SGDC_SRC_NAME();
        src.header.type = 1; src.header.size = (uint)Marshal.SizeOf(typeof(SGDC_SRC_NAME));
        src.header.adapterId.LowPart = (uint)Marshal.ReadInt32(pa, off);
        src.header.adapterId.HighPart = Marshal.ReadInt32(pa, off + 4);
        src.header.id = (uint)Marshal.ReadInt32(pa, off + 8);
        if (DisplayConfigGetDeviceInfo(ref src) != 0) continue;
        var dst = new SGDC_DST_NAME();
        dst.header.type = 2; dst.header.size = (uint)Marshal.SizeOf(typeof(SGDC_DST_NAME));
        dst.header.adapterId.LowPart = (uint)Marshal.ReadInt32(pa, off + 20);
        dst.header.adapterId.HighPart = Marshal.ReadInt32(pa, off + 24);
        dst.header.id = (uint)Marshal.ReadInt32(pa, off + 28);
        if (DisplayConfigGetDeviceInfo(ref dst) != 0) continue;
        string dev = src.viewGdiDeviceName;
        if (string.IsNullOrEmpty(dev) || !seen.Add(dev.ToUpperInvariant())) continue;
        sb.Append("T|").Append(dev).Append("|").Append(dst.outputTechnology)
          .Append("|").Append(dst.edidManufactureId).Append("|").Append(dst.edidProductCodeId)
          .Append("|").Append(dst.connectorInstance).Append("|").Append(dst.monitorDevicePath).Append("\n");
      }
      return sb.ToString();
    } finally { Marshal.FreeHGlobal(pa); Marshal.FreeHGlobal(ma); }
  }

  // ===== 软件层总亮度（gamma ramp）：不依赖 DDC/CI，对任何显示器都生效 =====
  // 设计要点：三通道写同一条曲线 → 只改明暗、不动色相（颜色安全）；
  // 范围锁 40~160，避免把屏幕调到看不见；gamma ramp 不写注册表、重启即失效（天然可回滚）。

  /// 生成亮度曲线：out = v^(100/pct)，端点 0/1 保持不变（不削顶、不发灰）
  static ushort[] BuildRamp(int pct) {
    if (pct < 40) pct = 40;
    if (pct > 160) pct = 160;
    double inv = 100.0 / pct;
    var ramp = new ushort[768];
    for (int i = 0; i < 256; i++) {
      double v = i / 255.0;
      double o = Math.Pow(v, inv);
      int w = (int)Math.Round(o * 65535.0);
      if (w < 0) w = 0;
      if (w > 65535) w = 65535;
      ushort u = (ushort)w;
      ramp[i] = u; ramp[i + 256] = u; ramp[i + 512] = u;
    }
    return ramp;
  }

  /// 把总亮度应用到所有在用屏幕。返回逐屏结果：OK|<dev> / FAIL|<dev>
  public static string GammaSetAll(int pct) {
    if (pct < 40) pct = 40;
    if (pct > 160) pct = 160;
    ushort[] ramp = BuildRamp(pct);
    var sb = new StringBuilder();
    for (int i = 1; i <= 12; i++) {
      string dev = "\\\\.\\DISPLAY" + i;
      var dm = AllocDevMode();
      try {
        if (!EnumDisplaySettingsW(dev, -1, dm)) continue;
        if (Marshal.ReadInt32(dm, OFF_PELSWIDTH) <= 0) continue;
        IntPtr dc = CreateDCW(null, dev, null, IntPtr.Zero);
        if (dc == IntPtr.Zero) continue;
        try {
          bool ok = SetDeviceGammaRamp(dc, ramp);
          sb.Append(ok ? "OK|" : "FAIL|").Append(dev).Append((char)10);
        } finally { DeleteDC(dc); }
      } finally { Marshal.FreeHGlobal(dm); }
    }
    return sb.ToString();
  }

  /// 读回当前总亮度（按中灰点反解 gamma 指数）。行: G|<dev>|<pct>
  public static string GammaGetAll() {
    var sb = new StringBuilder();
    for (int i = 1; i <= 12; i++) {
      string dev = "\\\\.\\DISPLAY" + i;
      var dm = AllocDevMode();
      try {
        if (!EnumDisplaySettingsW(dev, -1, dm)) continue;
        if (Marshal.ReadInt32(dm, OFF_PELSWIDTH) <= 0) continue;
        IntPtr dc = CreateDCW(null, dev, null, IntPtr.Zero);
        if (dc == IntPtr.Zero) continue;
        try {
          var ramp = new ushort[768];
          int pct = 100;
          if (GetDeviceGammaRamp(dc, ramp)) {
            double mid = ramp[128] / 65535.0;          // 128/255 = 0.50196
            if (mid > 0.03 && mid < 0.97) {
              double f = Math.Log(0.50196) / Math.Log(mid);
              if (f > 0.3 && f < 3.5) pct = (int)Math.Round(f * 100.0);
            }
          }
          sb.Append("G|").Append(dev).Append("|").Append(pct).Append((char)10);
        } finally { DeleteDC(dc); }
      } finally { Marshal.FreeHGlobal(dm); }
    }
    return sb.ToString();
  }

  // ===== HDR（advanced color）状态读写 =====
  /// 每台屏的 HDR 状态：H|<dev>|<是否支持>|<是否已启用>
  public static string HDRStates() {
    var sb = new StringBuilder();
    uint nPaths, nModes;
    if (GetDisplayConfigBufferSizes(2, out nPaths, out nModes) != 0 || nPaths == 0) return "";
    IntPtr pa = Marshal.AllocHGlobal((int)(nPaths * 72));
    IntPtr ma = Marshal.AllocHGlobal((int)(nModes * 64));
    try {
      uint p = nPaths, m = nModes;
      if (QueryDisplayConfigRaw(2, ref p, pa, ref m, ma, IntPtr.Zero) != 0) return "";
      var seen = new System.Collections.Generic.HashSet<string>();
      for (int i = 0; i < p; i++) {
        int off = i * 72;
        var src = new SGDC_SRC_NAME();
        src.header.type = 1; src.header.size = (uint)Marshal.SizeOf(typeof(SGDC_SRC_NAME));
        src.header.adapterId.LowPart = (uint)Marshal.ReadInt32(pa, off);
        src.header.adapterId.HighPart = Marshal.ReadInt32(pa, off + 4);
        src.header.id = (uint)Marshal.ReadInt32(pa, off + 8);
        if (DisplayConfigGetDeviceInfo(ref src) != 0) continue;
        string dev = src.viewGdiDeviceName;
        if (string.IsNullOrEmpty(dev) || !seen.Add(dev.ToUpperInvariant())) continue;
        var aci = new SGDC_ACI();
        aci.header.type = 9;                          // GET_ADVANCED_COLOR_INFO
        aci.header.size = (uint)Marshal.SizeOf(typeof(SGDC_ACI));
        aci.header.adapterLow = (uint)Marshal.ReadInt32(pa, off + 20);
        aci.header.adapterHigh = Marshal.ReadInt32(pa, off + 24);
        aci.header.id = (uint)Marshal.ReadInt32(pa, off + 28);
        if (DisplayConfigGetDeviceInfo(ref aci) != 0) continue;
        int sup = (int)(aci.value & 1);
        int en = (int)((aci.value >> 1) & 1);
        sb.Append("H|").Append(dev).Append("|").Append(sup).Append("|").Append(en).Append((char)10);
      }
      return sb.ToString();
    } finally { Marshal.FreeHGlobal(pa); Marshal.FreeHGlobal(ma); }
  }

  /// 把所有支持 HDR 的屏统一设为开(on=1)或关(on=0)。返回逐屏结果 R|dev|OK或ERR
  public static string HDRSetAll(int on) {
    var sb = new StringBuilder();
    uint nPaths, nModes;
    if (GetDisplayConfigBufferSizes(2, out nPaths, out nModes) != 0 || nPaths == 0) return "";
    IntPtr pa = Marshal.AllocHGlobal((int)(nPaths * 72));
    IntPtr ma = Marshal.AllocHGlobal((int)(nModes * 64));
    try {
      uint p = nPaths, m = nModes;
      if (QueryDisplayConfigRaw(2, ref p, pa, ref m, ma, IntPtr.Zero) != 0) return "";
      var seen = new System.Collections.Generic.HashSet<string>();
      for (int i = 0; i < p; i++) {
        int off = i * 72;
        var src = new SGDC_SRC_NAME();
        src.header.type = 1; src.header.size = (uint)Marshal.SizeOf(typeof(SGDC_SRC_NAME));
        src.header.adapterId.LowPart = (uint)Marshal.ReadInt32(pa, off);
        src.header.adapterId.HighPart = Marshal.ReadInt32(pa, off + 4);
        src.header.id = (uint)Marshal.ReadInt32(pa, off + 8);
        if (DisplayConfigGetDeviceInfo(ref src) != 0) continue;
        string dev = src.viewGdiDeviceName;
        if (string.IsNullOrEmpty(dev) || !seen.Add(dev.ToUpperInvariant())) continue;

        var aci = new SGDC_ACI();
        aci.header.type = 9; aci.header.size = (uint)Marshal.SizeOf(typeof(SGDC_ACI));
        aci.header.adapterLow = (uint)Marshal.ReadInt32(pa, off + 20);
        aci.header.adapterHigh = Marshal.ReadInt32(pa, off + 24);
        aci.header.id = (uint)Marshal.ReadInt32(pa, off + 28);
        if (DisplayConfigGetDeviceInfo(ref aci) != 0) continue;
        if ((aci.value & 1) == 0) {                   // 不支持 HDR 的屏跳过（不乱设）
          sb.Append("R|").Append(dev).Append("|SKIP(该屏不支持HDR)").Append((char)10);
          continue;
        }
        var acs = new SGDC_ACS();
        acs.header.type = 10;                         // SET_ADVANCED_COLOR_STATE
        acs.header.size = (uint)Marshal.SizeOf(typeof(SGDC_ACS));
        acs.header.adapterLow = (uint)Marshal.ReadInt32(pa, off + 20);
        acs.header.adapterHigh = Marshal.ReadInt32(pa, off + 24);
        acs.header.id = (uint)Marshal.ReadInt32(pa, off + 28);
        acs.enable = (uint)(on != 0 ? 1 : 0);
        int r = DisplayConfigSetDeviceInfo(ref acs);
        // 关键：SET_ADVANCED_COLOR_STATE 只是把新状态写进数据库，必须再 Apply 一次
        // （SetDisplayConfig(QDC_DATABASE_CURRENT)）系统才真正切 HDR ——
        // 少了这一步会「API 返回 0 成功、读回来还是老状态」（实测踩过）。
        int ap = SetDisplayConfigApply(0, IntPtr.Zero, 0, IntPtr.Zero, 4);
        sb.Append("R|").Append(dev).Append("|").Append(r == 0 ? "OK" : ("ERR:set " + r))
          .Append("|apply=").Append(ap == 0 ? "OK" : ("ERR:" + ap)).Append((char)10);
      }
      return sb.ToString();
    } finally { Marshal.FreeHGlobal(pa); Marshal.FreeHGlobal(ma); }
  }

  // ===== 双屏铺满（任意前台窗口，不限于播放器）=====
  /// 从极简 JSON 里取一个整数键（避免引入 JSON 依赖）
  static long JsInt(string js, string key) {
    try {
      int k = js.IndexOf("\"" + key + "\"");
      if (k < 0) return 0;
      int c = js.IndexOf(":", k);
      if (c < 0) return 0;
      int s = c + 1;
      while (s < js.Length && (js[s] == 32 || js[s] == 9)) s++;
      int e = s;
      while (e < js.Length && (char.IsDigit(js[e]) || js[e] == 45)) e++;
      long v;
      return long.TryParse(js.Substring(s, e - s), out v) ? v : 0;
    } catch { return 0; }
  }

  /// 只设置「指定某一台屏」的 HDR（用于基线还原：各屏原本状态可能不同）
  /// 返回 OK / ERR:... 。与 HDRSetAll 同源：SET 之后必须再 Apply 一次才真正生效。
  public static string HDRSetDev(string dev, int on) {
    if (string.IsNullOrEmpty(dev)) return "ERR:设备名为空";
    uint nPaths, nModes;
    if (GetDisplayConfigBufferSizes(2, out nPaths, out nModes) != 0 || nPaths == 0) return "ERR:读取显示配置失败";
    IntPtr pa = Marshal.AllocHGlobal((int)(nPaths * 72));
    IntPtr ma = Marshal.AllocHGlobal((int)(nModes * 64));
    try {
      uint p = nPaths, m = nModes;
      if (QueryDisplayConfigRaw(2, ref p, pa, ref m, ma, IntPtr.Zero) != 0) return "ERR:QueryDisplayConfig 失败";
      for (int i = 0; i < p; i++) {
        int off = i * 72;
        var src = new SGDC_SRC_NAME();
        src.header.type = 1; src.header.size = (uint)Marshal.SizeOf(typeof(SGDC_SRC_NAME));
        src.header.adapterId.LowPart = (uint)Marshal.ReadInt32(pa, off);
        src.header.adapterId.HighPart = Marshal.ReadInt32(pa, off + 4);
        src.header.id = (uint)Marshal.ReadInt32(pa, off + 8);
        if (DisplayConfigGetDeviceInfo(ref src) != 0) continue;
        if (!string.Equals(src.viewGdiDeviceName, dev, StringComparison.OrdinalIgnoreCase)) continue;
        var aci = new SGDC_ACI();
        aci.header.type = 9; aci.header.size = (uint)Marshal.SizeOf(typeof(SGDC_ACI));
        aci.header.adapterLow = (uint)Marshal.ReadInt32(pa, off + 20);
        aci.header.adapterHigh = Marshal.ReadInt32(pa, off + 24);
        aci.header.id = (uint)Marshal.ReadInt32(pa, off + 28);
        if (DisplayConfigGetDeviceInfo(ref aci) != 0) return "ERR:读取 HDR 状态失败";
        if ((aci.value & 1) == 0) return "SKIP:该屏不支持 HDR";
        if (((aci.value >> 1) & 1) == (uint)(on != 0 ? 1 : 0)) return "OK:已是目标状态";
        var acs = new SGDC_ACS();
        acs.header.type = 10; acs.header.size = (uint)Marshal.SizeOf(typeof(SGDC_ACS));
        acs.header.adapterLow = (uint)Marshal.ReadInt32(pa, off + 20);
        acs.header.adapterHigh = Marshal.ReadInt32(pa, off + 24);
        acs.header.id = (uint)Marshal.ReadInt32(pa, off + 28);
        acs.enable = (uint)(on != 0 ? 1 : 0);
        int r = DisplayConfigSetDeviceInfo(ref acs);
        int ap = SetDisplayConfigApply(0, IntPtr.Zero, 0, IntPtr.Zero, 4);
        if (r != 0) return "ERR:set " + r;
        if (ap != 0) return "ERR:apply " + ap;
        return "OK";
      }
      return "ERR:没找到该显示设备";
    } finally { Marshal.FreeHGlobal(pa); Marshal.FreeHGlobal(ma); }
  }

  /// 把某个 ICC 配置文件关联到指定显示设备（基线还原用），内部回读校验
  public static string IccSetPath(string dev, string profile) {
    if (string.IsNullOrEmpty(dev) || string.IsNullOrEmpty(profile)) return "ERR:参数为空";
    if (!System.IO.File.Exists(profile)) return "ERR:配置文件不存在";
    IntPtr hdc = CreateDCW(null, dev, null, IntPtr.Zero);
    if (hdc == IntPtr.Zero) return "ERR:CreateDC 失败";
    try {
      SetICMProfileW(hdc, profile);   // 失败也会返回 TRUE，所以下面必须回读
    } finally { DeleteDC(hdc); }
    var sb = new StringBuilder(600);
    IntPtr h2 = CreateDCW(null, dev, null, IntPtr.Zero);
    if (h2 == IntPtr.Zero) return "ERR:回读 CreateDC 失败";
    try {
      uint sz = 600;
      if (GetICMProfileW(h2, ref sz, sb)) {
        string got = sb.ToString();
        return string.Equals(got, profile, StringComparison.OrdinalIgnoreCase) ? "OK" : ("ERR:回读不符（可能需要管理员权限）");
      }
      return "ERR:回读失败";
    } finally { DeleteDC(h2); }
  }

  /// 把当前前台窗口铺满所有屏幕的包围盒（抖音/浏览器全屏视频、播放器都适用）。
  /// 记录窗口句柄 + 原始位置/样式到 stateFile，供 RestoreForeground 还原。
  /// 返回 OK|<窗口标题> 或 ERR:<原因>
  // DPI 感知：不设的话本进程看到的坐标被系统缩放过（实测主屏 3840 被看成 2560、
  // 虚拟桌面 6240x3840 被看成 3760x2507），铺满就会算错尺寸 —— 副屏那块永远铺不到。
  // 设成 PER_MONITOR_AWARE_V2(-4) 之后拿到的才是真实物理像素。
  // 注意：SetProcessDpiAwarenessContext 的 DllImport 本类里已经有，别重复声明。
  static bool _dpiOn = false;
  static void DpiOn() { if (!_dpiOn) { try { SetProcessDpiAwarenessContext(new IntPtr(-4)); } catch { } _dpiOn = true; } }

  public static string SpanForeground(string stateFile, int myPid, int mode) {
    DpiOn();
    IntPtr h = GetForegroundWindow();
    if (h == IntPtr.Zero) return "ERR:没有前台窗口";
    if (h == GetShellWindow() || h == GetDesktopWindow()) return "ERR:当前前台是桌面/任务栏，请先切换到要铺满的窗口";
    var ti = new StringBuilder(300);
    GetWindowTextW(h, ti, 300);
    string title = ti.ToString();
    // 安全闸门：绝不铺满本程序自己的窗口 —— 一旦把自己铺满、而窗口底部被裁掉，
    // 用户就找不到「还原」入口了（实测踩过）。此时改提示用「选择窗口」。
    uint fgPid;
    GetWindowThreadProcessId(h, out fgPid);
    if ((int)fgPid == myPid) {
      return "ERR:当前前台是本程序自己的窗口，不能铺满它（否则你可能找不到「还原」）。请先在右下角「选择窗口」里挑一个别的窗口";
    }
    long originalShowCmd = IsZoomed(h) ? 3 : (IsIconic(h) ? 6 : 1);
    // 最大化/最小化先还原，否则 SetWindowPos 改不动尺寸
    if (IsIconic(h) || IsZoomed(h)) ShowWindow(h, 9);  // SW_RESTORE
    System.Threading.Thread.Sleep(260);
    SGRECT wr;
    if (!GetWindowRect(h, out wr)) {
      RestoreShowState(h, originalShowCmd);
      return "ERR:读取窗口位置失败；已恢复原最大化/最小化状态";
    }
    if (wr.Right - wr.Left <= 0 || wr.Bottom - wr.Top <= 0) {
      RestoreShowState(h, originalShowCmd);
      return "ERR:窗口尺寸无效（可能已被最小化）；已恢复原显示状态";
    }
    return SpanHandle(h, stateFile, myPid, mode, originalShowCmd);
  }

  /// 还原上一次被铺满的窗口（按记录里的句柄找，不依赖当前前台）
  public static string RestoreForeground(string stateFile) {
    // Restoration coordinates were captured in per-monitor physical pixels.
    // Each PowerShell bridge call runs in a fresh process, so explicitly use the
    // same coordinate space as the span operation before applying those bounds.
    IntPtr oldDpi = IntPtr.Zero;
    try { oldDpi = SetThreadDpiAwarenessContext(new IntPtr(-4)); } catch { }
    if (oldDpi == IntPtr.Zero) return "ERR:无法启用逐屏 DPI 坐标，未还原窗口";
    try { return RestoreForegroundDpiAware(stateFile); }
    finally { try { SetThreadDpiAwarenessContext(oldDpi); } catch { } }
  }

  static string RestoreForegroundDpiAware(string stateFile) {
    string js;
    try { js = System.IO.File.ReadAllText(stateFile); } catch { return "ERR:没有可恢复的记录（先点一次铺满）"; }
    IntPtr h = new IntPtr(JsInt(js, "hwnd"));
    if (h == IntPtr.Zero || !IsWindow(h)) return "ERR:原窗口已关闭";
    int x = (int)JsInt(js, "x"), y = (int)JsInt(js, "y");
    int w = (int)JsInt(js, "w"), hh = (int)JsInt(js, "h");
    long st = JsInt(js, "style");
    long showCmd = JsInt(js, "showCmd");
    if (st != 0) SetWindowLongPtr(h, GWL_STYLE, new IntPtr(st));
    if (!SetWindowPos(h, IntPtr.Zero, x, y, w, hh, SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW)) {
      return "ERR:还原窗口位置失败（" + Marshal.GetLastWin32Error() + "），目标矩形=" + x + "," + y + " " + w + "x" + hh;
    }
    System.Threading.Thread.Sleep(120);
    SGRECT actual;
    if (!GetWindowRect(h, out actual)) return "ERR:窗口已发送还原命令，但无法回读实际边界";
    int actualW = actual.Right - actual.Left, actualH = actual.Bottom - actual.Top;
    if (Math.Abs(actual.Left - x) > 8 || Math.Abs(actual.Top - y) > 8 || Math.Abs(actualW - w) > 8 || Math.Abs(actualH - hh) > 8) {
      return "ERR:窗口未接受还原尺寸；目标=" + x + "," + y + " " + w + "x" + hh + "，实际=" + actual.Left + "," + actual.Top + " " + actualW + "x" + actualH;
    }
    RestoreShowState(h, showCmd);
    return "OK";
  }

  static void RestoreShowState(IntPtr h, long showCmd) {
    if (!IsWindow(h)) return;
    if (showCmd == 3) ShowWindow(h, 3);       // SW_MAXIMIZE
    else if (showCmd == 2 || showCmd == 6) ShowWindow(h, 6); // minimized before span
  }

  static string EscW(string s) {
    if (s == null) return "";
    return s.Replace("|", "/").Replace((char)13, (char)32).Replace((char)10, (char)32);
  }

  /// 列出「可以被铺满的窗口」：可见、有标题、尺寸正常、非本程序、非桌面/任务栏。
  /// 每行 W|hwnd|宽x高|标题。给用户一个可选项，避免只能对"当前前台"下手。
  /// myPid 必须由 Rust 侧传入（应用自己的 PID）—— 因为这段 C# 跑在 powershell.exe 里，
  /// GetCurrentProcessId() 拿到的是 PowerShell 的 PID，拿它排除「自己」是错的（实测踩过）。
  public static string ListWindows(int myPid) {
    var sb = new StringBuilder();
    IntPtr shell = GetShellWindow();
    IntPtr desk = GetDesktopWindow();
    try {
      EnumWindows(delegate(IntPtr h, IntPtr p) {
        if (!IsWindowVisible(h)) return true;
        if (h == shell || h == desk) return true;
        var t = new StringBuilder(300);
        GetWindowTextW(h, t, 300);
        string title = t.ToString();
        if (title.Length == 0) return true;
        uint pid;
        GetWindowThreadProcessId(h, out pid);
        if ((int)pid == myPid) return true;        // 不列自己的窗口（铺满自己会找不到还原入口）
        long ex = GetWindowLongPtr(h, -20).ToInt64();   // GWL_EXSTYLE
        if ((ex & 0x00000080L) != 0) return true;  // WS_EX_TOOLWINDOW：工具窗/浮动提示，跳过
        SGRECT r;
        if (!GetWindowRect(h, out r)) return true;
        int w = r.Right - r.Left, hh = r.Bottom - r.Top;
        if (w < 120 || hh < 90) return true;       // 过滤托盘小窗、0 尺寸窗
        sb.Append("W|").Append(h.ToInt64()).Append("|").Append(w).Append("x").Append(hh)
          .Append("|").Append(EscW(title)).Append((char)10);
        return true;
      }, IntPtr.Zero);
    } catch { }
    return sb.ToString();
  }

  /// 真正干活的实现：对**指定句柄**动手（不依赖前台窗口）。
  /// 必须拆出来：SetForegroundWindow 常常被 Windows 拒绝（后台程序不能抢前台），
  /// 于是原来「先置前再走前台那套逻辑」会把**别人**的窗口铺满 —— 实测拿记事本当靶子，
  /// 结果铺满的是当时真正在前台的抖音窗口。
  static string SpanHandle(IntPtr h, string stateFile, int myPid, int mode, long originalShowCmd) {
    // Screen metrics and SetWindowPos must use the same per-monitor physical-pixel
    // coordinate space. PowerShell may already have initialized USER32 before the
    // process-level DPI switch, so set the calling thread context explicitly.
    IntPtr oldDpi = IntPtr.Zero;
    try { oldDpi = SetThreadDpiAwarenessContext(new IntPtr(-4)); } catch { }
    if (oldDpi == IntPtr.Zero) {
      RestoreShowState(h, originalShowCmd);
      return "ERR:无法启用逐屏 DPI 坐标，未调整窗口；已恢复原最大化/最小化状态";
    }
    try { return SpanHandleDpiAware(h, stateFile, myPid, mode, originalShowCmd); }
    finally { try { SetThreadDpiAwarenessContext(oldDpi); } catch { } }
  }

  static void RollbackSpanWindow(IntPtr h, long oldStyle, SGRECT oldRect, long originalShowCmd) {
    if (!IsWindow(h)) return;
    SetWindowLongPtr(h, GWL_STYLE, new IntPtr(oldStyle));
    SetWindowPos(h, IntPtr.Zero, oldRect.Left, oldRect.Top,
      oldRect.Right - oldRect.Left, oldRect.Bottom - oldRect.Top,
      SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW);
    RestoreShowState(h, originalShowCmd);
  }

  static string SpanHandleDpiAware(IntPtr h, string stateFile, int myPid, int mode, long originalShowCmd) {
    StringBuilder tb = new StringBuilder(512);
    GetWindowTextW(h, tb, 300);
    string title = tb.ToString();
    // 这两句原来在读前台窗口的那段里，搬到 SpanHandle 时必须一起带上（否则 wr 未声明）
    SGRECT wr;
    if (!GetWindowRect(h, out wr)) {
      RestoreShowState(h, originalShowCmd);
      return "ERR:读取窗口位置失败；已恢复原最大化/最小化状态";
    }
    if (wr.Right - wr.Left <= 0 || wr.Bottom - wr.Top <= 0) {
      RestoreShowState(h, originalShowCmd);
      return "ERR:窗口尺寸无效（可能已被最小化）；已恢复原显示状态";
    }
    long oldStyle = GetWindowLongPtr(h, GWL_STYLE).ToInt64();
    int vx0 = GetSystemMetrics(76), vy0 = GetSystemMetrics(77);
    int vw0 = GetSystemMetrics(78), vh0 = GetSystemMetrics(79);
    // 防呆：已经处于铺满状态、且备份记录属于同一个窗口时，不要再覆盖备份
    // （否则连点两次「铺满」会把备份写成铺满后的矩形，「还原」就再也回不去了）
    bool alreadySpanned = Math.Abs(wr.Left - vx0) < 4 && Math.Abs(wr.Top - vy0) < 4
      && Math.Abs((wr.Right - wr.Left) - vw0) < 8 && Math.Abs((wr.Bottom - wr.Top) - vh0) < 8;
    // 以主屏为基准时，铺满后的矩形不是包围盒，而是「包围盒宽 × 主屏高」，要按这个口径判断
    if (mode == 1) {
      int ph0 = GetSystemMetrics(1); // SM_CYSCREEN = 主屏高
      alreadySpanned = Math.Abs(wr.Left - vx0) < 4 && Math.Abs(wr.Top) < 4
        && Math.Abs((wr.Right - wr.Left) - vw0) < 8 && Math.Abs((wr.Bottom - wr.Top) - ph0) < 8;
    }
    bool sameWin = false;
    try {
      if (System.IO.File.Exists(stateFile)) {
        sameWin = JsInt(System.IO.File.ReadAllText(stateFile), "hwnd") == h.ToInt64();
      }
    } catch { }
    if (!(alreadySpanned && sameWin)) {
      string json = "{\"hwnd\":" + h.ToInt64() + ",\"x\":" + wr.Left + ",\"y\":" + wr.Top
        + ",\"w\":" + (wr.Right - wr.Left) + ",\"h\":" + (wr.Bottom - wr.Top)
        + ",\"style\":" + oldStyle + ",\"showCmd\":" + originalShowCmd + "}";
      try { System.IO.File.WriteAllText(stateFile, json); } catch { }
    }
    long s = oldStyle;
    s &= ~((long)WS_CAPTION | (long)WS_THICKFRAME);
    s |= (long)WS_POPUP;
    SetWindowLongPtr(h, GWL_STYLE, new IntPtr(s));
    int vx = GetSystemMetrics(76), vy = GetSystemMetrics(77);
    int vw = GetSystemMetrics(78), vh = GetSystemMetrics(79);
    if (mode == 1) {
      // 「以主屏为基准」：宽度照样横跨整块桌面，但高度只取**主屏高度**并对齐主屏顶端。
      // 为什么需要：两块屏分辨率不同（本机主屏 3840x2160 横、副屏 2400x3840 竖），
      // 把窗口放大到整个包围盒(4160x2560)再整体缩放，较短的那块屏底部必然被裁掉；
      // 用主屏高度 → 主屏画面完整无裁切，竖屏那块上下留边而不是切掉画面（用户实测反馈）。
      int ph = GetSystemMetrics(1); // SM_CYSCREEN
      if (ph > 0 && ph < vh) {
        vh = ph;
        vy = 0;
      }
    }
    uint spanFlags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW;
    bool bypassedWindowPosChanging = false;
    int normalError = 0;
    if (!SetWindowPos(h, IntPtr.Zero, vx, vy, vw, vh, spanFlags)) {
      normalError = Marshal.GetLastWin32Error();
      // Some application windows reject cross-monitor dimensions in
      // WM_WINDOWPOSCHANGING. Retry once without that pre-change hook.
      if (!SetWindowPos(h, IntPtr.Zero, vx, vy, vw, vh, spanFlags | SWP_NOSENDCHANGING)) {
        int bypassError = Marshal.GetLastWin32Error();
        RollbackSpanWindow(h, oldStyle, wr, originalShowCmd);
        return "ERR:SetWindowPos 普通请求失败（" + normalError + "），绕过 WM_WINDOWPOSCHANGING 重试也失败（" + bypassError + "），目标矩形=" + vx + "," + vy + " " + vw + "x" + vh;
      }
      bypassedWindowPosChanging = true;
    }
    System.Threading.Thread.Sleep(120);
    SGRECT actual;
    if (!GetWindowRect(h, out actual)) {
      RollbackSpanWindow(h, oldStyle, wr, originalShowCmd);
      return "ERR:已发送铺满命令，但无法回读窗口边界；已尝试恢复窗口原样";
    }
    int actualW = actual.Right - actual.Left, actualH = actual.Bottom - actual.Top;
    if (Math.Abs(actual.Left - vx) > 8 || Math.Abs(actual.Top - vy) > 8 || Math.Abs(actualW - vw) > 8 || Math.Abs(actualH - vh) > 8) {
      if (!bypassedWindowPosChanging) {
        // Give the target app its normal resize hook first. Bypass it only when
        // the physical-pixel readback proves that it clamped the requested span.
        if (!SetWindowPos(h, IntPtr.Zero, vx, vy, vw, vh, spanFlags | SWP_NOSENDCHANGING)) {
          int bypassError = Marshal.GetLastWin32Error();
          RollbackSpanWindow(h, oldStyle, wr, originalShowCmd);
          return "ERR:窗口普通调整未铺满且绕过 WM_WINDOWPOSCHANGING 重试失败（" + bypassError + "）；目标=" + vx + "," + vy + " " + vw + "x" + vh + "，普通调整实际=" + actual.Left + "," + actual.Top + " " + actualW + "x" + actualH;
        }
        bypassedWindowPosChanging = true;
        System.Threading.Thread.Sleep(120);
        if (!GetWindowRect(h, out actual)) {
          RollbackSpanWindow(h, oldStyle, wr, originalShowCmd);
          return "ERR:绕过 WM_WINDOWPOSCHANGING 重试后无法回读窗口边界；已尝试恢复窗口原样";
        }
        actualW = actual.Right - actual.Left;
        actualH = actual.Bottom - actual.Top;
      }
      if (Math.Abs(actual.Left - vx) > 8 || Math.Abs(actual.Top - vy) > 8 || Math.Abs(actualW - vw) > 8 || Math.Abs(actualH - vh) > 8) {
        RollbackSpanWindow(h, oldStyle, wr, originalShowCmd);
        return "ERR:目标窗口未接受完整的双屏尺寸（已尝试跳过 WM_WINDOWPOSCHANGING）；目标=" + vx + "," + vy + " " + vw + "x" + vh + "，实际=" + actual.Left + "," + actual.Top + " " + actualW + "x" + actualH;
      }
    }
    return "OK|" + EscW(string.IsNullOrEmpty(title) ? "（无标题窗口）" : title)
      + "|窗口外框回读=" + actual.Left + "," + actual.Top + " " + actualW + "x" + actualH
      + "；目标=" + vx + "," + vy + " " + vw + "x" + vh;
  }

  /// 铺满指定的窗口（按 hwnd）。直接对目标句柄动手，不依赖前台窗口。
  public static string SpanWindow(long hwnd, string stateFile, int myPid, int mode) {
    DpiOn();
    IntPtr h = new IntPtr(hwnd);
    if (!IsWindow(h)) return "ERR:该窗口已关闭";
    long originalShowCmd = IsZoomed(h) ? 3 : (IsIconic(h) ? 6 : 1);
    // A maximized app keeps its primary-monitor maximize rectangle and can
    // ignore a cross-desktop resize. Restore it before removing its frame and
    // setting the explicit virtual-desktop bounds (same sequence as foreground).
    if (IsIconic(h) || IsZoomed(h)) {
      ShowWindow(h, 9); // SW_RESTORE
      System.Threading.Thread.Sleep(260);
    }
    // 直接对目标句柄动手，**不**依赖 SetForegroundWindow
    // （它会失败，导致铺满的是当时真正在前台的**别人**的窗口 —— 实测踩过）
    return SpanHandle(h, stateFile, myPid, mode, originalShowCmd);
  }

  /// 对指定显示设备执行 DDC/CI（VCP 读或写），返回是否命中；retries 为最大重试次数
  /// 加固（v0.2.8）：① 整体 try/catch——显卡驱动崩溃/TDR 期间 dxva2 调用可能抛异常或返回无效句柄，
  ///   此前无保护会让异常沿 EnumDisplayMonitors 回调冒泡、甚至崩掉承载的 PowerShell 进程；
  ///   ② DestroyPhysicalMonitors 放进 finally——确保任何路径都释放物理显示器句柄，防泄漏；
  ///   ③ 单次失败不再盲目 sleep 重试——驱动复位循环里 DDC 反复超时是加剧 TDR 的元凶，快速失败更安全。
  static bool TryVCP(string dev, byte code, bool write, uint val, int retries, out uint cur) {
    cur = 0;
    var curBox = new uint[1];
    var anyBox = new bool[1];
    try {
      EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, (h, hdc, rc, lp) => {
        var mi = new SGMONITORINFO();
        mi.cb = Marshal.SizeOf(typeof(SGMONITORINFO));
        if (!GetMonitorInfo(h, ref mi)) return true;
        if (!mi.szDevice.Equals(dev, StringComparison.OrdinalIgnoreCase)) return true;
        uint n;
        if (!GetNumberOfPhysicalMonitorsFromHMONITOR(h, out n) || n == 0) return true;
        var arr = new SGPHYS_MON[n];
        if (!GetPhysicalMonitorsFromHMONITOR(h, n, arr)) return true;
        try {
          foreach (var pm in arr) {
            if (pm.h == IntPtr.Zero) continue;
            if (write) {
              // DDC 写入偶发失败，重试；一旦成功即止
              for (int t = 0; t < retries && !anyBox[0]; t++) {
                if (SetVCPFeature(pm.h, code, val)) anyBox[0] = true;
                else if (t + 1 < retries) System.Threading.Thread.Sleep(80);
              }
            } else {
              // 显示器 DDC 响应慢：单次读取常失败，必须重试
              // 注意：变量不能叫 cur/max——与外层参数 out cur 同名的局部变量会 C# 编译失败
              uint curVal = 0, maxVal = 0;
              bool ok = false;
              for (int t = 0; t < retries && !ok; t++) {
                ok = GetVCPFeatureAndVCPFeatureReply(pm.h, code, IntPtr.Zero, ref curVal, ref maxVal);
                if (!ok && t + 1 < retries) System.Threading.Thread.Sleep(80);
              }
              if (ok) { curBox[0] = curVal; anyBox[0] = true; }
            }
          }
        } finally {
          // 无论读写成败都释放句柄，避免物理显示器句柄泄漏（泄漏会在热插拔/驱动复位后累积）
          try { DestroyPhysicalMonitors(n, arr); } catch { }
        }
        return true;
      }, IntPtr.Zero);
    } catch {
      // 驱动栈异常（如 nvlddmkm TDR 崩溃循环、显示器刚被热插拔）时静默快速失败，
      // 不再让异常向上冒泡。返回 false 让上层按“DDC 不可用”走 WMI/提示兜底。
      return false;
    }
    cur = curBox[0];
    return anyBox[0];
  }

  /// 读 VCP 值，失败返回 ERR
  public static string DDCRead(string dev, byte code) {
    uint cur;
    return TryVCP(dev, code, false, 0, 5, out cur) ? cur.ToString() : "ERR";
  }

  /// 读 VCP 当前值与面板上报上限，返回 "cur|max"，失败返回 ERR。
  /// P1-4 便携屏功耗诊断专用：evidence 需要同时给当前亮度与面板最大亮度数值。
  /// 独立实现、不复用 TryVCP——避免改动高频热路径（DDCProbe/DDCRead/DDCWrite）的签名与行为。
  public static string DDCReadCurMax(string dev, byte code) {
    var curBox = new uint[1];
    var maxBox = new uint[1];
    var okBox = new bool[1];
    try {
      EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, (h, hdc, rc, lp) => {
        var mi = new SGMONITORINFO();
        mi.cb = Marshal.SizeOf(typeof(SGMONITORINFO));
        if (!GetMonitorInfo(h, ref mi)) return true;
        if (!mi.szDevice.Equals(dev, StringComparison.OrdinalIgnoreCase)) return true;
        uint n;
        if (!GetNumberOfPhysicalMonitorsFromHMONITOR(h, out n) || n == 0) return true;
        var arr = new SGPHYS_MON[n];
        if (!GetPhysicalMonitorsFromHMONITOR(h, n, arr)) return true;
        try {
          foreach (var pm in arr) {
            if (pm.h == IntPtr.Zero) continue;
            uint curVal = 0, maxVal = 0;
            bool ok = false;
            for (int t = 0; t < 5 && !ok; t++) {
              ok = GetVCPFeatureAndVCPFeatureReply(pm.h, code, IntPtr.Zero, ref curVal, ref maxVal);
              if (!ok && t + 1 < 5) System.Threading.Thread.Sleep(80);
            }
            if (ok) { curBox[0] = curVal; maxBox[0] = maxVal; okBox[0] = true; break; }
          }
        } finally {
          try { DestroyPhysicalMonitors(n, arr); } catch { }
        }
        return true;
      }, IntPtr.Zero);
    } catch {
      return "ERR";
    }
    return okBox[0] ? curBox[0] + "|" + maxBox[0] : "ERR";
  }

  /// 写 VCP 值，返回 OK / ERR
  public static string DDCWrite(string dev, byte code, uint val) {
    uint cur;
    return TryVCP(dev, code, true, val, 3, out cur) ? "OK" : "ERR:该显示器不支持 DDC/CI";
  }

  /// 探测是否支持 DDC（VCP 0x10 亮度可读）。重试少：列表刷新会逐屏调用，需快速返回。
  public static string DDCProbe(string dev) {
    uint cur;
    return TryVCP(dev, 0x10, false, 0, 2, out cur) ? "1" : "0";
  }

  // ---------- 屏幕电源控制（方案A：系统级 + DDC 精细） ----------
  [DllImport("user32.dll", SetLastError = true)]
  static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam, uint fuFlags, uint uTimeout, out IntPtr lpdwResult);
  [DllImport("user32.dll")] static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);

  /// 系统级关闭所有显示器：一次让全部屏进入待机（含 DDC 不通的屏，如本机小米）。
  /// 必须用 SendMessageTimeout 而非 SendMessage：广播是同步的，遇到无响应窗口会永久卡死
  /// （实测 SendMessage 广播卡住 90 秒以上），SMTO_ABORTIFHUNG + 超时保证必然返回。
  public static string ScreenOff() {
    IntPtr r;
    SendMessageTimeout((IntPtr)0xFFFF, 0x0112, (IntPtr)0xF170, (IntPtr)2, 0x0002, 3000, out r);
    return "OK";
  }

  /// 唤醒屏幕：合成一次 Shift 按下/抬起（无副作用的真实输入事件，可解除显示器待机）
  public static string ScreenWake() {
    keybd_event(0x10, 0, 0, UIntPtr.Zero);
    keybd_event(0x10, 0, 2, UIntPtr.Zero);
    return "OK";
  }

  /// 强制系统按数据库里保存的当前拓扑重新应用一遍显示配置，并触发重新枚举模式。
  /// 第一段 flags = SDC_APPLY(0x80) | SDC_USE_DATABASE_CURRENT(0x0F)——注意不能加
  /// SDC_FORCE_MODE_ENUMERATION(0x1000)：它只允许与 SDC_USE_SUPPLIED_DISPLAY_CONFIG
  /// 同用，与 DATABASE_CURRENT 组合会返回 87（ERROR_INVALID_PARAMETER，实测踩过）。
  /// 返回 OK / ERR:SetDisplayConfig 返回 <码>
  public static string Redetect() {
    int r = SetDisplayConfigApply(0, IntPtr.Zero, 0, IntPtr.Zero, 0x80 | 0x0F);
    if (r != 0) return "ERR:SetDisplayConfig 返回 " + r;
    // 第二段：补一次重新枚举（与「重新枚举」动作同源），刷新模式列表
    ChangeDisplaySettingsExW(null, IntPtr.Zero, IntPtr.Zero, 0, IntPtr.Zero);
    return "OK";
  }

  /// 显卡驱动软重置：合成 Win+Ctrl+Shift+B 组合键（Windows 官方的显示驱动重置快捷键）。
  /// 屏幕会黑 1~3 秒后恢复，等效于驱动重新初始化，不改任何显示设置。
  public static string GpuReset() {
    keybd_event(0x5B, 0, 0, UIntPtr.Zero); // Win down
    keybd_event(0x11, 0, 0, UIntPtr.Zero); // Ctrl down
    keybd_event(0x10, 0, 0, UIntPtr.Zero); // Shift down
    keybd_event(0x42, 0, 0, UIntPtr.Zero); // B down
    keybd_event(0x42, 0, 2, UIntPtr.Zero); // B up
    keybd_event(0x10, 0, 2, UIntPtr.Zero); // Shift up
    keybd_event(0x11, 0, 2, UIntPtr.Zero); // Ctrl up
    keybd_event(0x5B, 0, 2, UIntPtr.Zero); // Win up
    return "OK";
  }

  /// 读显示器电源模式（VCP 0xD6）：1=开 2=待机 4=软关 5=硬关
  public static string DDCPowerRead(string dev) {
    uint cur;
    return TryVCP(dev, 0xD6, false, 0, 5, out cur) ? cur.ToString() : "ERR";
  }

  /// 旋转副屏。newOri: 0=横, 1=顺时针90, 3=逆时针90
  /// 按 DEVMODEW 固定偏移「就地改」：只写 dmSize / dmFields / 方向 / 宽高，其余字节原样不动。
  /// （旧实现用 Explicit 结构体 PtrToStructure→StructureToPtr 整块回写，未声明区间会被清零）
  public static string Rotate(string dev, uint newOri) {
    var b = AllocDevMode();
    try {
      if (!EnumDisplaySettingsW(dev, -1, b)) return "ERR:读取当前显示模式失败";
      uint w = (uint)Marshal.ReadInt32(b, OFF_PELSWIDTH);
      uint h = (uint)Marshal.ReadInt32(b, OFF_PELSHEIGHT);
      uint curOri = (uint)Marshal.ReadInt32(b, OFF_ORIENTATION);
      bool curP = (curOri == 1 || curOri == 3);
      bool newP = (newOri == 1 || newOri == 3);
      if (curP != newP) { uint t = w; w = h; h = t; }
      Marshal.WriteInt32(b, OFF_ORIENTATION, (int)newOri);
      Marshal.WriteInt32(b, OFF_PELSWIDTH, (int)w);
      Marshal.WriteInt32(b, OFF_PELSHEIGHT, (int)h);
      Marshal.WriteInt32(b, OFF_DMFIELDS, (int)(DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY | DM_DISPLAYORIENTATION));
      Marshal.WriteInt16(b, OFF_DMSIZE, (short)DEVMODE_SIZE);
      int r = ChangeDisplaySettingsExW(dev, b, IntPtr.Zero, CDS_UPDATEREGISTRY | CDS_GLOBAL, IntPtr.Zero);
      return r == 0 ? "OK" : ("ERR:ChangeDisplaySettingsEx 返回 " + r);
    } finally { Marshal.FreeHGlobal(b); }
  }

  /// 设置指定显示设备的分辨率/刷新率（就地改 DEVMODE，只写宽/高/频率 + dmFields，其余字节原样保留）。
  /// 返回 OK / ERR:<原因>。w/h 为 0 表示该项不修改；hz 为 0 表示不修改刷新率。
  /// 用于驱动崩溃后把降级的分辨率恢复回显示器原生模式（等价于系统「设置→显示」里改分辨率）。
  public static string SetMode(string dev, uint w, uint h, uint hz) {
    var b = AllocDevMode();
    try {
      if (!EnumDisplaySettingsW(dev, -1, b)) return "ERR:读取当前显示模式失败";
      uint fields = 0;
      if (w != 0 && h != 0) {
        Marshal.WriteInt32(b, OFF_PELSWIDTH, (int)w);
        Marshal.WriteInt32(b, OFF_PELSHEIGHT, (int)h);
        fields |= DM_PELSWIDTH | DM_PELSHEIGHT;
      }
      if (hz != 0) {
        Marshal.WriteInt32(b, OFF_FREQUENCY, (int)hz);
        fields |= DM_DISPLAYFREQUENCY;
      }
      if (fields == 0) return "ERR:未指定要修改的项";
      Marshal.WriteInt32(b, OFF_DMFIELDS, (int)fields);
      Marshal.WriteInt16(b, OFF_DMSIZE, (short)DEVMODE_SIZE);
      int r = ChangeDisplaySettingsExW(dev, b, IntPtr.Zero, CDS_UPDATEREGISTRY | CDS_GLOBAL, IntPtr.Zero);
      return r == 0 ? "OK" : ("ERR:ChangeDisplaySettingsEx 返回 " + r);
    } finally { Marshal.FreeHGlobal(b); }
  }

  // ===== 显示器健康：KVM 切换 / EDID 重新协商异常的事件读取与修复 =====

  /// 读取最近的显示 / 设备链路异常事件（KVM 切换、显示器突然移除、驱动看门狗超时）。
  /// 返回每行: E|时间|等级|来源|事件ID|摘要
  /// 数据源：Kernel-PnP 设备管理（surprise removed = 显示器从系统里消失）、
  ///         Kernel-PnP 驱动看门狗（长时间阻塞）、System（驱动重置 TDR 4101）。
  public static string ReadDisplayEvents(int minutes) {
    var sb = new System.Text.StringBuilder();
    string[] logs = new string[] {
      "Microsoft-Windows-Kernel-PnP/Device Management",
      "Microsoft-Windows-Kernel-PnP/Driver Watchdog",
      "System"
    };
    long windowMs = (long)minutes * 60000L;
    foreach (string ln in logs) {
      try {
        var q = new System.Diagnostics.Eventing.Reader.EventLogQuery(
          ln, System.Diagnostics.Eventing.Reader.PathType.LogName,
          "*[System[TimeCreated[timediff(@SystemTime) <= " + windowMs + "]]]");
        var rd = new System.Diagnostics.Eventing.Reader.EventLogReader(q);
        int n = 0;
        for (var e = rd.ReadEvent(); e != null && n < 40; e = rd.ReadEvent()) {
          string prov = e.ProviderName == null ? "" : e.ProviderName;
          string msg = "";
          try { msg = e.FormatDescription(); } catch { }
          if (msg == null) msg = "";
          msg = msg.Replace((char)13, (char)32).Replace((char)10, (char)32);
          if (msg.Length > 120) msg = msg.Substring(0, 120);
          string low = msg.ToLower();
          bool keep = false;
          if (low.IndexOf("display") >= 0 || low.IndexOf("monitor") >= 0) keep = true;
          if (prov.IndexOf("PnP") >= 0) keep = true;
          if (e.Id == 4101 || e.Id == 1010 || e.Id == 900 || e.Id == 902 || e.Id == 933 || e.Id == 901) keep = true;
          if (!keep) continue;
          int lv = e.Level == null ? 4 : (int)e.Level;
          string lvl = lv == 1 ? "严重" : (lv == 2 ? "错误" : (lv == 3 ? "警告" : "信息"));
          string ts = e.TimeCreated == null ? "" : ((DateTime)e.TimeCreated).ToString("MM-dd HH:mm:ss");
          sb.Append("E|").Append(ts).Append("|").Append(lvl).Append("|").Append(prov)
            .Append("|").Append(e.Id).Append("|").Append(msg).Append((char)10);
          n++;
        }
        rd.Dispose();
      } catch { }
    }
    return sb.ToString();
  }

  /// 强制显示链路重新协商：先把刷新率降下来再切回原模式（两次 ChangeDisplaySettingsEx）。
  /// 用于修复 KVM 切换 / EDID 重新协商后常见的「画面发白发亮（色彩格式被降级成 YCbCr）、
  /// 清晰度变差、抖动花屏、刷新率异常」。返回 OK:<模式> 或 ERR:<原因>。
  public static string ForceReNegotiate(string dev) {
    var orig = AllocDevMode();
    try {
      if (!EnumDisplaySettingsW(dev, -1, orig)) return "ERR:读取当前显示模式失败";
      int w = Marshal.ReadInt32(orig, OFF_PELSWIDTH);
      int h = Marshal.ReadInt32(orig, OFF_PELSHEIGHT);
      int hz = Marshal.ReadInt32(orig, OFF_FREQUENCY);
      if (w <= 0 || h <= 0) return "ERR:当前模式无效";
      if (hz != 60) {
        var tmp = AllocDevMode();
        try {
          if (EnumDisplaySettingsW(dev, -1, tmp)) {
            Marshal.WriteInt32(tmp, OFF_FREQUENCY, 60);
            Marshal.WriteInt32(tmp, OFF_DMFIELDS, (int)DM_DISPLAYFREQUENCY);
            Marshal.WriteInt16(tmp, OFF_DMSIZE, (short)DEVMODE_SIZE);
            ChangeDisplaySettingsExW(dev, tmp, IntPtr.Zero, CDS_UPDATEREGISTRY | CDS_GLOBAL, IntPtr.Zero);
            System.Threading.Thread.Sleep(1200);
          }
        } finally { Marshal.FreeHGlobal(tmp); }
      }
      Marshal.WriteInt32(orig, OFF_DMFIELDS, (int)(DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY | DM_DISPLAYORIENTATION));
      Marshal.WriteInt16(orig, OFF_DMSIZE, (short)DEVMODE_SIZE);
      int r = ChangeDisplaySettingsExW(dev, orig, IntPtr.Zero, CDS_UPDATEREGISTRY | CDS_GLOBAL, IntPtr.Zero);
      return r == 0 ? ("OK:" + w + "x" + h + "@" + hz) : ("ERR:ChangeDisplaySettingsEx 返回 " + r);
    } finally { Marshal.FreeHGlobal(orig); }
  }

  /// 每台在用的屏的当前模式：M|设备名|宽|高|刷新率|方向
  /// 末尾追加活动输出信号分辨率（GetDisplayConfig 的 target mode），取不到时为 ?。
  static System.Collections.Generic.Dictionary<string, string> ActiveSignalSizes() {
    var result = new System.Collections.Generic.Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
    uint nPaths, nModes;
    if (GetDisplayConfigBufferSizes(2, out nPaths, out nModes) != 0 || nPaths == 0 || nModes == 0) return result;
    IntPtr pa = Marshal.AllocHGlobal((int)(nPaths * 72));
    IntPtr ma = Marshal.AllocHGlobal((int)(nModes * 64));
    try {
      uint p = nPaths, m = nModes;
      if (QueryDisplayConfigRaw(2, ref p, pa, ref m, ma, IntPtr.Zero) != 0) return result;
      for (int i = 0; i < p; i++) {
        int pathOff = i * 72;
        // DISPLAYCONFIG_PATH_INFO.targetInfo.modeInfoIdx is at +28. +32 is
        // outputTechnology (HDMI / DisplayPort / etc.), not a mode index. Using
        // +32 can accidentally read an unrelated mode and report bogus timings.
        int modeIndex = Marshal.ReadInt32(pa, pathOff + 28);
        if (modeIndex == 0xffff || (uint)modeIndex >= m) continue;
        int modeOff = modeIndex * 64;
        if (Marshal.ReadInt32(ma, modeOff) != 2) continue;
        int w = Marshal.ReadInt32(ma, modeOff + 40);
        int h = Marshal.ReadInt32(ma, modeOff + 44);
        if (w <= 0 || h <= 0) continue;
        // targetInfo.rotation is at +36; +40 is scaling.
        int rotation = Marshal.ReadInt32(pa, pathOff + 36);
        if (rotation == 2 || rotation == 4) { int swap = w; w = h; h = swap; }
        var src = new SGDC_SRC_NAME();
        src.header.type = 1;
        src.header.size = (uint)Marshal.SizeOf(typeof(SGDC_SRC_NAME));
        src.header.adapterId.LowPart = (uint)Marshal.ReadInt32(pa, pathOff);
        src.header.adapterId.HighPart = Marshal.ReadInt32(pa, pathOff + 4);
        src.header.id = (uint)Marshal.ReadInt32(pa, pathOff + 8);
        if (DisplayConfigGetDeviceInfo(ref src) != 0 || string.IsNullOrEmpty(src.viewGdiDeviceName)) continue;
        result[src.viewGdiDeviceName] = w + "x" + h;
      }
      return result;
    } finally { Marshal.FreeHGlobal(pa); Marshal.FreeHGlobal(ma); }
  }

  /// Update only active source origins while preserving the queried source and target modes.
  /// Map format: GDI device|x|y;GDI device|x|y. Validate the supplied config before applying it.
  public static string SetDisplayPositions(string mapping, bool apply) {
    var requested = new System.Collections.Generic.Dictionary<string, int[]>(StringComparer.OrdinalIgnoreCase);
    foreach (string row in (mapping ?? "").Split(new char[] { (char)59 }, StringSplitOptions.RemoveEmptyEntries)) {
      string[] fields = row.Split((char)124);
      int x, y;
      if (fields.Length != 3 || !int.TryParse(fields[1], out x) || !int.TryParse(fields[2], out y))
        return "ERR:显示器位置参数无效";
      requested[fields[0]] = new int[] { x, y };
    }
    if (requested.Count == 0) return "ERR:没有需要设置的显示器位置";

    uint nPaths, nModes;
    int sizeResult = GetDisplayConfigBufferSizes(2, out nPaths, out nModes);
    if (sizeResult != 0 || nPaths == 0 || nModes == 0)
      return "ERR:GetDisplayConfigBufferSizes 失败（" + sizeResult + "）";
    IntPtr paths = Marshal.AllocHGlobal(checked((int)nPaths * 72));
    IntPtr modes = Marshal.AllocHGlobal(checked((int)nModes * 64));
    try {
      uint pathCount = nPaths, modeCount = nModes;
      int queryResult = QueryDisplayConfigRaw(2, ref pathCount, paths, ref modeCount, modes, IntPtr.Zero);
      if (queryResult != 0)
        return "ERR:QueryDisplayConfig 读取活动路径失败（" + queryResult + "）";

      var found = new System.Collections.Generic.HashSet<string>(StringComparer.OrdinalIgnoreCase);
      for (int i = 0; i < pathCount; i++) {
        int pathOffset = i * 72;
        int sourceModeIndex = Marshal.ReadInt32(paths, pathOffset + 12);
        if (sourceModeIndex == -1 || sourceModeIndex < 0 || (uint)sourceModeIndex >= modeCount)
          return "ERR:活动显示路径缺少可编辑的源模式";
        int modeOffset = sourceModeIndex * 64;
        if (Marshal.ReadInt32(modes, modeOffset) != 1)
          return "ERR:活动显示路径引用的模式不是源模式";

        var sourceName = new SGDC_SRC_NAME();
        sourceName.header.type = 1;
        sourceName.header.size = (uint)Marshal.SizeOf(typeof(SGDC_SRC_NAME));
        sourceName.header.adapterId.LowPart = (uint)Marshal.ReadInt32(paths, pathOffset);
        sourceName.header.adapterId.HighPart = Marshal.ReadInt32(paths, pathOffset + 4);
        sourceName.header.id = (uint)Marshal.ReadInt32(paths, pathOffset + 8);
        int nameResult = DisplayConfigGetDeviceInfo(ref sourceName);
        if (nameResult != 0 || string.IsNullOrEmpty(sourceName.viewGdiDeviceName))
          return "ERR:无法读取活动显示源设备名（" + nameResult + "）";

        int[] position;
        if (!requested.TryGetValue(sourceName.viewGdiDeviceName, out position)) continue;
        // DISPLAYCONFIG_SOURCE_MODE starts at modeInfo + 16; position is after width,
        // height, and pixelFormat (x at +28, y at +32 from the modeInfo base).
        Marshal.WriteInt32(modes, modeOffset + 28, position[0]);
        Marshal.WriteInt32(modes, modeOffset + 32, position[1]);
        found.Add(sourceName.viewGdiDeviceName);
      }
      if (found.Count != requested.Count)
        return "ERR:部分设备不在当前活动显示路径中（命中 " + found.Count + "/" + requested.Count + "）";

      int validateResult = SetDisplayConfigApply(pathCount, paths, modeCount, modes, 0x40 | 0x20);
      if (validateResult != 0)
        return "ERR:SetDisplayConfig 校验位置布局失败（" + validateResult + "）";
      if (!apply) return "OK|活动显示位置配置校验通过";

      int applyResult = SetDisplayConfigApply(pathCount, paths, modeCount, modes, 0x80 | 0x20 | 0x200);
      if (applyResult != 0)
        return "ERR:SetDisplayConfig 应用位置布局失败（" + applyResult + "）";
      return "OK|SetDisplayConfig 已保留显示模式并更新桌面位置";
    } catch (Exception e) {
      return "ERR:SetDisplayConfig 位置调整异常：" + e.Message;
    } finally {
      Marshal.FreeHGlobal(paths);
      Marshal.FreeHGlobal(modes);
    }
  }

  /// 每台在用的屏的当前模式：M|设备名|宽|高|刷新率|方向|活动输出宽x高
  public static string ModeDetail() {
    var sb = new System.Text.StringBuilder();
    var signals = ActiveSignalSizes();
    for (int i = 1; i <= 12; i++) {
      string dev = "\\\\.\\DISPLAY" + i;
      var dm = AllocDevMode();
      try {
        if (EnumDisplaySettingsW(dev, -1, dm)) {
          int w = Marshal.ReadInt32(dm, OFF_PELSWIDTH);
          int h = Marshal.ReadInt32(dm, OFF_PELSHEIGHT);
          int hz = Marshal.ReadInt32(dm, OFF_FREQUENCY);
          int ori = Marshal.ReadInt32(dm, OFF_ORIENTATION);
          if (w > 0 && h > 0)
            sb.Append("M|").Append(dev).Append("|").Append(w).Append("|").Append(h).Append("|").Append(hz).Append("|").Append(ori).Append("|").Append(signals.ContainsKey(dev) ? signals[dev] : "?").Append((char)10);
        }
      } finally { Marshal.FreeHGlobal(dm); }
    }
    return sb.ToString();
  }

  // ---------------- 色彩配置（ICC）关联 ----------------
  // 走 GDI 设备上下文——实测这是 Windows 色彩引擎真正读取的路径。
  // 实测无效、已弃用的做法：WcsAssociateColorProfileWithDevice 无论设备名对错都返回 TRUE，
  // 关联却从不落地；SetICMProfileW 在权限不足时同样返回 TRUE 但不生效 → 因此必须回读校验。
  static string IccOnHandle(IntPtr hdc) {
    var sb = new StringBuilder(1024);
    uint cb = 1024;
    return GetICMProfileW(hdc, ref cb, sb) ? sb.ToString() : null;
  }

  /// 开一个**新**的设备上下文读当前关联（必须用新 DC：
  /// 同一 DC 会缓存刚 SetICMProfileW 写入的值，用它回读会得出「总是成功」的假象）
  static string IccReadFresh(string dev) {
    IntPtr hdc = CreateDCW("DISPLAY", dev, null, IntPtr.Zero);
    if (hdc == IntPtr.Zero) return null;
    try { return IccOnHandle(hdc); } finally { DeleteDC(hdc); }
  }

  /// 读某显示设备当前关联的 ICC 路径 → OK:<path> / ERR:<原因>
  public static string IccGet(string dev) {
    string p = IccReadFresh(dev);
    return p == null ? "ERR:无法打开显示设备或 GetICMProfile 无返回" : ("OK:" + p);
  }

  /// 关联 ICC 到指定显示设备 → OK / ERR:<原因>
  /// 关键：写入后**另开 DC** 回读校验才算成功
  /// （Windows 权限不足时 SetICMProfileW 照样返回 TRUE，但关联不会落到设备上）
  public static string IccSet(string dev, string profile) {
    IntPtr hdc = CreateDCW("DISPLAY", dev, null, IntPtr.Zero);
    if (hdc == IntPtr.Zero) return "ERR:无法打开显示设备";
    try { SetICMProfileW(hdc, profile); } finally { DeleteDC(hdc); }
    string now = IccReadFresh(dev);
    if (now != null && string.Equals(now, profile, StringComparison.OrdinalIgnoreCase)) return "OK";
    return "ERR:未生效(回读=" + (now == null ? "读取失败" : now) + ")";
  }

  /// 安装 ICC 到系统色彩目录（Windows 只认该目录下的配置；实测非管理员可用）
  public static string IccInstall(string profile) {
    try { return InstallColorProfileW(IntPtr.Zero, profile) ? "OK" : "ERR:InstallColorProfile 失败"; }
    catch { return "ERR:InstallColorProfile 异常"; }
  }

  // ---------------- 剪贴板（软 KVM 用，raw Win32，不依赖 WinForms） ----------------
  [DllImport("user32.dll")] static extern bool OpenClipboard(IntPtr h);
  [DllImport("user32.dll")] static extern bool CloseClipboard();
  [DllImport("user32.dll")] static extern bool EmptyClipboard();
  [DllImport("user32.dll")] static extern IntPtr SetClipboardData(uint f, IntPtr h);
  [DllImport("user32.dll")] static extern IntPtr GetClipboardData(uint f);
  [DllImport("user32.dll")] static extern bool IsClipboardFormatAvailable(uint f);
  [DllImport("kernel32.dll")] static extern IntPtr GlobalAlloc(uint flags, UIntPtr bytes);
  [DllImport("kernel32.dll")] static extern IntPtr GlobalLock(IntPtr h);
  [DllImport("kernel32.dll")] static extern bool GlobalUnlock(IntPtr h);
  [DllImport("kernel32.dll")] static extern UIntPtr GlobalSize(IntPtr h);

  /// 读剪贴板纯文本 → 文本内容 / NONE / ERR:<原因>
  public static string ClipboardGet() {
    try {
      if (!IsClipboardFormatAvailable(13)) return "NONE";
      if (!OpenClipboard(IntPtr.Zero)) return "ERR:OpenClipboard 失败";
      try {
        IntPtr h = GetClipboardData(13);
        if (h == IntPtr.Zero) return "NONE";
        IntPtr p = GlobalLock(h);
        if (p == IntPtr.Zero) return "ERR:GlobalLock 失败";
        try {
          int bytes = (int)GlobalSize(h);
          if (bytes <= 0) return "NONE";
          int chars = bytes / 2;
          if (chars > 131072) chars = 131072;
          string s = Marshal.PtrToStringUni(p, chars);
          if (s == null) return "NONE";
          int z = s.IndexOf("\0", StringComparison.Ordinal);
          if (z >= 0) s = s.Substring(0, z);
          return s;
        } finally { GlobalUnlock(h); }
      } finally { CloseClipboard(); }
    } catch (Exception e) { return "ERR:" + e.Message; }
  }

  /// 写剪贴板纯文本（CF_UNICODETEXT）→ OK / ERR:<原因>
  public static string ClipboardSet(string text) {
    if (text == null) text = "";
    try {
      if (!OpenClipboard(IntPtr.Zero)) return "ERR:OpenClipboard 失败";
      try {
        EmptyClipboard();
        IntPtr h = GlobalAlloc(2, (UIntPtr)((text.Length + 1) * 2));
        if (h == IntPtr.Zero) return "ERR:GlobalAlloc 失败";
        IntPtr p = GlobalLock(h);
        if (p == IntPtr.Zero) return "ERR:GlobalLock 失败";
        try {
          if (text.Length > 0) Marshal.Copy(text.ToCharArray(), 0, p, text.Length);
          Marshal.WriteInt16(p, text.Length * 2, 0);
        } finally { GlobalUnlock(h); }
        if (SetClipboardData(13, h) == IntPtr.Zero) return "ERR:SetClipboardData 失败";
        return "OK";
      } finally { CloseClipboard(); }
    } catch (Exception e) { return "ERR:" + e.Message; }
  }
}

// ---------------- 系统音频端点（CoreAudio COM） ----------------
// 为什么只有 DDC 0x62 不够：0x62 只作用于「显示器内置喇叭」，笔记本内建喇叭 /
// 耳机 / 蓝牙根本没有这条通路；而部分 HDMI/DP 显示器音频端点是固定音量，
// Windows 系统滑块也调不动——所以补上「系统默认输出设备」的音量控制，两者互补。
// 接口序与 CoreAudio 官方 vtable 严格一致；接口声明省略尾部方法不影响已声明方法。
[ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
public class SGMMEnumerator { }

[Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface SGIMMEnum {
  int EnumAudioEndpoints(int flow, int mask, out SGIMMColl coll);
  int GetDefaultAudioEndpoint(int flow, int role, out SGIMMDev dev);
}

[Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface SGIMMColl {
  int GetCount(out int count);
  int Item(int index, out SGIMMDev dev);
}

[Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface SGIMMDev {
  int Activate(ref Guid iid, uint clsCtx, IntPtr actParams, [MarshalAs(UnmanagedType.IUnknown)] out object iface);
  int OpenPropertyStore(int stgm, out SGPropStore store);
  int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
  int GetState(out int state);
}

[Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface SGPropStore {
  int GetCount(out int count);
  int GetAt(int index, out SGPKEY key);
  int GetValue(ref SGPKEY key, out SGPV value);
}

[Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface SGEndVol {
  int RegisterControlChangeNotify(IntPtr n);
  int UnregisterControlChangeNotify(IntPtr n);
  int GetChannelCount(out uint c);
  int SetMasterVolumeLevel(float db, IntPtr ctx);
  int SetMasterVolumeLevelScalar(float level, IntPtr ctx);
  int GetMasterVolumeLevel(out float db);
  int GetMasterVolumeLevelScalar(out float level);
  int SetChannelVolumeLevel(uint ch, float db, IntPtr ctx);
  int SetChannelVolumeLevelScalar(uint ch, float level, IntPtr ctx);
  int GetChannelVolumeLevel(uint ch, out float db);
  int GetChannelVolumeLevelScalar(uint ch, out float level);
  // 注意：这里必须用 int（Win32 BOOL，4 字节）。COM 互操作里 bool 默认按
  // VARIANT_BOOL（2 字节）编组，而 IAudioEndpointVolume 的静音参数是 BOOL——
  // API 写 4 字节进 2 字节缓冲会破坏栈帧，下一句托管代码报 NullReferenceException
  // （实测：AudioGet 因此全挂，AudioSet 因不调 GetMute 而幸存）。
  int SetMute(int mute, IntPtr ctx);
  int GetMute(out int mute);
  int GetVolumeStepInfo(out uint step, out uint steps);
  int VolumeStepUp(IntPtr ctx);
  int VolumeStepDown(IntPtr ctx);
  int QueryHardwareSupport(out uint mask);
  int GetVolumeRange(out float minDb, out float maxDb, out float incDb);
}

[StructLayout(LayoutKind.Sequential)]
public struct SGPKEY { public Guid fmtid; public uint pid; }

// PROPVARIANT 只消费两种：VT_LPWSTR(31) 指针 / VT_UI4(19) 内联整数。
// 关键坑：内联值放在 union 本体里，若把它当指针 Marshal.ReadInt32(v.p) =
// 按值解引用 → 访问违例，进程直接崩（PS 的 try/catch 也拦不住）。
// 必须用重叠字段原地读。
[StructLayout(LayoutKind.Explicit)]
public struct SGPV {
  [FieldOffset(0)] public ushort vt;
  [FieldOffset(8)] public IntPtr p;
  [FieldOffset(8)] public int i32;
}

// 未公开 COM 接口 IPolicyConfig：切换系统默认音频输出设备的唯一途径
// （SoundSwitch / AudioDeviceCmdlets 同款方案，Win7~Win11 可用）。
// 虚表 12 个方法必须按此顺序声明，SetDefaultEndpoint 在第 11 位。
[Guid("f8679f50-850a-41cf-9c72-430f290290c8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface SGPolicyCfg {
  [PreserveSig] int GetMixFormat(string a, IntPtr b);
  [PreserveSig] int GetDeviceFormat(string a, bool b, IntPtr c);
  [PreserveSig] int ResetDeviceFormat(string a);
  [PreserveSig] int SetDeviceFormat(string a, IntPtr b, IntPtr c);
  [PreserveSig] int GetProcessingPeriod(string a, bool b, IntPtr c, IntPtr d);
  [PreserveSig] int SetProcessingPeriod(string a, IntPtr b);
  [PreserveSig] int GetShareMode(string a, IntPtr b);
  [PreserveSig] int SetShareMode(string a, IntPtr b);
  [PreserveSig] int GetPropertyValue(string a, bool b, IntPtr c, IntPtr d);
  [PreserveSig] int SetPropertyValue(string a, bool b, IntPtr c, IntPtr d);
  [PreserveSig] int SetDefaultEndpoint(string a, int role);
  [PreserveSig] int SetEndpointVisibility(string a, bool b);
}
[ComImport, Guid("870af99c-171d-4f9e-af0d-e63df40c2bc9")]
class SGPolicyClient { }

public class SGAudio {
  static Guid IID_VOL = new Guid("5CDF2C82-841E-4546-9722-0CF74078229A");

  static string Esc(string s) {
    if (s == null) return "";
    return s.Replace("|", "/").Replace("\r", " ").Replace("\n", " ");
  }

  // 默认渲染端点（eRender + eMultimedia：媒体应用的默认输出，与系统托盘一致）
  static SGIMMDev DefaultDev() {
    var en = (SGIMMEnum)(object)new SGMMEnumerator();
    SGIMMDev dev;
    return en.GetDefaultAudioEndpoint(0, 1, out dev) == 0 ? dev : null;
  }

  static SGEndVol VolOf(SGIMMDev dev) {
    object o;
    if (dev.Activate(ref IID_VOL, 23, IntPtr.Zero, out o) != 0 || o == null) return null;
    return (SGEndVol)o;
  }

  /// 默认输出端点信息：OK|名称|音量0-100|静音|硬件支持掩码|形态因子(9=HDMI/DP音频)
  /// 全程分步防御：任何一步失败返回带步骤标记的 ERR，而不是笼统的 NRE
  public static string AudioGet() {
    try {
      var en = (SGIMMEnum)(object)new SGMMEnumerator();
      SGIMMDev dev;
      int hr = en.GetDefaultAudioEndpoint(0, 1, out dev);
      if (hr != 0 || dev == null) return "ERR:枚举默认设备失败 hr=" + hr;
      string name = "", ff = "";
      try {
        SGPropStore store;
        if (dev.OpenPropertyStore(0, out store) == 0 && store != null) {
          SGPV v;
          SGPKEY kn = new SGPKEY(); kn.fmtid = new Guid("a45c254e-df1c-4efd-8020-67d146a850e0"); kn.pid = 14;
          if (store.GetValue(ref kn, out v) == 0 && v.vt == 31 && v.p != IntPtr.Zero) {
            string s = Marshal.PtrToStringUni(v.p);
            if (s != null) name = s;
          }
          SGPKEY kf = new SGPKEY(); kf.fmtid = new Guid("1da5d803-d492-4edd-8c23-e0c0ffee7f0e"); kf.pid = 0;
          if (store.GetValue(ref kf, out v) == 0 && (v.vt == 19 || v.vt == 3)) ff = v.i32.ToString();
        }
      } catch (Exception) { name = name.Length > 0 ? name : "默认输出设备"; }
      object o;
      Guid iid = new Guid("5CDF2C82-841E-4546-9722-0CF74078229A");
      int ahr = dev.Activate(ref iid, 23, IntPtr.Zero, out o);
      if (ahr != 0 || o == null) return "ERR:端点激活失败 hr=" + ahr;
      SGEndVol vol = (SGEndVol)o;
      float lvl; uint mask; int mute;
      vol.GetMasterVolumeLevelScalar(out lvl);
      vol.QueryHardwareSupport(out mask);
      vol.GetMute(out mute);
      return "OK|" + Esc(name) + "|" + Math.Round(lvl * 100) + "|" + (mute != 0 ? 1 : 0) + "|" + mask + "|" + ff;
    } catch (Exception e) { return "ERR:" + Esc(e.Message); }
  }

  /// 设置系统音量 0-100（默认输出端点）
  public static string AudioSet(uint v) {
    try {
      SGIMMDev dev = DefaultDev();
      if (dev == null) return "ERR:没有默认输出设备";
      SGEndVol vol = VolOf(dev);
      if (vol == null) return "ERR:端点激活失败";
      if (v > 100) v = 100;
      return vol.SetMasterVolumeLevelScalar(v / 100f, IntPtr.Zero) == 0
        ? "OK" : "ERR:设置失败（端点可能为固定音量，常见于部分 HDMI/DP 音频）";
    } catch (Exception e) { return "ERR:" + Esc(e.Message); }
  }

  /// 系统静音开关：on 非 0 = 静音
  public static string AudioMute(uint on) {
    try {
      SGIMMDev dev = DefaultDev();
      if (dev == null) return "ERR:没有默认输出设备";
      SGEndVol vol = VolOf(dev);
      if (vol == null) return "ERR:端点激活失败";
      return vol.SetMute(on != 0 ? 1 : 0, IntPtr.Zero) == 0 ? "OK" : "ERR:静音设置失败";
    } catch (Exception e) { return "ERR:" + Esc(e.Message); }
  }

  /// 列出所有「活动的输出端点」，每行：A|端点id|名称|音量|静音|可调|形态
  /// 为什么需要：部分 HDMI/DP 显示器音频端点是**固定音量**，Windows 自己的音量条
  /// 也调不动。此时不该让滑块假装能用，而应让用户从列表里挑一个真正能调的设备。
  struct SGWA { public int l; public int t; public int r; public int b; }
  [DllImport("user32.dll", CharSet = CharSet.Auto)]
  static extern bool SystemParametersInfo(uint act, uint u, ref SGWA p, uint f);

  /// 主屏「工作区」右下角（已排除任务栏）——通知气泡要贴这个角
  /// 用 SPI_GETWORKAREA(0x30) 而不是自己算：它带正确的原点偏移且自动避开任务栏
  public static string PrimaryWorkArea() {
    try {
      SGWA w = new SGWA();
      if (SystemParametersInfo(0x0030, 0, ref w, 0) && w.r > w.l && w.b > w.t) {
        return "WA|" + w.l + "|" + w.t + "|" + w.r + "|" + w.b;
      }
    } catch { }
    return "WA|0|0|0|0";
  }

  // ---- 双屏对齐体检：枚举某块屏支持的所有分辨率 ----
  // 注意：这里**不用结构体**，直接用显式字节偏移操作 220 字节缓冲区 ——
  // 之前用 [StructLayout] 声明 DEVMODEW，dmSize/偏移没对上，EnumDisplaySettingsW 第一次就返回 false，
  // 结果「可用分辨率只有 1 种」。DEVMODEW(Unicode) 布局（共 220 字节）：
  //   0..63 dmDeviceName[32] · 64 dmSpecVersion · 66 dmDriverVersion · 68 dmSize · 70 dmDriverExtra
  //   72 dmFields · 76..91 打印参数联合体 · 92 dmColor … 100 dmCollate
  //   102..165 dmFormName[32] · 166 dmLogPixels · 172 dmBitsPerPel · 176 dmPelsWidth ·
  //   180 dmPelsHeight · 184 dmDisplayFlags · 188 dmDisplayFrequency
  [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "EnumDisplaySettingsW")]
  static extern bool EnumDisplaySettingsW(string dev, int mode, IntPtr dm);

  /// 按显示器编号枚举分辨率（**只传数字**：设备名里的 `\\.\` 反斜杠经
  /// 命令行传给 PowerShell 时会被吃掉一个，实测会变成 `\.\DISPLAY1` 导致 API 报 203）
  public static string DisplayModesNum(int n) {
    return DisplayModes("\\\\.\\DISPLAY" + n);
  }

  /// 列出某块屏支持的分辨率（去重，按面积从大到小），形如 MODES|<宽x高 …>|<个数>
  public static string DisplayModes(string dev) {
    const int SZ = 220;
    IntPtr buf = Marshal.AllocHGlobal(SZ);
    try {
      var list = new System.Collections.Generic.List<int[]>();
      var seen = new System.Collections.Generic.HashSet<string>();
      for (int i = 0; i < 4000; i++) {
        for (int b = 0; b < SZ; b++) Marshal.WriteByte(buf, b, 0);
        Marshal.WriteInt16(buf, 68, (short)SZ);
        if (!EnumDisplaySettingsW(dev, i, buf)) break;
        int w = Marshal.ReadInt32(buf, 176);
        int h = Marshal.ReadInt32(buf, 180);
        int f = Marshal.ReadInt32(buf, 188);
        if (w >= 640 && h >= 480 && f >= 24) {
          string k = w + "x" + h;
          if (seen.Add(k)) list.Add(new int[] { w, h });
        }
      }
      list.Sort(delegate(int[] a, int[] b) { return (b[0] * b[1]).CompareTo(a[0] * a[1]); });
      var sb = new StringBuilder();
      foreach (int[] m in list) sb.Append(m[0]).Append("x").Append(m[1]).Append(" ");
      return "MODES|" + sb.ToString().Trim() + "|" + list.Count;
    } catch (Exception e) { return "MODES||0|ERR:" + Esc(e.Message); }
    finally { Marshal.FreeHGlobal(buf); }
  }

  // ---- 结构化体检：枚举某块屏全部支持模式（含刷新率），推导原生分辨率 ----
  // 每个分辨率取最高刷新率。输出：nativeWxH|原生下最高Hz|WxH@Hz,WxH@Hz,...
  // 原生分辨率 = 面积最大的支持模式（数字面板驱动总把原生排进支持列表）
  public static string ModeStats(string dev) {
    const int SZ = 220;
    IntPtr buf = Marshal.AllocHGlobal(SZ);
    try {
      var best = new System.Collections.Generic.Dictionary<string, int>();
      for (int i = 0; i < 4000; i++) {
        for (int b = 0; b < SZ; b++) Marshal.WriteByte(buf, b, 0);
        Marshal.WriteInt16(buf, 68, (short)SZ);
        if (!EnumDisplaySettingsW(dev, i, buf)) break;
        int w = Marshal.ReadInt32(buf, 176);
        int h = Marshal.ReadInt32(buf, 180);
        int f = Marshal.ReadInt32(buf, 188);
        if (w < 640 || h < 480 || f < 24) continue;
        string k = w + "x" + h;
        // 注意：SGCore 由 PowerShell 5.1 的 Add-Type 编译（C# 5），out 变量必须先声明，
        // 不能写 `out int cur` 内联声明（会让整个程序集编译失败、get_displays 全空）
        int cur;
        if (!best.TryGetValue(k, out cur) || f > cur) best[k] = f;
      }
      if (best.Count == 0) return "ERR:没有枚举到任何支持模式";
      int nw = 0, nh = 0;
      foreach (var kv in best) {
        // 注意：不用 IndexOf 字符字面量——CORE_CS 禁单引号（check_repo 守护规则）
        int ix = kv.Key.IndexOf("x", System.StringComparison.Ordinal);
        int w = int.Parse(kv.Key.Substring(0, ix));
        int h = int.Parse(kv.Key.Substring(ix + 1));
        if ((long)w * h > (long)nw * nh) { nw = w; nh = h; }
      }
      var parts = new System.Collections.Generic.List<string>();
      foreach (var kv in best) parts.Add(kv.Key + "@" + kv.Value);
      parts.Sort(StringComparer.Ordinal);
      return nw + "x" + nh + "|" + best[nw + "x" + nh] + "|" + string.Join(",", parts);
    } catch (Exception e) { return "ERR:" + Esc(e.Message); }
    finally { Marshal.FreeHGlobal(buf); }
  }

  public static string AudioList() {
    var sb = new StringBuilder();
    try {
      var en = (SGIMMEnum)(object)new SGMMEnumerator();
      SGIMMColl coll;
      // mask = 0x0F(DEVICE_STATEMASK_ALL)：把「未插入/未激活/已禁用」的端点也列出来，
      // 否则用户会问「我的副屏音频设备去哪了」——副屏的 HDMI/DP 音频常常处于未激活态。
      if (en.EnumAudioEndpoints(0, 15, out coll) != 0 || coll == null) return "ERR:枚举输出端点失败";
      int cnt;
      if (coll.GetCount(out cnt) != 0) return "ERR:读取端点数量失败";
      for (int i = 0; i < cnt; i++) {
        // 注意：这次特意连「未激活/未插入」的端点一起枚举（用户要看到副屏设备），
        // 而对这类端点做 Activate 会抛 0x80070003「系统找不到指定的路径」。
        // 所以逐个用 try 包住：某个端点坏掉只是跳过它，不能让整个列表打不开。
        try {
        SGIMMDev dev;
        if (coll.Item(i, out dev) != 0 || dev == null) continue;
        string id = "";
        try { dev.GetId(out id); } catch { }
        string name = "";
        string ff = "0";
        try {
          SGPropStore store;
          if (dev.OpenPropertyStore(0, out store) == 0 && store != null) {
            SGPV v;
            SGPKEY kn = new SGPKEY(); kn.fmtid = new Guid("a45c254e-df1c-4efd-8020-67d146a850e0"); kn.pid = 14;
            if (store.GetValue(ref kn, out v) == 0 && v.vt == 31 && v.p != IntPtr.Zero) {
              string s = Marshal.PtrToStringUni(v.p);
              if (s != null) name = s;
            }
            SGPKEY kf = new SGPKEY(); kf.fmtid = new Guid("1da5d803-d492-4edd-8c23-e0c0ffee7f0e"); kf.pid = 0;
            if (store.GetValue(ref kf, out v) == 0 && (v.vt == 19 || v.vt == 3)) ff = v.i32.ToString();
          }
        } catch { }
        SGEndVol vol = VolOf(dev);
        if (vol == null) continue;
        float lvl = 0; uint mask = 0; int mute = 0;
        try { vol.GetMasterVolumeLevelScalar(out lvl); } catch { }
        try { vol.QueryHardwareSupport(out mask); } catch { }
        try { vol.GetMute(out mute); } catch { }
        sb.Append("A|").Append(Esc(id)).Append("|").Append(Esc(name)).Append("|")
          .Append(Math.Round(lvl * 100)).Append("|").Append(mute != 0 ? 1 : 0).Append("|")
          .Append((mask & 1) != 0 ? 1 : 0).Append("|").Append(ff).Append("|")
          .Append(IsDefaultId(id) ? 1 : 0).Append((char)10);
        } catch { }
      }
      return sb.ToString();
    } catch (Exception e) { return "ERR:" + Esc(e.Message); }
  }

  /// 该端点是否就是 Windows 当前的默认输出设备
  static bool IsDefaultId(string id) {
    try {
      SGIMMDev d = DefaultDev();
      if (d == null) return false;
      string did = "";
      d.GetId(out did);
      return did == id;
    } catch { return false; }
  }

  static SGIMMDev DevById(string id) {
    if (id == null || id.Length == 0) return DefaultDev();
    try {
      var en = (SGIMMEnum)(object)new SGMMEnumerator();
      SGIMMColl coll;
      if (en.EnumAudioEndpoints(0, 1, out coll) != 0 || coll == null) return DefaultDev();
      int cnt; coll.GetCount(out cnt);
      for (int i = 0; i < cnt; i++) {
        SGIMMDev d;
        if (coll.Item(i, out d) != 0 || d == null) continue;
        string did = "";
        try { d.GetId(out did); } catch { }
        if (did == id) return d;
      }
    } catch { }
    return DefaultDev();
  }

  /// 设定指定输出端点的音量（id 为空则用系统默认端点）
  public static string AudioSetDev(string id, uint v) {
    try {
      SGIMMDev dev = DevById(id);
      if (dev == null) return "ERR:没有可用的输出设备";
      SGEndVol vol = VolOf(dev);
      if (vol == null) return "ERR:端点激活失败";
      if (v > 100) v = 100;
    int r = vol.SetMasterVolumeLevelScalar(v / 100f, IntPtr.Zero);
    return r == 0 ? "OK" : ("ERR:设置失败 hr=" + r + "（该端点是固定音量，换一个输出设备）");
  } catch (Exception e) { return "ERR:" + Esc(e.Message); }
  }

  /// 把系统默认输出设备切到指定端点（软 KVM 音频切换用）
  /// role 0=eConsole（与托盘「默认设备」一致）
  public static string AudioSetDefault(string id) {
    try {
      if (id == null || id.Length == 0) return "ERR:端点 id 为空";
      object o = new SGPolicyClient();
      SGPolicyCfg pc = (SGPolicyCfg)o;
      int hr = pc.SetDefaultEndpoint(id, 0);
      return hr == 0 ? "OK" : "ERR:hr=0x" + hr.ToString("X");
    } catch (Exception e) { return "ERR:" + Esc(e.Message); }
  }

  /// 指定输出端点静音开关
  public static string AudioMuteDev(string id, uint on) {
    try {
      SGIMMDev dev = DevById(id);
      if (dev == null) return "ERR:没有可用的输出设备";
      SGEndVol vol = VolOf(dev);
      if (vol == null) return "ERR:端点激活失败";
      return vol.SetMute(on != 0 ? 1 : 0, IntPtr.Zero) == 0 ? "OK" : "ERR:静音设置失败";
    } catch (Exception e) { return "ERR:" + Esc(e.Message); }
  }
}

// ============ 电池设备（SetupAPI + Battery IOCTL） ============
// 所有带电池的连接设备——笔记本内电池 / USB·2.4G 无线 HID 键鼠 / 蓝牙（含 BLE GATT
// 电量服务 0x180F）——都在 Battery 设备类注册接口；电池驱动注册的设备接口 GUID 恰好
// 就是 GUID_DEVCLASS_BATTERY(72631E54-78A4-11D0-BCF7-00AA00B7B32A)，因此用
// DIGCF_DEVICEINTERFACE | DIGCF_PRESENT 枚举。与驱动栈状态无关，不碰注册表只读。
[StructLayout(LayoutKind.Sequential)]
public struct SGIFACE_DATA { public int cb; public Guid cls; public uint Flags; public IntPtr Reserved; }

[StructLayout(LayoutKind.Sequential)]
public struct SGDEVINFO_DATA { public int cb; public Guid cls; public uint DevInst; public IntPtr Reserved; }

public class SGBatt {
  // IOCTL_BATTERY_*：FILE_DEVICE_BATTERY=0x29，FILE_READ_ACCESS=1，METHOD_BUFFERED=0
  // CTL_CODE(0x29, Function, 0, 1) = 0x290000|0x4000|(Function<<2)
  const uint IOCTL_TAG = 0x294040;        // QUERY_TAG      (Function 0x10)
  const uint IOCTL_INFO = 0x294044;       // QUERY_INFORMATION (Function 0x11)
  const uint IOCTL_STATUS = 0x29404c;     // QUERY_STATUS   (Function 0x13)
  // BATTERY_QUERY_INFORMATION.InformationLevel
  const int LEVEL_INFO = 0, LEVEL_NAME = 4;
  // BATTERY_STATUS.PowerState 位（ntddbat.h）
  const uint ST_ON_LINE = 0x1, ST_DISCHARGING = 0x2, ST_CHARGING = 0x4;

  [DllImport("setupapi.dll", CharSet = CharSet.Unicode)] static extern IntPtr SetupDiGetClassDevsW(ref Guid g, string e, IntPtr p, uint f);
  [DllImport("setupapi.dll", CharSet = CharSet.Unicode)] static extern bool SetupDiEnumDeviceInterfaces(IntPtr h, IntPtr di, ref Guid g, uint i, ref SGIFACE_DATA d);
  [DllImport("setupapi.dll", CharSet = CharSet.Unicode)] static extern bool SetupDiGetDeviceInterfaceDetailW(IntPtr h, ref SGIFACE_DATA d, IntPtr det, uint sz, out uint need, ref SGDEVINFO_DATA di);
  [DllImport("setupapi.dll", CharSet = CharSet.Unicode)] static extern bool SetupDiGetDeviceRegistryPropertyW(IntPtr h, ref SGDEVINFO_DATA di, uint prop, out uint t, IntPtr buf, uint sz, out uint need);
  [DllImport("setupapi.dll")] static extern bool SetupDiDestroyDeviceInfoList(IntPtr h);
  [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr CreateFileW(string p, uint a, uint s, IntPtr sa, uint disp, uint flags, IntPtr tpl);
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool DeviceIoControl(IntPtr h, uint ctl, IntPtr inp, uint ins, IntPtr outp, uint outs, out uint ret, IntPtr ov);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);

  static string Esc(string s) {
    if (s == null) return "";
    return s.Replace("|", "/").Replace("\r", " ").Replace("\n", " ");
  }

  // 注册表字符串属性（SPDRP_FRIENDLYNAME=0x0C / SPDRP_DEVICEDESC=0）
  static string RegProp(IntPtr hdev, ref SGDEVINFO_DATA di, uint prop) {
    try {
      uint t, need;
      if (!SetupDiGetDeviceRegistryPropertyW(hdev, ref di, prop, out t, IntPtr.Zero, 0, out need)) return "";
      if (need == 0 || need > 1024) return "";
      IntPtr buf = Marshal.AllocHGlobal((int)need);
      try {
        if (SetupDiGetDeviceRegistryPropertyW(hdev, ref di, prop, out t, buf, need, out need)) {
          string s = Marshal.PtrToStringUni(buf);
          if (s != null) return s.Trim();
        }
      } finally { Marshal.FreeHGlobal(buf); }
    } catch (Exception) { }
    return "";
  }

  /// 枚举所有带电池的设备。行: B|<名称>|<百分比或-1>|<状态位1=充电,2=接电>|<连接类型>
  public static string List() {
    var sb = new StringBuilder();
    try {
      Guid bat = new Guid(0x72631E54, 0x78A4, 0x11D0, 0xBC, 0xF7, 0x00, 0xAA, 0x00, 0xB7, 0xB3, 0x2A);
      IntPtr hdev = SetupDiGetClassDevsW(ref bat, null, IntPtr.Zero, 0x12); // DIGCF_PRESENT|DIGCF_DEVICEINTERFACE
      if (hdev == new IntPtr(-1)) return "ERR:SetupDiGetClassDevs 失败";
      try {
        uint idx = 0;
        while (true) {
          var ifd = new SGIFACE_DATA(); ifd.cb = Marshal.SizeOf(typeof(SGIFACE_DATA));
          if (!SetupDiEnumDeviceInterfaces(hdev, IntPtr.Zero, ref bat, idx, ref ifd)) break;
          idx++;
          try { EmitBattery(hdev, ref ifd, ref bat, sb); } catch (Exception) { }
        }
      } finally { SetupDiDestroyDeviceInfoList(hdev); }
      return sb.ToString();
    } catch (Exception e) { return "ERR:" + Esc(e.Message); }
  }

  static void EmitBattery(IntPtr hdev, ref SGIFACE_DATA ifd, ref Guid bat, StringBuilder sb) {
    // 1) 接口路径。cbSize 在 x64 必须写 8（sizeof 含对齐尾填充，API 校验值），
    //    但 DevicePath 本身在偏移 4（紧跟 DWORD cbSize，x64 实测）——按偏移直读绕开 marshaller
    uint need;
    var di = new SGDEVINFO_DATA(); di.cb = Marshal.SizeOf(typeof(SGDEVINFO_DATA));
    SetupDiGetDeviceInterfaceDetailW(hdev, ref ifd, IntPtr.Zero, 0, out need, ref di); // 预期失败，need 已写入
    if (need == 0 || need > 4096) return;
    IntPtr det = Marshal.AllocHGlobal((int)need);
    try {
      Marshal.WriteInt32(det, 8); // SP_DEVICE_INTERFACE_DETAIL_DATA.cbSize（x64 校验值）
      if (!SetupDiGetDeviceInterfaceDetailW(hdev, ref ifd, det, need, out need, ref di)) return;
      string path = Marshal.PtrToStringUni(new IntPtr(det.ToInt64() + 4));
      if (String.IsNullOrEmpty(path)) return;

      // 2) 连接类型（接口路径前缀判读）
      string pl = path.ToUpperInvariant();
      string conn = "其它";
      if (pl.Contains("BTHLE") || pl.Contains("BTHENUM")) conn = "蓝牙";
      else if (pl.Contains("ACPI")) conn = "内置";
      else if (pl.Contains("HID")) conn = "USB/无线";
      else if (pl.Contains("USB")) conn = "USB";

      // 3) 名称：注册表友好名 → 设备描述 → 电池栈设备名 → 兜底
      string name = RegProp(hdev, ref di, 0x0C);
      if (name.Length == 0) name = RegProp(hdev, ref di, 0);
      bool genericName = name.Length == 0;
      // 内置电池通常无友好名，直接用「内置电池」而非序列号；外设保留电池栈设备名兜底
      if (genericName) name = (conn == "内置") ? "内置电池" : "电池设备";

      // 4) 打开设备查状态（GENERIC_READ|GENERIC_WRITE，FILE_SHARE_RW，OPEN_EXISTING）
      IntPtr b = CreateFileW(path, 0xC0000000u, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
      if (b == new IntPtr(-1)) {
        sb.Append("B|").Append(Esc(name)).Append("|-1|0|").Append(conn).Append((char)10);
        return;
      }
      try {
        // 4a) QUERY_TAG：入参是 ULONG 等待超时（0 = 立即），出参 BatteryTag（0 = 电池不存在）
        IntPtr tagBuf = Marshal.AllocHGlobal(4);
        IntPtr waitIn = Marshal.AllocHGlobal(4);
        try {
          Marshal.WriteInt32(waitIn, 0);
          uint got;
          if (!DeviceIoControl(b, IOCTL_TAG, waitIn, 4, tagBuf, 4, out got, IntPtr.Zero) || got != 4) return;
          int tag = Marshal.ReadInt32(tagBuf);
          if (tag == 0) return;

          // 4b) QUERY_INFORMATION(level=0)：BATTERY_INFORMATION 36 字节（含 CycleCount），
          //     FullChargedCapacity 在偏移 16（绝对模式=mWh，相对模式=100）
          uint full = 0, designed = 0;
          IntPtr qi = Marshal.AllocHGlobal(12);   // {tag, level, atRate}
          IntPtr info = Marshal.AllocHGlobal(48);
          try {
            Marshal.WriteInt32(qi, 0, tag);
            Marshal.WriteInt32(qi, 4, LEVEL_INFO);
            Marshal.WriteInt32(qi, 8, 0);
            if (DeviceIoControl(b, IOCTL_INFO, qi, 12, info, 48, out got, IntPtr.Zero) && got >= 20) {
              full = (uint)Marshal.ReadInt32(info, 16); // FullChargedCapacity
              if (got >= 16) designed = (uint)Marshal.ReadInt32(info, 12); // DesignedCapacity（健康度分母）
            }
            // 名称兜底：外设（USB/无线/蓝牙）注册表名是泛型时，读电池栈 BatteryDeviceName
            if (genericName && conn != "内置") {
              Marshal.WriteInt32(qi, 4, LEVEL_NAME);
              IntPtr nbuf = Marshal.AllocHGlobal(512);
              try {
                if (DeviceIoControl(b, IOCTL_INFO, qi, 12, nbuf, 512, out got, IntPtr.Zero) && got >= 4) {
                  string dn = Marshal.PtrToStringUni(nbuf);
                  if (dn != null && dn.Trim().Length > 0) name = dn.Trim();
                }
              } finally { Marshal.FreeHGlobal(nbuf); }
            }
          } finally { Marshal.FreeHGlobal(qi); Marshal.FreeHGlobal(info); }

          // 4c) QUERY_STATUS：BATTERY_WAIT_STATUS 20 字节入，BATTERY_STATUS 16 字节出
          //     {PowerState(0), Capacity(4), Voltage(8), Rate(12)}
          uint powerState = 0, cap = 0;
          IntPtr ws = Marshal.AllocHGlobal(20);
          IntPtr st = Marshal.AllocHGlobal(16);
          try {
            Marshal.WriteInt32(ws, 0, tag); // 其余字段全 0（不等待、不设阈值）
            if (DeviceIoControl(b, IOCTL_STATUS, ws, 20, st, 16, out got, IntPtr.Zero) && got >= 8) {
              powerState = (uint)Marshal.ReadInt32(st, 0);
              cap = (uint)Marshal.ReadInt32(st, 4);
            }
          } finally { Marshal.FreeHGlobal(ws); Marshal.FreeHGlobal(st); }

          // 5) 百分比：绝对模式 cap*100/full；相对模式（多数 HID 外设）full=100 公式同样成立；
          //    full 读不到但 cap<=100 时视为本身就是百分比
          int percent;
          if (full > 0 && cap <= full) percent = (int)Math.Round(cap * 100.0 / full);
          else if (cap > 0 && cap <= 100) percent = (int)cap;
          else percent = -1;
          if (percent > 100) percent = 100;

          uint stateBits = 0;
          if ((powerState & ST_CHARGING) != 0) stateBits |= 1;
          if ((powerState & ST_ON_LINE) != 0) stateBits |= 2;
          // 健康度：满充容量 / 设计容量（仅当两者合理时给出，否则 -1 不给误导数字）
          int health = -1;
          if (designed > 0 && full > 0 && full <= designed + designed / 20) {
            health = (int)Math.Round(full * 100.0 / designed);
            if (health > 100) health = 100;
            if (health < 1) health = -1;
          }
          sb.Append("B|").Append(Esc(name)).Append("|").Append(percent).Append("|")
            .Append(stateBits).Append("|").Append(conn).Append("|").Append(health)
            .Append("|").Append(cap).Append("|").Append(full).Append((char)10);
        } finally { Marshal.FreeHGlobal(tagBuf); Marshal.FreeHGlobal(waitIn); }
      } finally { CloseHandle(b); }
    } finally { Marshal.FreeHGlobal(det); }
  }

}
"#;

// ---------- uid -> devName 缓存（显示器重插后由 get_displays 刷新） ----------
/// 从显示器标识里提取「厂商+型号」段。
/// uid 形如 "MONITOR\XMI27B3\{GUID}\0007"，WMI InstanceName 形如
/// "DISPLAY\XMI27B3\5&...&UID4352"，PerMonitorSettings 键名形如 "XMI27B30_0A_..."。
/// 三者前缀不同，统一取第二段对齐（用于名字匹配与 DPI 档匹配）。
fn short_id(s: &str) -> String {
    s.split('\\').nth(1).unwrap_or(s).to_string()
}

fn dev_cache() -> &'static Mutex<Option<HashMap<String, String>>> {
    static CACHE: OnceLock<Mutex<Option<HashMap<String, String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// uid（如 DISPLAY\XMI27B3\5&...&UID4352）→ 设备名（\\.\DISPLAY1）
/// 缓存未命中时重新枚举
fn resolve_dev(uid: &str) -> Result<String, String> {
    {
        let guard = dev_cache().lock().unwrap();
        if let Some(map) = guard.as_ref() {
            if let Some(dev) = map.get(uid) {
                return Ok(dev.clone());
            }
        }
    }
    // 重新枚举填充
    let raw = ps_core("[SGCore]::ListDisplays()")?;
    let mut map: HashMap<String, String> = HashMap::new();
    for line in raw.lines() {
        let p: Vec<&str> = line.split('|').collect();
        if p.len() >= 3 && p[0] == "D" {
            map.insert(p[2].to_string(), p[1].to_string());
        }
    }
    if map.is_empty() {
        return Err("未枚举到显示器".to_string());
    }
    let dev = map
        .get(uid)
        .cloned()
        .ok_or_else(|| format!("未找到显示器 {}", uid))?;
    *dev_cache().lock().unwrap() = Some(map);
    Ok(dev)
}

// ---------- 显示器列表 ----------
pub fn get_displays() -> Vec<DisplayInfo> {
    // 走编译缓存加载（见 core_asm_ref 注释：不缓存会每次都重编译整份 C#，拖滑块时界面卡死）
    let csp = match core_asm_ref() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\nAdd-Type -Path '{cs}';\n$lines = [SGCore]::ListDisplays() -split ([char]10);\n$lines | ForEach-Object {{ Write-Output $_ }};\n$devs = @();\nforeach ($l in $lines) {{ $p = $l -split '\\|'; if ($p[0] -eq 'D') {{ $devs += $p[1] }} }};\n[SGCore]::DisplayNameMap();\nGet-CimInstance -Namespace root/wmi -ClassName WmiMonitorID -ErrorAction SilentlyContinue | ForEach-Object {{\n  $nm = (($_.UserFriendlyName | Where-Object {{ [int]$_ -ne 0 }} | ForEach-Object {{ [char][int]$_ }}) -join '');\n  $inst = $_.InstanceName -replace '_\\d+$', '';\n  if ($_.Active -and $nm -and $nm.Trim()) {{ Write-Output ('N|' + $inst + '|' + $nm) }}\n}};\nforeach ($dev in $devs) {{ $pr = [SGCore]::DDCProbe($dev); $b = ''; $v = ''; if ($pr -eq '1') {{ $b = [SGCore]::DDCRead($dev, [byte]0x10); $v = [SGCore]::DDCRead($dev, [byte]0x62) }}; Write-Output ('P|' + $dev + '|' + $pr + '|' + $b + '|' + $v); Write-Output ('C|' + $dev + '|' + [SGCore]::IccGet($dev)) }};\nGet-CimInstance -Namespace root/wmi -ClassName WmiMonitorBrightness -ErrorAction SilentlyContinue | ForEach-Object {{ $sg = ($_.InstanceName -split '\\\\')[1]; if ($sg) {{ Write-Output ('W|' + $sg + '|' + $_.CurrentBrightness) }} }}",
        cs = csp
    );
    let raw = ps(&script).unwrap_or_default();
    let mut rows: Vec<(String, String, u32, u32, u32, bool, u32)> = Vec::new(); // dev,uid,w,h,hz,main,ori
    let mut dc_names: HashMap<String, String> = HashMap::new(); // DisplayConfig: dev(小写) → 友好名
    let mut names: HashMap<String, String> = HashMap::new();
    let mut ddc: HashMap<String, bool> = HashMap::new();
    let mut bri: HashMap<String, u32> = HashMap::new();
    let mut vol: HashMap<String, u32> = HashMap::new();
    let mut icc: HashMap<String, String> = HashMap::new();
    // WMI 亮度（笔记本内屏：无 DDC/CI，走 root\wmi 的 ACPI 亮度）键 = uid 第二段
    let mut wmi_bri: HashMap<String, u32> = HashMap::new();
    for line in raw.lines() {
        let p: Vec<&str> = line.split('|').collect();
        if p.is_empty() {
            continue;
        }
        match p[0] {
            "D" if p.len() >= 8 => {
                let w: u32 = p[3].trim().parse().unwrap_or(0);
                let h: u32 = p[4].trim().parse().unwrap_or(0);
                let hz: u32 = p[5].trim().parse().unwrap_or(0);
                let main = p[6].trim() == "1";
                let ori: u32 = p[7].trim().parse().unwrap_or(0);
                rows.push((p[1].to_string(), p[2].to_string(), w, h, hz, main, ori));
            }
            "N" if p.len() >= 3 => {
                names.insert(short_id(p[1]), p[2].to_string());
            }
            // M|\\.\DISPLAYn|<友好名> —— DisplayConfig 权威映射（与驱动栈状态无关）
            "M" if p.len() >= 3 => {
                dc_names.insert(p[1].trim().to_lowercase(), p[2].to_string());
            }
            "P" if p.len() >= 3 => {
                let has = p[2].trim() == "1";
                ddc.insert(p[1].to_string(), has);
                if has {
                    // P|dev|1|亮度|音量（探测时顺带读回真实值，避免 UI 滑块初值错误）
                    if let Some(v) = p.get(3).and_then(|s| s.trim().parse::<u32>().ok()) {
                        bri.insert(p[1].to_string(), v);
                    }
                    if let Some(v) = p.get(4).and_then(|s| s.trim().parse::<u32>().ok()) {
                        vol.insert(p[1].to_string(), v);
                    }
                }
            }
            // C|dev|OK:<完整 ICC 路径> —— UI 只展示文件名
            "C" if p.len() >= 3 => {
                if let Some(full) = p[2].trim().strip_prefix("OK:") {
                    if let Some(base) = full.rsplit('\\').next() {
                        if !base.is_empty() {
                            icc.insert(p[1].to_string(), base.to_string());
                        }
                    }
                }
            }
            // W|<厂商型号段>|<当前亮度> —— WMI 内屏亮度（无 DDC 的显示器的回退来源）
            "W" if p.len() >= 3 => {
                if let Ok(v) = p[2].trim().parse::<u32>() {
                    wmi_bri.insert(p[1].to_string(), v);
                }
            }
            _ => {}
        }
    }
    // 刷新 dev 缓存
    {
        let mut map: HashMap<String, String> = HashMap::new();
        for (dev, uid, _, _, _, _, _) in &rows {
            map.insert(uid.clone(), dev.clone());
        }
        if !map.is_empty() {
            *dev_cache().lock().unwrap() = Some(map);
        }
    }
    // 名字匹配（优先级从高到低）：
    //  ① DisplayConfig 权威映射（GDI 设备名 → 友好名）：与 PnP/WMI 监视器状态无关，
    //     Win11 24H2 上那两者失效时它是唯一可靠来源
    //  ② uid 第二段（厂商+型号）对齐 WMI 名字（原有逻辑）
    //  ③ 1↔1 兜底：仅当「只剩一行没名字」且「恰好还剩一个没被 ①② 用掉的名字」时才猜
    let mut matched: Vec<Option<String>> = Vec::with_capacity(rows.len());
    for (dev, uid, _, _, _, _, _) in rows.iter() {
        let mut name = dc_names
            .get(&dev.to_lowercase())
            .cloned()
            .filter(|n| !n.is_empty());
        if name.is_none() {
            let key = short_id(uid);
            if !key.is_empty() {
                name = names.get(&key).cloned().filter(|n| !n.is_empty());
            }
        }
        matched.push(name);
    }
    {
        // 兜底前必须先算清「哪些名字已经被用掉」——① DisplayConfig 与 ② uid 段两条来源都要计入。
        // 只看 ② 的用量会误配：小米已由 DisplayConfig 匹配到自己的名字，副屏没有 WMI 名字时，
        // 兜底逻辑又把同一个名字塞给副屏，于是两台屏显示成同一个名字（实测 v0.2.9 就是这个现象）。
        let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
        for m in matched.iter().flatten() {
            used.insert(m.clone());
        }
        let unmatched: Vec<usize> = matched
            .iter()
            .enumerate()
            .filter(|(_, m)| m.is_none())
            .map(|(i, _)| i)
            .collect();
        let leftover: Vec<&String> = names.values().filter(|n| !used.contains(*n)).collect();
        if unmatched.len() == 1 && leftover.len() == 1 {
            matched[unmatched[0]] = Some(leftover[0].clone());
        }
    }
    let mut result: Vec<DisplayInfo> = Vec::new();
    for (i, (dev, uid, w, h, hz, main, ori)) in rows.into_iter().enumerate() {
        let name = matched
            .get(i)
            .and_then(|m| m.clone())
            .unwrap_or_else(|| format!("显示器 {}", result.len() + 1));
        let res = if w > 0 && h > 0 {
            format!("{}x{}", w, h)
        } else {
            String::new()
        };
        let _ = ori; // 当前方向信息暂不暴露
        result.push(DisplayInfo {
            id: uid.clone(),
            name,
            resolution: res,
            hz: if hz > 0 { Some(hz) } else { None },
            main,
            connected: true,
            ddc: ddc.get(&dev).copied().unwrap_or(false),
            brightness: bri.get(&dev).copied().or_else(|| wmi_bri.get(&short_id(&uid)).copied()),
            volume: vol.get(&dev).copied(),
            color_profile: icc.get(&dev).cloned(),
            ddc_id: None,
        });
    }
    result
}

// ---------- 原生 DDC/CI 快路径（毫秒级，不启动 PowerShell） ----------
//
// 为什么要它：走 PowerShell 时一次 DDC 读写 = 启动进程(~0.4s) + 加载程序集 + I2C 通信，
// 实测单次 get_displays 要 1.9 秒。拖亮度滑块时每一格都等这么一下，手感就是一顿一顿的。
// 直接调 dxva2 的 VCP 接口只要十几毫秒。原生失败会自动回退到下面的 PowerShell 路径。
use std::ffi::c_void;

/// DDC 用到的句柄/指针统一别名
type DdcPtr = *mut c_void;

#[repr(C)]
struct DdcRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct DdcMonitorInfoExW {
    cb_size: u32,
    rc_monitor: DdcRect,
    rc_work: DdcRect,
    flags: u32,
    device: [u16; 32],
}

#[repr(C)]
struct DdcPhysicalMonitor {
    handle: DdcPtr,
    description: [u16; 128],
}

type DdcEnumProc = extern "system" fn(DdcPtr, DdcPtr, *mut DdcRect, isize) -> i32;

#[link(name = "user32")]
extern "system" {
    fn EnumDisplayMonitors(hdc: DdcPtr, clip: DdcPtr, cb: DdcEnumProc, data: isize) -> i32;
    fn GetMonitorInfoW(hmonitor: DdcPtr, mi: *mut DdcMonitorInfoExW) -> i32;
}

#[link(name = "dxva2")]
extern "system" {
    fn GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor: DdcPtr, n: *mut u32) -> i32;
    fn GetPhysicalMonitorsFromHMONITOR(
        hmonitor: DdcPtr,
        n: u32,
        arr: *mut DdcPhysicalMonitor,
    ) -> i32;
    fn SetVCPFeature(hmonitor: DdcPtr, code: u8, value: u32) -> i32;
    fn GetVCPFeatureAndVCPFeatureReply(
        hmonitor: DdcPtr,
        code: u8,
        vct: *mut i32,
        cur: *mut u32,
        max: *mut u32,
    ) -> i32;
    fn DestroyPhysicalMonitors(n: u32, arr: *mut DdcPhysicalMonitor) -> i32;
}

struct DdcFind {
    want: String,
    found: DdcPtr,
}

extern "system" fn ddc_find_cb(h: DdcPtr, _hdc: DdcPtr, _rc: *mut DdcRect, data: isize) -> i32 {
    if data == 0 {
        return 0;
    }
    let ctx = unsafe { &mut *(data as *mut DdcFind) };
    let mut mi: DdcMonitorInfoExW = unsafe { std::mem::zeroed() };
    mi.cb_size = std::mem::size_of::<DdcMonitorInfoExW>() as u32;
    if unsafe { GetMonitorInfoW(h, &mut mi) } != 0 {
        let len = mi.device.iter().position(|c| *c == 0).unwrap_or(mi.device.len());
        let dev = String::from_utf16_lossy(&mi.device[..len]);
        if dev.eq_ignore_ascii_case(&ctx.want) {
            ctx.found = h;
            return 0; // 命中，停止枚举
        }
    }
    1 // 继续找
}

/// 在指定显示器的物理句柄上执行一次操作（自动获取并释放句柄）
fn ddc_with<T>(dev: &str, f: impl FnOnce(DdcPtr) -> T) -> Option<T> {
    let mut ctx = DdcFind {
        want: dev.to_string(),
        found: std::ptr::null_mut(),
    };
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            ddc_find_cb,
            &mut ctx as *mut DdcFind as isize,
        )
    };
    if ctx.found.is_null() {
        return None;
    }
    let mut n: u32 = 0;
    if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(ctx.found, &mut n) } == 0 || n == 0 {
        return None;
    }
    let mut arr: Vec<DdcPhysicalMonitor> = Vec::with_capacity(n as usize);
    for _ in 0..n {
        arr.push(unsafe { std::mem::zeroed() });
    }
    if unsafe { GetPhysicalMonitorsFromHMONITOR(ctx.found, n, arr.as_mut_ptr()) } == 0 {
        return None;
    }
    let out = f(arr[0].handle);
    unsafe { DestroyPhysicalMonitors(n, arr.as_mut_ptr()) };
    Some(out)
}

/// 原生写 VCP（true = 成功）
fn ddc_set(dev: &str, code: u8, value: u32) -> bool {
    ddc_with(dev, |h| unsafe { SetVCPFeature(h, code, value) != 0 }).unwrap_or(false)
}

/// 原生读 VCP 当前值
fn ddc_get(dev: &str, code: u8) -> Option<u32> {
    ddc_with(dev, |h| {
        let (mut vct, mut cur, mut max) = (0i32, 0u32, 0u32);
        if unsafe { GetVCPFeatureAndVCPFeatureReply(h, code, &mut vct, &mut cur, &mut max) } != 0 {
            Some(cur)
        } else {
            None
        }
    })
    .flatten()
}

// ---------- DDC：亮度 / 音量（按显示器精确控制） ----------
fn vcp_op(display_id: &str, code: u8, value: Option<u32>) -> Result<(), String> {
    let dev = resolve_dev(display_id)?;
    // 快路径：原生写入。拖亮度/音量滑块走的就是这里，省掉每次约 0.5 秒的 PowerShell 启动开销。
    if let Some(v) = value {
        if ddc_set(&dev, code, v) {
            return Ok(());
        }
    }
    // 慢路径：原 PowerShell 实现（原生不可用时兜底，保证兼容性）
    let call = match value {
        Some(v) => format!(
            "[SGCore]::DDCWrite('{dev}',[byte]{code},[uint32]{v})",
            dev = dev,
            code = code,
            v = v
        ),
        None => format!("[SGCore]::DDCRead('{dev}',[byte]{code})", dev = dev, code = code),
    };
    let out = ps_core(&call)?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else if t == "ERR" {
        Err("该显示器不支持 DDC/CI".to_string())
    } else if t.starts_with("ERR:") {
        Err(t[4..].to_string())
    } else {
        // 读值场景：直接返回数字不适用，这里仅用于写
        Err("DDC 操作失败".to_string())
    }
}

pub fn set_brightness(display_id: &str, value: u32) -> Result<(), String> {
    let v = value.clamp(0, 100);
    if vcp_op(display_id, 0x10, Some(v)).is_ok() {
        return Ok(());
    }
    // DDC 失败的典型场景：笔记本内屏（eDP 面板根本没有 DDC/CI 通路）。
    // 回退 WMI 亮度（ACPI _BCM，内屏的标准控制方式，与系统亮度滑块同源）。
    let seg = short_id(display_id);
    if !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || "_- &{}".contains(c)) {
        let script = format!(
            "$b = Get-CimInstance -Namespace root/wmi -ClassName WmiMonitorBrightnessMethods -ErrorAction SilentlyContinue | Where-Object {{ ($_.InstanceName -split '\\\\')[1] -eq '{seg}' }} | Select-Object -First 1; if ($b) {{ Invoke-CimMethod -InputObject $b -MethodName WmiSetBrightness -Arguments @{{Timeout=0; Brightness={v}}} | Out-Null; Write-Output OK }} else {{ Write-Output 'ERR:no-wmi-brightness' }}",
            seg = seg,
            v = v
        );
        let out = ps(&script)?;
        if out.trim() == "OK" {
            return Ok(());
        }
        return Err(format!(
            "此屏不支持 DDC/CI 且 WMI 亮度不可用（{}）。外接屏请在显示器 OSD 菜单开启 DDC/CI；内屏亮度失败常见于驱动栈异常，可按 Win+Ctrl+Shift+B 重启图形驱动后重试",
            out.trim().trim_start_matches("ERR:").trim()
        ));
    }
    Err("该显示器不支持 DDC/CI".to_string())
}

pub fn set_volume(display_id: &str, value: u32) -> Result<(), String> {
    let v = value.clamp(0, 100);
    vcp_op(display_id, 0x62, Some(v))
}

// ---------- 系统音量（CoreAudio 默认输出端点） ----------
// SystemAudio 结构体定义在 platform/mod.rs（三平台共享，非 Windows 由存根实现兜底）

pub fn get_system_audio() -> Result<SystemAudio, String> {
    let out = ps_core("[SGAudio]::AudioGet()")?;
    let t = out.trim();
    if let Some(rest) = t.strip_prefix("OK|") {
        let p: Vec<&str> = rest.split('|').collect();
        if p.len() >= 5 {
            return Ok(SystemAudio {
                name: p[0].to_string(),
                volume: p[1].parse().unwrap_or(0),
                mute: p[2] == "1",
                adjustable: p[3].parse::<u32>().unwrap_or(0) & 1 != 0,
                form_factor: p[4].parse().unwrap_or(0),
            });
        }
    }
    Err(t.strip_prefix("ERR:").unwrap_or(t).to_string())
}

pub fn set_system_volume(v: u32) -> Result<(), String> {
    let out = ps_core(&format!("[SGAudio]::AudioSet([uint32]{})", v.clamp(0, 100)))?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else {
        Err(t.strip_prefix("ERR:").unwrap_or(t).to_string())
    }
}

pub fn set_system_mute(on: bool) -> Result<(), String> {
    let out = ps_core(&format!("[SGAudio]::AudioMute([uint32]{})", if on { 1 } else { 0 }))?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else {
        Err(t.strip_prefix("ERR:").unwrap_or(t).to_string())
    }
}

// ---------- 电池设备（Battery 类设备：内电池 / USB·无线 HID / 蓝牙） ----------
// BatteryInfo 结构体定义在 platform/mod.rs（三平台共享，非 Windows 由存根实现兜底）

pub fn get_batteries() -> Result<Vec<BatteryInfo>, String> {
    let out = ps_core("[SGBatt]::List()")?;
    let t = out.trim();
    if t.starts_with("ERR:") {
        return Err(t.strip_prefix("ERR:").unwrap_or(t).to_string());
    }
    let mut list = Vec::new();
    for line in out.lines() {
        let t = line.trim_end();
        // B|<名称>|<百分比或-1>|<状态位1=充电,2=接电>|<连接类型>|<健康度>|<当前容量mWh>|<满充容量mWh>
        // 备注：打开设备失败时只输出前 5 段（后三项缺省）
        let rest = match t.strip_prefix("B|") {
            Some(r) => r,
            None => continue,
        };
        let p: Vec<&str> = rest.split('|').collect();
        if p.len() < 4 {
            continue;
        }
        let percent: i32 = p[1].trim().parse().unwrap_or(-1);
        let state: u32 = p[2].trim().parse().unwrap_or(0);
        let health: i32 = if p.len() > 4 { p[4].trim().parse().unwrap_or(-1) } else { -1 };
        let cap_mwh: u32 = if p.len() > 5 { p[5].trim().parse().unwrap_or(0) } else { 0 };
        let full_mwh: u32 = if p.len() > 6 { p[6].trim().parse().unwrap_or(0) } else { 0 };
        list.push(BatteryInfo {
            name: p[0].to_string(),
            percent,
            charging: state & 1 != 0,
            on_ac: state & 2 != 0,
            conn: p[3].to_string(),
            health,
            cap_mwh,
            full_mwh,
        });
    }
    Ok(list)
}
// ---------- 色彩同步（ICC 关联） ----------
//
// 以下实现方式由本机（Win11 24H2 双屏、非管理员）实测确定：
//   ① WcsAssociateColorProfileWithDevice：设备名无论对错都返回 TRUE，关联却从不落地 → 已弃用
//   ② GetICMProfileW / SetICMProfileW（GDI 设备上下文）才是色彩引擎真正读取的路径，读取可靠
//   ③ 非管理员下 SetICMProfileW 同样返回 TRUE 却**不生效** → 必须回读校验（校验在 C# 侧 IccSet 内）
//   ④ InstallColorProfileW 非管理员可用，可把用户自备的 P3 / AdobeRGB 装进系统色彩目录
const COLOR_DIR: &str = "screenguard_icc";
const REFERENCE_COLOR_DIR: &str = "Screenguard\\icc-reference";
/// 系统色彩目录：Windows 只认这里的配置文件
const SYSTEM_COLOR_DIR: &str = r"C:\Windows\System32\spool\drivers\color";

/// 色彩空间别名表：用户目录 / 系统色彩目录里只要放着名字匹配的 .icm/.icc 就能用。
/// 这样"支持多少种色彩空间"不再被硬编码列表限制 —— 放进对应配置文件即可生效。
/// 键为规范化后的空间名（小写、去掉 - . 空格），值为候选文件名片段（同样规范化）。
const SPACE_ALIASES: &[(&str, &[&str])] = &[
    ("srgb", &["srgbcolorspaceprofile", "srgb"]),
    ("p3", &["displayp3", "displayp3", "p3"]),
    ("dcip3", &["dcip3", "p3d65", "dci"]),
    ("adobergb", &["adobergb1998", "adobergb", "adobergb1998"]),
    ("rec709", &["rec709", "bt709", "itu709"]),
    ("rec2020", &["rec2020", "bt2020", "itu2020"]),
    ("prophoto", &["prophoto"]),
    ("gray", &["gray", "grey", "grayscale"]),
];

fn flatten_key(s: &str) -> String {
    s.to_lowercase()
        .replace("-", "")
        .replace(".", "")
        .replace(" ", "")
        .replace("_", "")
}

/// 定位目标 ICC 文件。
/// 顺序：用户自备文件 → Windows 系统色彩目录 → Screenguard 生成的标准参考配置。
/// 用户自备的显示器校准配置优先；自动生成的配置只描述标准色域，不代表显示器已经校准。
fn icc_path(space: &str) -> Result<String, String> {
    let key = flatten_key(space);
    if key == "gray" {
        return Err("Windows 端灰度 ICC 不能安全地作为 RGB 显示器配置使用；请选择一种 RGB 色彩空间".to_string());
    }
    let wants: Vec<String> = SPACE_ALIASES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, a)| a.iter().map(|s| flatten_key(s)).collect())
        .unwrap_or_else(|| vec![key.clone()]);
    let local_app_data = std::env::var("LOCALAPPDATA")
        .map_err(|_| "无法读取 LOCALAPPDATA，不能定位 Windows 色彩配置目录".to_string())?;
    let user_dir = std::path::PathBuf::from(&local_app_data).join(COLOR_DIR);
    for dir in [user_dir.clone(), std::path::PathBuf::from(SYSTEM_COLOR_DIR)] {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let low = name.to_lowercase();
            let ext = low.rsplit('.').next().unwrap_or("").to_string();
            if ext != "icm" && ext != "icc" {
                continue;
            }
            let flat = flatten_key(&low);
            if wants.iter().any(|w| !w.is_empty() && flat.contains(w.as_str())) {
                return Ok(e.path().to_string_lossy().to_string());
            }
        }
    }
    let reference = crate::icc_profile::reference_profile(&key)
        .ok_or_else(|| format!("不支持的 Windows RGB 色彩空间：{}", space))?;
    let reference_dir = std::path::PathBuf::from(local_app_data).join(REFERENCE_COLOR_DIR);
    std::fs::create_dir_all(&reference_dir)
        .map_err(|e| format!("无法创建 Screenguard 色彩配置目录：{}", e))?;
    let path = reference_dir.join(reference.file_name);
    if !path.exists() {
        let temp_path = path.with_extension("icc.tmp");
        std::fs::write(&temp_path, &reference.bytes)
            .map_err(|e| format!("无法写入标准色彩参考配置：{}", e))?;
        match std::fs::rename(&temp_path, &path) {
            Ok(()) => {}
            Err(_) if path.exists() => {
                let _ = std::fs::remove_file(&temp_path);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&temp_path);
                return Err(format!("无法保存标准色彩参考配置：{}", e));
            }
        }
    }
    Ok(path.to_string_lossy().to_string())
}

/// C# 桥接源码的落盘路径（%TEMP%\screenguard_core.cs）
fn core_cs_file() -> std::path::PathBuf {
    std::env::temp_dir().join("screenguard_core.cs")
}

/// 确保 C# 桥接源码已落盘（带 UTF-8 BOM，PowerShell 5.1 才能正确解析中文注释），
/// 返回可传给 PowerShell 的正斜杠路径。
///
/// 为什么必须落盘而不是内联：整段 C# 内联进 `Add-Type -TypeDefinition '<CORE_CS>'`
/// 会撞 Windows 命令行长度上限 —— 实测 app 报「无法启动 powershell: 文件名或扩展名太长
/// (os error 206)」，导致显示器列表 / DDC / 旋转等所有依赖 SGCore 的命令集体失败。
/// 改为 Add-Type -Path 后命令行只剩一个短路径，后续再加代码也不会再撞线。
/// C# 桥接的编译产物（程序集）缓存路径
fn core_dll_file() -> std::path::PathBuf {
    std::env::temp_dir().join("screenguard_core.dll")
}

/// Quote a value as a PowerShell single-quoted string literal.
/// Windows profile paths can contain apostrophes (for example, in the user name).
fn ps_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// 确保 C# 桥接可用，返回可传给 PowerShell 的加载路径。
///
/// 落盘原因：整段 C# 内联进 `Add-Type -TypeDefinition '<CORE_CS>'` 会撞 Windows 命令行
/// 长度上限（app 报「文件名或扩展名太长 (os error 206)」，所有依赖 SGCore 的命令集体失败）。
/// 编译缓存原因：每次调用都 `Add-Type -Path '<源文件>'` 会**重新编译整份 C#**（实测 1~3 秒），
/// 拖亮度滑块时每一格都要等一次，界面直接卡死。改成先编译成程序集（仅在源码变化时做一次），
/// 之后每次只加载 DLL（约 0.2 秒）。
fn core_asm_ref() -> Result<String, String> {
    let cs = core_cs_file();
    let dll = core_dll_file();
    // 1) 源码落盘（带 UTF-8 BOM：PowerShell 5.1 没 BOM 会按 ANSI 读中文注释 → 编译失败）
    let want = CORE_CS.len() as u64 + 3; // + 3 字节 BOM
    if !matches!(std::fs::metadata(&cs), Ok(m) if m.len() == want) {
        let mut buf = Vec::with_capacity(CORE_CS.len() + 3);
        buf.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        buf.extend_from_slice(CORE_CS.as_bytes());
        std::fs::write(&cs, &buf).map_err(|e| format!("写入 C# 桥接文件失败：{}", e))?;
    }
    let cs_p = cs.display().to_string().replace('\\', "/");
    let dll_p = dll.display().to_string().replace('\\', "/");
    // 2) 程序集可用？存在、非空、且不旧于源文件
    let fresh = match (std::fs::metadata(&cs), std::fs::metadata(&dll)) {
        (Ok(c), Ok(d)) => match (c.modified(), d.modified()) {
            (Ok(cm), Ok(dm)) => dm >= cm && d.len() > 0,
            _ => false,
        },
        _ => false,
    };
    if fresh {
        return Ok(dll_p);
    }
    // 3) 编译（失败就退回源码直载：慢，但至少能用，不会因为编译问题整个功能不可用）
    let script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\nAdd-Type -Path {cs} -OutputAssembly {dll}",
        cs = ps_literal(&cs_p),
        dll = ps_literal(&dll_p)
    );
    if ps(&script).is_err() || !dll.exists() {
        return Ok(cs_p);
    }
    Ok(dll_p)
}

/// 在已加载 CORE_CS 的会话里执行一段桥接调用
fn ps_core(call: &str) -> Result<String, String> {
    let cs = core_asm_ref()?;
    let script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\nAdd-Type -Path {cs};\n{call}",
        cs = ps_literal(&cs),
        call = call
    );
    ps(&script)
}

/// 所有显示屏的设备名（\\.\DISPLAYn，即 CreateDC / EnumDisplaySettings 用的名字）
fn display_devs() -> Result<Vec<String>, String> {
    let raw = ps_core("[SGCore]::ListDisplays()")?;
    let devs: Vec<String> = raw
        .lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('|').collect();
            if p.len() >= 8 && p[0] == "D" && !p[1].is_empty() {
                Some(p[1].to_string())
            } else {
                None
            }
        })
        .collect();
    if devs.is_empty() {
        Err("未枚举到显示器".to_string())
    } else {
        Ok(devs)
    }
}

/// 读某设备当前关联的 ICC 完整路径
fn icc_get(dev: &str) -> Option<String> {
    let out = ps_core(&format!("[SGCore]::IccGet({})", ps_literal(dev))).ok()?;
    out.trim()
        .strip_prefix("OK:")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 安装 ICC 到系统色彩目录
fn icc_install(profile: &str) -> Result<(), String> {
    let out = ps_core(&format!("[SGCore]::IccInstall({})", ps_literal(profile)))?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else if let Some(e) = t.strip_prefix("ERR:") {
        Err(e.to_string())
    } else {
        Err(t.to_string())
    }
}

/// 关联 ICC（含回读校验，校验在 C# 侧完成）
fn icc_set(dev: &str, profile: &str) -> Result<(), String> {
    let out = ps_core(&format!(
        "[SGCore]::IccSet({},{})",
        ps_literal(dev),
        ps_literal(profile)
    ))?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else if let Some(e) = t.strip_prefix("ERR:") {
        Err(e.to_string())
    } else {
        Err(t.to_string())
    }
}

/// 当前进程是否已提权（whoami /groups 的完整性级别：S-1-16-12288 = 高完整性 = 管理员）
fn is_elevated() -> bool {
    run_cmd("whoami", &["/groups"])
        .map(|o| o.contains("S-1-16-12288"))
        .unwrap_or(false)
}

/// 以管理员身份重启本程序：弹 UAC，用户同意后启动带 --elevated 参数的新实例
/// （调用方随后退出当前实例）。为什么带参数：防双开守卫是「PID 小者留下」，
/// 提权新实例的 PID 几乎必然更大，若不等旧实例退场就会被守卫当场杀掉；
/// 新实例见到 --elevated 会先等旧实例退出再继续（见 lib.rs setup）。
pub fn relaunch_as_admin() -> Result<(), String> {
    if is_elevated() {
        return Err("当前已经是管理员权限运行，无需重启".to_string());
    }
    let exe = current_exe_path()?;
    let script = format!(
        "$ErrorActionPreference='Stop'; Start-Process -FilePath {} -ArgumentList '--elevated' -Verb RunAs | Out-Null; 'OK'",
        ps_literal(&exe)
    );
    // run_cmd 以 stdout 为准；UAC 被取消时 Start-Process 抛异常、stdout 为空 → Err
    match run_cmd(PS, &["-NoProfile", "-Command", &script]) {
        Ok(o) if o.trim() == "OK" => Ok(()),
        _ => Err("未获得管理员授权（UAC 被取消或被系统策略阻止），程序未重启".to_string()),
    }
}

/// 把指定 ICC 关联到所有显示屏；至少一台成功即算成功，全部失败则给出可操作的原因
fn associate_all(profile: &str) -> Result<(), String> {
    let devs = display_devs()?;
    let mut ok = 0usize;
    let mut errs: Vec<String> = Vec::new();
    for d in &devs {
        match icc_set(d, profile) {
            Ok(()) => ok += 1,
            Err(e) => errs.push(format!("{} {}", d, e)),
        }
    }
    if ok > 0 {
        return Ok(());
    }
    let hint = if is_elevated() {
        "请在「设置 → 系统 → 显示 → 高级显示 → 颜色管理」中指定配置文件".to_string()
    } else {
        "Windows 变更显示器色彩配置需要管理员权限：请右键本程序选「以管理员身份运行」".to_string()
    };
    Err(format!(
        "色彩关联未生效（{} 台设备全部失败）→ {}{}",
        devs.len(),
        hint,
        if errs.is_empty() { String::new() } else { format!("（{}）", errs.join("；")) }
    ))
}

/// Windows 端「色彩同步」：把 ICC 关联到所有显示屏。
/// 关联可随时改回、无破坏性；失败会给出真实原因，不再静默假装成功。
pub fn apply_color_space(space: &str) -> Result<(), String> {
    mark_change(); // 用户主动改的显示配置：自动修复 120 秒内不要插手
    let profile = icc_path(space)?;
    // 先安装进系统色彩目录（Windows 只认该目录下的配置）；sRGB 为系统自带，装不上可忽略
    if let Err(e) = icc_install(&profile) {
        if !profile.starts_with(SYSTEM_COLOR_DIR) {
            return Err(format!("无法安装色彩配置：{}", e));
        }
    }
    associate_all(&profile)
}

/// 「对齐 Mac 内建屏」在 Windows 没有对应物（Windows 无内建 P3 屏），
/// 因此语义改为：以主屏当前关联的 ICC 为准，把其余屏全部对齐到主屏。
pub fn match_mac() -> Result<(), String> {
    let displays = get_displays();
    if displays.len() < 2 {
        return Err("需要主屏 + 副屏各一块才能使用此功能".to_string());
    }
    let main = displays.iter().find(|d| d.main).ok_or("未找到主屏")?;
    let dev = resolve_dev(&main.id)?;
    let profile = icc_get(&dev).ok_or("读取主屏当前色彩配置失败（GetICMProfile 无返回）")?;
    associate_all(&profile)
}

/// 匹配两屏 PPI：检查主/副屏 DPI 缩放是否一致（窗口跨屏等大的前提）。
/// Windows 的缩放只能通过系统设置修改（需注销），此函数做诊断并给出明确指引，不擅自改动。
pub fn match_ppi() -> Result<(), String> {
    let displays = get_displays();
    if displays.len() < 2 {
        return Err("需要主屏 + 副屏各一块才能使用此功能".to_string());
    }
    let main = displays.iter().find(|d| d.main).ok_or("未找到主屏")?;
    let sec = displays
        .iter()
        .find(|d| !d.main)
        .ok_or("需要主屏 + 副屏各一块才能使用此功能")?;
    // 逻辑尺寸 = 物理 / 缩放；这里用注册表 PerMonitorSettings 读取每屏的 DPI 档
    let script = "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\n\
                  $vals = Get-ChildItem 'HKCU:\\Control Panel\\Desktop\\PerMonitorSettings' -ErrorAction SilentlyContinue;\n\
                  foreach ($v in $vals) { Write-Output ('S|' + $v.PSChildName + '|' + (Get-ItemProperty $v.PSPath -ErrorAction SilentlyContinue).DpiValue) }";
    let raw = ps(script).unwrap_or_default();
    let mut dpi_main: Option<u32> = None;
    let mut dpi_sec: Option<u32> = None;
    // PerMonitorSettings 键名形如 "XMI27B30_0A_07EA_47^HASH"（厂商型号开头），
    // 用 uid 的第二段(厂商+型号)做前缀匹配；同一屏可能有多条历史键，取首个命中。
    let main_key = short_id(&main.id).to_lowercase();
    let sec_key = short_id(&sec.id).to_lowercase();
    for line in raw.lines() {
        let p: Vec<&str> = line.split('|').collect();
        if p.len() >= 3 && p[0] == "S" {
            let key = p[1].to_lowercase();
            let val: u32 = p[2].trim().parse().unwrap_or(0);
            if !main_key.is_empty() && key.starts_with(&main_key) && dpi_main.is_none() {
                dpi_main = Some(val);
            }
            if !sec_key.is_empty() && key.starts_with(&sec_key) && dpi_sec.is_none() {
                dpi_sec = Some(val);
            }
        }
    }
    // PerMonitorSettings 的键名与 EDID 相关，匹配不上时用系统默认（0 代表跟随系统）
    let m = dpi_main.unwrap_or(0);
    let s = dpi_sec.unwrap_or(0);
    if m == s || (m == 0 && s == 0) {
        Ok(())
    } else {
        Err(format!(
            "主屏缩放档 {} 与副屏 {} 不一致。请在「设置 → 系统 → 屏幕 → 缩放」把两块屏设为相同百分比后重试（Windows 需注销登录生效）",
            if m == 0 { "系统默认".to_string() } else { m.to_string() },
            if s == 0 { "系统默认".to_string() } else { s.to_string() }
        ))
    }
}

// ---------- 跨屏 DPI 对齐：真正落地（写 HKCU 逐屏缩放档）+ 可回滚 ----------
// 为什么旧版"看起来没实现"：Windows 改每屏缩放只有注册表一条路，且必须注销/重登才生效，
// 没有任何运行时 API 能立刻改 —— 所以旧版只做了诊断。现在改为「先备份、再统一、可还原」。

fn dpi_backup_path() -> String {
    format!(
        "{}\\Screenguard\\dpi_backup.txt",
        std::env::var("APPDATA").unwrap_or_default()
    )
}

/// 读出逐屏缩放档：返回 (注册表键名, DpiValue) 列表
fn dpi_rows() -> Result<Vec<(String, u32)>, String> {
    let script = "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\n\
                  $vals = Get-ChildItem 'HKCU:\\Control Panel\\Desktop\\PerMonitorSettings' -ErrorAction SilentlyContinue;\n\
                  foreach ($v in $vals) { $p = Get-ItemProperty $v.PSPath -ErrorAction SilentlyContinue; Write-Output ('S|' + $v.PSChildName + '|' + $p.DpiValue) }";
    let raw = ps(script).unwrap_or_default();
    let mut rows = Vec::new();
    for line in raw.lines() {
        let p: Vec<&str> = line.split('|').collect();
        if p.len() >= 3 && p[0] == "S" && !p[1].trim().is_empty() {
            rows.push((p[1].trim().to_string(), p[2].trim().parse::<u32>().unwrap_or(0)));
        }
    }
    if rows.is_empty() {
        return Err("读不到逐屏缩放设置（PerMonitorSettings 为空）".to_string());
    }
    Ok(rows)
}

fn dpi_write(key: &str, value: u32) -> Result<(), String> {
    // 键名含 ^ 与十六进制，一律走单引号字符串；不做任何转义处理以免破坏键名
    let s = format!(
        "$p = 'HKCU:\\Control Panel\\Desktop\\PerMonitorSettings\\{}'; if (-not (Test-Path $p)) {{ New-Item -Path $p -Force | Out-Null }}; Set-ItemProperty -Path $p -Name DpiValue -Value {} -Type DWord; Write-Output OK",
        key, value
    );
    let out = ps(&s)?;
    if out.contains("OK") {
        Ok(())
    } else {
        Err(format!("写入缩放档 {} 失败", key))
    }
}

/// 把副屏（以及其它屏）的缩放档统一成主屏的值；原值备份，可一键还原。
pub fn match_dpi_apply() -> Result<String, String> {
    mark_change(); // 用户主动改的显示配置：自动修复 120 秒内不要插手
    let displays = get_displays();
    if displays.len() < 2 {
        return Err("需要主屏 + 副屏各一块才能使用此功能".to_string());
    }
    let main = displays.iter().find(|d| d.main).ok_or("未找到主屏")?;
    let mkey = short_id(&main.id).to_lowercase();
    let rows = dpi_rows()?;
    let target = rows
        .iter()
        .find(|(k, _)| !mkey.is_empty() && k.to_lowercase().starts_with(&mkey))
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            "没找到主屏对应的缩放档（两块屏可能被系统记在同一个键上）。请到「设置 → 屏幕 → 缩放」手动把两块屏设成同一百分比".to_string()
        })?;
    if target == 0 {
        return Err("主屏缩放当前是「跟随系统」，无法作为对齐目标：请先在系统设置里给主屏选一个具体百分比".to_string());
    }
    // 备份（每行 键名|原值）
    let mut buf = String::new();
    for (k, v) in &rows {
        buf.push_str(&format!("{}|{}\n", k, v));
    }
    let bp = dpi_backup_path();
    if let Some(dir) = std::path::Path::new(&bp).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&bp, &buf).map_err(|e| format!("备份缩放设置失败：{}", e))?;
    let mut changed = 0;
    for (k, v) in &rows {
        if *v == target {
            continue;
        }
        if dpi_write(k, target).is_ok() {
            changed += 1;
        }
    }
    Ok(format!(
        "已把 {} 个缩放档统一为 {}（原值已备份到 {}）。Windows 需注销或重新登录后生效；不满意可点「还原缩放」回到原样",
        changed,
        target,
        bp
    ))
}

/// 还原到 match_dpi_apply 之前备份的缩放档
pub fn match_dpi_restore() -> Result<String, String> {
    mark_change(); // 用户主动改的显示配置：自动修复 120 秒内不要插手
    let bp = dpi_backup_path();
    let txt = std::fs::read_to_string(&bp)
        .map_err(|_| "没有可还原的缩放备份（还没用「窗口跨屏等大」对齐过）".to_string())?;
    let mut n = 0;
    for line in txt.lines() {
        let p: Vec<&str> = line.split('|').collect();
        if p.len() < 2 || p[0].trim().is_empty() {
            continue;
        }
        let v: u32 = p[1].trim().parse().unwrap_or(0);
        if dpi_write(p[0].trim(), v).is_ok() {
            n += 1;
        }
    }
    Ok(format!("已把 {} 个缩放档还原为备份值（注销或重新登录后生效）", n))
}

// ---------- 副屏旋转 ----------
/// 副屏 = 第一块非主屏
fn secondary_dev() -> Result<(String, u32), String> {
    let raw = ps_core("[SGCore]::ListDisplays()")?;
    let mut main_dev: Option<String> = None;
    let mut sec: Option<(String, u32)> = None;
    for line in raw.lines() {
        let p: Vec<&str> = line.split('|').collect();
        if p.len() >= 8 && p[0] == "D" {
            let ori: u32 = p[7].trim().parse().unwrap_or(0);
            if p[6].trim() == "1" {
                main_dev = Some(p[1].to_string());
            } else if sec.is_none() && main_dev.is_some() {
                sec = Some((p[1].to_string(), ori));
            } else if sec.is_none() && main_dev.is_none() {
                sec = Some((p[1].to_string(), ori));
            }
        }
    }
    sec.ok_or_else(|| "需要主屏 + 副屏各一块才能使用此功能".to_string())
}

// ---------- 「刚主动改过显示配置」闸门 ----------
// 为什么要它：切旋转/写缩放/铺满/HDR/色彩都会产生一堆显示事件，自动修复线程看到
// 就当成「显示器掉线」去修（强制重协商），于是把我们**有意**改的状态又改回去 ——
// 用户实测：点「副屏方向」当场生效、几秒后自己变回去了，正是这个原因。
static LAST_INTENT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// 用户主动改了显示配置：自动修复 120 秒内不插手；P1-1 自动触发用 quiet_minutes 判断
pub fn mark_change() {
    if let Ok(mut g) = LAST_INTENT.lock() {
        *g = Some(std::time::Instant::now());
    }
}

/// 最近 secs 秒内是否由用户主动改过显示配置（自动修复要让路）
pub fn recently_changed(secs: u64) -> bool {
    if let Ok(g) = LAST_INTENT.lock() {
        if let Some(t) = *g {
            return t.elapsed().as_secs() < secs;
        }
    }
    false
}

/// 横竖屏切换（再点一次转回）
pub fn rotate_secondary() -> Result<(), String> {
    mark_change(); // 用户主动改的显示配置：自动修复 120 秒内不要插手
    let (dev, ori) = secondary_dev()?;
    let target = if ori == 1 || ori == 3 { 0 } else { 3 };
    let call = format!(
        "[SGCore]::Rotate('{dev}',[uint32]{target})",
        dev = dev,
        target = target
    );
    let out = ps_core(&call)?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else if t.starts_with("ERR:") {
        Err(t[4..].to_string())
    } else {
        Err(t.to_string())
    }
}

/// 恢复副屏为竖屏（安全复位）
pub fn restore_secondary() -> Result<(), String> {
    mark_change(); // 用户主动改的显示配置：自动修复 120 秒内不要插手
    let (dev, ori) = secondary_dev()?;
    if ori == 3 || ori == 1 {
        return Ok(());
    }
    let call = format!("[SGCore]::Rotate('{dev}',[uint32]3)", dev = dev);
    let out = ps_core(&call)?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else if t.starts_with("ERR:") {
        Err(t[4..].to_string())
    } else {
        Err(t.to_string())
    }
}

// ================= 屏幕电源控制（方案A：纯本地，不依赖网络） =================
// 设计要点：本机小米显示器（Redmi G Pro 27U）DDC/CI 不通，无法用 DDC 控制它；
// 因此「一次黑掉所有屏」走 Windows 系统级指令（对所有屏生效，含小米），
// 而单台精细待机/唤醒走 DDC（实测副屏 MTI 支持 VCP 0xD6）。

/// 等待键鼠输入空闲：连续 quiet_ms 内没有任何输入才返回；最多等 max_wait_ms 兜底。
/// 用途：用户点「全部待机」时鼠标往往还在动（刚点完按钮），若立刻广播关屏指令，
/// 这点抖动会把刚进入待机的屏瞬间唤醒（表现为「点了没反应 / 马上又亮」，实测踩过）。
/// 等输入真正停下之后再广播，屏才会稳定保持待机；之后晃鼠标/敲键盘即原生唤醒。
fn wait_input_idle(quiet_ms: u64, max_wait_ms: u64) {
    let start = std::time::Instant::now();
    loop {
        let t0 = last_input_tick();
        std::thread::sleep(std::time::Duration::from_millis(quiet_ms));
        let t1 = last_input_tick();
        if t0 != 0 && t1 == t0 {
            return; // quiet_ms 内无新输入 → 输入已空闲
        }
        if start.elapsed().as_millis() as u64 >= max_wait_ms {
            return; // 超时兜底：用户一直在操作时最多等 max_wait_ms 就执行
        }
    }
}

/// 系统级关闭所有显示器：一次让全部屏进入待机。
/// 这是唯一能影响到无 DDC 显示器（本机小米）的软件手段。
/// 行为约定（用户要求）：点击后两块屏幕全部待机；之后晃动鼠标 / 敲键盘即亮屏
/// （待机是显示器电源状态，任何输入事件都会被系统原生唤醒，无需软件参与）。
pub fn screen_off() -> Result<(), String> {
    // 先等输入空闲再广播（最多 2 秒）：避免点击瞬间的鼠标抖动把屏立刻唤醒
    wait_input_idle(700, 2000);
    let r = screen_off_inner();
    if r.is_ok() {
        // 记录「是**我们主动**关的屏」并同时记下此刻的输入时刻。
        // 必须这么做：待机会让系统产生「显示器掉线」事件，自动修复线程看到就会去修，
        // 而深度修复里含有 screen_off() —— 结果就是「鼠标一唤醒、屏又立刻黑下去」（用户实测）。
        *asleep_tick().lock().unwrap() = last_input_tick().max(1);
    }
    r
}

/// 是否处于「我们主动让屏幕待机」的状态。
/// 用户一有输入（鼠标/键盘）就自动判定为已唤醒并清除标记 —— 这样唤醒后不会再被关回去。
pub fn standby_state() -> bool {
    let off_at = *asleep_tick().lock().unwrap();
    if off_at == 0 {
        return false;
    }
    let li = last_input_tick();
    if li != 0 && li > off_at {
        // 用户已经动过键鼠 → 视为已唤醒，清掉标记
        *asleep_tick().lock().unwrap() = 0;
        return false;
    }
    true
}

fn screen_off_inner() -> Result<(), String> {
    let out = ps_core("[SGCore]::ScreenOff()")?;
    if out.trim() == "OK" {
        Ok(())
    } else {
        Err(format!("关闭屏幕失败：{}", out.trim()))
    }
}

/// 唤醒屏幕：合成一次 Shift 按键（真实输入事件，可解除系统级显示器待机）
pub fn screen_wake() -> Result<(), String> {
    let r = screen_wake_inner();
    // 唤醒（不论是我们发的还是用户动键鼠）→ 清掉「待机中」标记，自动修复恢复工作
    *asleep_tick().lock().unwrap() = 0;
    r
}

fn screen_wake_inner() -> Result<(), String> {
    let out = ps_core("[SGCore]::ScreenWake()")?;
    if out.trim() == "OK" {
        Ok(())
    } else {
        Err(format!("唤醒屏幕失败：{}", out.trim()))
    }
}

/// DDC 单台电源模式：1=开 2=待机 4=软关（实测唤醒后需 2~5 秒 DDC 才恢复响应）
pub fn set_display_power(display_id: &str, mode: u32) -> Result<(), String> {
    mark_change(); // 用户主动改的显示配置：自动修复 120 秒内不要插手
    vcp_op(display_id, 0xD6, Some(mode))
}

/// 读单台显示器电源模式（1=开 2=待机 4=软关 5=硬关）
pub fn get_display_power(display_id: &str) -> Result<u32, String> {
    let dev = resolve_dev(display_id)?;
    // 快路径：原生读 VCP 0xD6（毫秒级）
    if let Some(v) = ddc_get(&dev, 0xD6) {
        return Ok(v);
    }
    // 慢路径：原 PowerShell 实现兜底
    let out = ps_core(&format!("[SGCore]::DDCPowerRead('{dev}')", dev = dev))?;
    let t = out.trim();
    if t == "ERR" {
        return Err("该显示器不支持电源模式控制（VCP 0xD6）".to_string());
    }
    t.parse::<u32>()
        .map_err(|_| format!("电源模式读取异常：{}", t))
}

/// 显示器在线快照（轻量，供联动检测轮询用）：返回当前在线显示器的稳定标识列表。
/// 与 get_displays 的区别：不做 DDC 探测与名字查询，只枚举，适合 2~3 秒级轮询。
pub fn display_snapshot() -> Vec<String> {
    let raw = match ps_core("[SGCore]::ListDisplays()") {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        let p: Vec<&str> = line.split('|').collect();
        if p.len() >= 3 && p[0] == "D" {
            out.push(p[2].trim().to_string());
        }
    }
    out.sort();
    out
}

// ================= 显示器健康：KVM 切换 / EDID 重新协商异常检测与修复 =================
// 背景：本机显示链路是「电脑 --USB-C→DP/HDMI-- KVM --DP/HDMI-- 两块屏」。
// KVM 在「切换电脑 / 显示器开关 / 电脑重启」时会重新协商 EDID，协商失败表现为
// ①黑屏不恢复 ②抖动花屏 ③清晰度变差、画面发白发亮（色彩格式被降级成 YCbCr）。
// 这些是「显示配置状态错了」而不是硬件坏 —— 重置显示配置即可恢复（所以更新显卡驱动
// 有时也能好）。本模块只做两件事：把异常事件读出来（供日志与提醒），以及一键重新协商。

/// 读取最近的显示链路异常事件。每行：E|时间|等级|来源|事件ID|摘要
pub fn health_events(minutes: u32) -> String {
    let csp = match core_asm_ref() {
        Ok(p) => p,
        Err(_) => return String::new(),
    };
    let script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\nAdd-Type -Path '{cs}';\n[SGCore]::ReadDisplayEvents({m})",
        cs = csp,
        m = minutes
    );
    ps(&script).unwrap_or_default()
}

/// 每台在用屏幕的当前模式。每行：M|设备名|宽|高|刷新率|方向
pub fn mode_detail() -> String {
    let csp = match core_asm_ref() {
        Ok(p) => p,
        Err(_) => return String::new(),
    };
    let script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\nAdd-Type -Path '{cs}';\n[SGCore]::ModeDetail()",
        cs = csp
    );
    ps(&script).unwrap_or_default()
}

// ================= 结构化体检报告（P0-1） =================
// 数据来源全部复用现有通道：get_displays（枚举/DDC 探测/亮度/ICC）、
// SGCore::ModeStats（新增 C# 方法，枚举支持模式推原生分辨率与最高刷新率）、
// hdr_states（HDR 状态）、WmiMonitorID（枚举「系统认识但不在活动路径上」的显示器）。
// 不重写枚举逻辑，不新增独立 PowerShell 链路。

use super::{
    aspect_mismatch, bandwidth_ladder, gcd, looks_generic_profile, required_bandwidth_gbps,
    ApplyReport, ApplyStep, DisplayHealth, DisplayTarget, HealthCheck, HealthReport, HopKind,
    HopStatus, Profile, ProfileTargets, RepairOutcome, RepairResult, Severity, Topology,
    TopologyHop,
};

/// DisplayConfig outputTechnology 枚举值 → 人话（未知值返回 None，不猜）
fn map_output_tech(t: u32) -> Option<&'static str> {
    Some(match t {
        1 => "VGA（D-Sub）",
        2 => "S 端子",
        3 => "复合视频",
        4 => "色差分量",
        5 => "DVI",
        6 => "HDMI",
        7 => "内建面板（eDP/LVDS）",
        8 => "D-JPN",
        9 => "SDI",
        10 => "DisplayPort",
        11 => "DisplayPort（内嵌）",
        12 => "间接输出（UDDI）",
        13 => "SDTV",
        14 => "无线投屏（Miracast）",
        15 => "有线间接输出",
        _ => return None,
    })
}

/// 是否间接/中转类输出（可能经扩展坞、转接头或无线）
fn is_indirect_tech(t: u32) -> bool {
    matches!(t, 12 | 14 | 15)
}

/// EDID 16 位厂商码 → 3 字母（5bit×3，A=1..26）。字节序无权威口径，两种都试；
/// 解不出（含全零）返回 None，绝不硬凑。
fn edid_manuf_letters(raw: u32) -> Option<String> {
    fn dec(v: u32) -> Option<String> {
        let ls = [((v >> 10) & 0x1f) as u8, ((v >> 5) & 0x1f) as u8, (v & 0x1f) as u8];
        if ls.iter().all(|&b| (1..=26).contains(&b)) {
            Some(ls.iter().map(|&b| (b - 1 + b'A') as char).collect())
        } else {
            None
        }
    }
    dec(raw).or_else(|| dec(raw.swap_bytes()))
}

/// 组装单屏链路拓扑（P0-2）。数据拿不到的一律 unknown + 说明，绝不猜。
/// - topo_row: DisplayConfig T 行 (outputTechnology, edidManufactureId, edidProductCodeId, monitorDevicePath)
/// - gpus: Win32_VideoController 的 (名称, 驱动版本) 列表
fn build_topology(
    d: &DisplayInfo,
    topo_row: Option<&(u32, u32, u32, String)>,
    gpus: &[(String, String)],
) -> Topology {
    let mut hops: Vec<TopologyHop> = Vec::new();
    let mut note: Option<String> = None;

    // ① GPU：WMI Win32_VideoController
    if gpus.len() == 1 {
        hops.push(TopologyHop {
            kind: HopKind::Gpu,
            label: gpus[0].0.clone(),
            detail: Some(format!("驱动版本 {}", gpus[0].1)),
            status: HopStatus::Ok,
        });
    } else if gpus.is_empty() {
        hops.push(TopologyHop {
            kind: HopKind::Gpu,
            label: "显卡".into(),
            detail: Some("WMI 未返回显卡信息（系统未提供）".into()),
            status: HopStatus::Unknown,
        });
    } else {
        // 核显+独显：系统不逐屏报告「这块屏归哪块 GPU 推」，全部列出，不猜归属
        let list = gpus
            .iter()
            .map(|(n, v)| format!("{}（驱动 {}）", n, v))
            .collect::<Vec<_>>()
            .join("；");
        hops.push(TopologyHop {
            kind: HopKind::Gpu,
            label: format!("显卡 ×{}", gpus.len()),
            detail: Some(format!("本机有多块显卡，系统不逐屏报告归属：{}", list)),
            status: HopStatus::Unknown,
        });
    }

    // ② 接口：DisplayConfig outputTechnology
    let tech = topo_row.map(|r| r.0);
    let tech_label = tech.and_then(map_output_tech);
    let builtin = matches!(tech, Some(7) | Some(11));
    hops.push(TopologyHop {
        kind: HopKind::Port,
        label: tech_label.unwrap_or("接口").to_string(),
        detail: Some(if tech_label.is_some() {
            "连接类型来自系统 DisplayConfig API（权威报告）".to_string()
        } else {
            "系统未提供连接类型（DisplayConfig 查询无结果），无法判断接口".to_string()
        }),
        status: if tech_label.is_some() { HopStatus::Ok } else { HopStatus::Unknown },
    });

    // ③ 线缆：规格不可读；按「当前模式所需带宽 vs 协议上限」反推
    let (cw, ch) = {
        let p: Vec<&str> = d.resolution.split('x').collect();
        if p.len() == 2 {
            (p[0].trim().parse().unwrap_or(0), p[1].trim().parse().unwrap_or(0))
        } else {
            (0, 0)
        }
    };
    let cable = match (cw, ch, d.hz) {
        (w, h, Some(hz)) if w > 0 && h > 0 => {
            let req = required_bandwidth_gbps(w, h, hz);
            let calc = format!("算式：{}×{}×{}×24bit×1.08 ≈ {:.1} Gbps", w, h, hz, req);
            let (status, head) = if req <= 8.16 {
                (HopStatus::Ok, String::new())
            } else {
                // suspect 只在「超出该连接类型族的现实上限」时判：HDMI 族以 HDMI 2.0（14.4）为界、
                // DP 族以 DP 1.4（25.92）为界；中间档给不出结论，标 unknown 并给出档位阶梯
                let hdmi = matches!(tech, Some(6));
                let dp = matches!(tech, Some(10) | Some(11));
                let suspect = (hdmi && req > 14.4) || (dp && req > 25.92) || req > 77.4;
                if suspect {
                    (
                        HopStatus::Suspect,
                        "所需带宽已超出该接口常见版本的上限：若出现黑屏、闪屏或最高刷新率档缺失，优先怀疑线缆/扩展坞带宽不足。".to_string(),
                    )
                } else {
                    (
                        HopStatus::Unknown,
                        "带宽档位较高，老版本线材可能承载不了；若刷新率上不去优先查线材。".to_string(),
                    )
                }
            };
            TopologyHop {
                kind: HopKind::Cable,
                label: "线缆".into(),
                detail: Some(if req <= 8.16 {
                    format!(
                        "当前 {}x{}@{} 所需带宽约 {:.1} Gbps，低于最保守的 HDMI 1.4 上限（8.16 Gbps），常见线缆都能满足。线材规格本身系统不可读。{}",
                        w, h, hz, req, calc
                    )
                } else {
                    format!(
                        "当前 {}x{}@{} 所需带宽{}。{}{}",
                        w,
                        h,
                        hz,
                        bandwidth_ladder(req),
                        head,
                        calc
                    )
                }),
                status,
            }
        }
        _ => TopologyHop {
            kind: HopKind::Cable,
            label: "线缆".into(),
            detail: Some(
                "读不到当前分辨率或刷新率，无法估算所需带宽；线材规格本身系统不可读".to_string(),
            ),
            status: HopStatus::Unknown,
        },
    };
    hops.push(cable);

    // ④ 扩展坞/转接：Windows 无标准 API，只有间接证据
    let dev_path = topo_row.map(|r| r.3.clone()).unwrap_or_default();
    if builtin {
        hops.push(TopologyHop {
            kind: HopKind::Hub,
            label: "扩展坞/转接".into(),
            detail: Some("内建面板直连主板，不经过外接转接链路".into()),
            status: HopStatus::Ok,
        });
    } else {
        let detail = if dev_path.is_empty() {
            "系统未提供中转设备（扩展坞/转接头）信息，无法判断是否经过转接。".to_string()
        } else {
            format!("系统未提供中转设备（扩展坞/转接头）信息。设备路径：{}", dev_path)
        };
        if tech.map(is_indirect_tech).unwrap_or(false) {
            note = Some("输出为间接/无线类型（系统报告）；若信号经过扩展坞或转接头，DDC/CI 通道可能被阻断。".into());
        } else if dev_path.to_uppercase().contains("USB") {
            note = Some("设备路径含 USB 节点，信号可能经过扩展坞/转接头；DDC/CI 通道可能被阻断。".into());
        }
        hops.push(TopologyHop {
            kind: HopKind::Hub,
            label: "扩展坞/转接".into(),
            detail: Some(detail),
            status: HopStatus::Unknown,
        });
    }

    // ⑤ 显示器：EDID 厂商码 + 产品码
    let (manuf_raw, prod_raw) = topo_row.map(|r| (r.1, r.2)).unwrap_or((0, 0));
    let display_hop = match edid_manuf_letters(manuf_raw) {
        Some(l) => TopologyHop {
            kind: HopKind::Display,
            label: d.name.clone(),
            detail: Some(format!("EDID 厂商代码 {} · 产品代码 {:#06X}", l, prod_raw)),
            status: HopStatus::Ok,
        },
        None => TopologyHop {
            kind: HopKind::Display,
            label: d.name.clone(),
            detail: Some("系统未提供可解码的 EDID 厂商字段，型号信息以面板名称为准".into()),
            status: HopStatus::Unknown,
        },
    };
    hops.push(display_hop);

    Topology { hops, note }
}

/// 显示器序列号（P0-3）：返回 (友好名, 序列号)。只在用户明确选择「包含序列号」时调用。
pub fn display_serials() -> Vec<(String, String)> {
    let script = "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\nGet-CimInstance -Namespace root/wmi -ClassName WmiMonitorID -ErrorAction SilentlyContinue | ForEach-Object {\n  $nm = (($_.UserFriendlyName | Where-Object { [int]$_ -ne 0 } | ForEach-Object { [char][int]$_ }) -join '');\n  $sn = (($_.SerialNumberID | Where-Object { [int]$_ -ne 0 } | ForEach-Object { [char][int]$_ }) -join '');\n  if ($sn) { Write-Output ('SN|' + $nm + '|' + $sn) }\n}";
    ps(&script)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('|').collect();
            if p.first() == Some(&"SN") && p.len() >= 3 && !p[2].trim().is_empty() {
                Some((p[1].trim().to_string(), p[2].trim().to_string()))
            } else {
                None
            }
        })
        .collect()
}

/// 读 VCP 0x10 的 (当前值, 面板上报上限)；失败返回 None（不猜）。
/// P1-4 便携屏功耗诊断专用，只在判定为便携屏时调用（一次 PS 桥调用，较贵）。
fn vcp_cur_max(dev: &str) -> Option<(u32, u32)> {
    let out = ps_core(&format!(
        "[SGCore]::DDCReadCurMax({}, [byte]0x10)",
        ps_literal(dev)
    ))
    .ok()?;
    let s = out.trim();
    if s.is_empty() || s == "ERR" {
        return None;
    }
    let (c, m) = s.split_once('|')?;
    Some((c.trim().parse().ok()?, m.trim().parse().ok()?))
}

/// 体检报告：结构化版本的一键检测。
pub fn health_report_impl() -> Result<HealthReport, String> {
    let ts = ps("Get-Date -Format \"yyyy-MM-dd HH:mm:ss\"")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    // 注意：单独的 ps() 调用默认按系统 ANSI 解码，中文 Caption（如「专业版」）会乱码，先设 UTF-8
    let os_version = ps(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8; $o = Get-CimInstance Win32_OperatingSystem -ErrorAction SilentlyContinue; ($o.Caption + ' ' + $o.Version)",
    )
    .map(|s| s.trim().to_string())
    .unwrap_or_default();

    let ds = get_displays();

    // 一次脚本拿齐：每块屏的支持模式统计 + 未在活动路径上的显示器（WMI Active=false）
    // + 显卡清单（Win32_VideoController）+ 拓扑事实（DisplayConfig outputTechnology/EDID）
    let mut mode_stats: HashMap<String, (String, u32, Vec<(u32, u32, u32)>)> = HashMap::new();
    let mut inactive: Vec<(String, String)> = Vec::new();
    let mut gpus: Vec<(String, String)> = Vec::new();
    let mut topo_map: HashMap<String, (u32, u32, u32, String)> = HashMap::new();
    if let Ok(csp) = core_asm_ref() {
        let script = format!(
            "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\nAdd-Type -Path '{cs}';\n$lines = [SGCore]::ListDisplays() -split ([char]10);\nforeach ($l in $lines) {{ $p = $l -split '\\|'; if ($p[0] -eq 'D' -and $p.Count -ge 3) {{ Write-Output ('MS|' + $p[2] + '|' + [SGCore]::ModeStats($p[1])) }} }};\nGet-CimInstance -Namespace root/wmi -ClassName WmiMonitorID -ErrorAction SilentlyContinue | ForEach-Object {{\n  if (-not $_.Active) {{\n    $nm = (($_.UserFriendlyName | Where-Object {{ [int]$_ -ne 0 }} | ForEach-Object {{ [char][int]$_ }}) -join '');\n    Write-Output ('NA|' + $_.InstanceName + '|' + $nm)\n  }}\n}};\nGet-CimInstance Win32_VideoController -ErrorAction SilentlyContinue | ForEach-Object {{ Write-Output ('G|' + $_.Name + '|' + $_.DriverVersion) }};\n[SGCore]::TopologyInfo()",
            cs = csp
        );
        if let Ok(raw) = ps(&script) {
            for line in raw.lines() {
                let p: Vec<&str> = line.split('|').collect();
                match p[0] {
                    "MS" if p.len() >= 3 => {
                        let uid = p[1].to_string();
                        if p[2].starts_with("ERR:") || p[2].is_empty() {
                            continue;
                        }
                        // p[2] = "3840x2160|144|3840x2160@144,2560x1440@144,..."
                        let joined = p[2..].join("|");
                        let segs: Vec<&str> = joined.splitn(3, '|').collect();
                        if segs.len() < 3 {
                            continue;
                        }
                        let native = segs[0].to_string();
                        let max_hz = segs[1].trim().parse::<u32>().unwrap_or(0);
                        let modes: Vec<(u32, u32, u32)> = segs[2]
                            .split(',')
                            .filter_map(|m| {
                                let (wh, hz) = m.split_once('@')?;
                                let (w, h) = wh.split_once('x')?;
                                Some((
                                    w.trim().parse().ok()?,
                                    h.trim().parse().ok()?,
                                    hz.trim().parse().ok()?,
                                ))
                            })
                            .collect();
                        mode_stats.insert(uid, (native, max_hz, modes));
                    }
                    "NA" if p.len() >= 3 => {
                        inactive.push((p[1].to_string(), p[2].to_string()));
                    }
                    // G|显卡名|驱动版本
                    "G" if p.len() >= 3 => {
                        let name = p[1].trim().to_string();
                        if !name.is_empty() {
                            gpus.push((name, p[2].trim().to_string()));
                        }
                    }
                    // T|dev|outputTechnology|edidManufactureId|edidProductCodeId|devicePath
                    "T" if p.len() >= 6 => {
                        let dev = p[1].trim().to_lowercase();
                        let tech = p[2].trim().parse().unwrap_or(0);
                        let manuf = p[3].trim().parse().unwrap_or(0);
                        let prod = p[4].trim().parse().unwrap_or(0);
                        let path = p[5..].join("|");
                        topo_map.insert(dev, (tech, manuf, prod, path));
                    }
                    _ => {}
                }
            }
        }
    }

    // HDR 状态按名字关联（H|<名>|支持|开关）
    let hdr_map: Vec<(String, bool, bool)> = hdr_states()
        .lines()
        .filter(|l| l.starts_with("H|"))
        .filter_map(|l| {
            let p: Vec<&str> = l.split('|').collect();
            if p.len() >= 4 {
                Some((p[1].to_string(), p[2] == "1", p[3] == "1"))
            } else {
                None
            }
        })
        .collect();

    let mut displays: Vec<DisplayHealth> = Vec::new();
    // 便携屏一线通功耗诊断（P1-4）：EDID 物理尺寸表 + 多屏高刷带宽（复用 P0-2 反推）
    let edid_size_rows = edid_sizes();
    let mut portable_sections: Vec<super::PortableSection> = Vec::new();
    let multi_bandwidth: Option<(f64, Vec<String>)> = {
        // ≥2 块活动屏刷新率 > 60Hz 且总带宽超过 DP 1.2 上限（17.28 Gbps）才提示
        let mut rows: Vec<(String, f64)> = Vec::new();
        for d in ds.iter().filter(|d| d.connected && d.hz.map_or(false, |h| h > 60)) {
            let p: Vec<&str> = d.resolution.split('x').collect();
            if p.len() != 2 {
                continue;
            }
            if let (Ok(w), Ok(h), Some(hz)) = (
                p[0].trim().parse::<u32>(),
                p[1].trim().parse::<u32>(),
                d.hz,
            ) {
                let bw = super::required_bandwidth_gbps(w, h, hz);
                rows.push((format!("{}：{}x{}@{}Hz ≈ {:.1} Gbps", d.name, w, h, hz, bw), bw));
            }
        }
        let total: f64 = rows.iter().map(|(_, b)| *b).sum();
        if rows.len() >= 2 && total > 17.28 {
            Some((total, rows.into_iter().map(|(s, _)| s).collect()))
        } else {
            None
        }
    };
    for d in &ds {
        let (cur_w, cur_h) = {
            let p: Vec<&str> = d.resolution.split('x').collect();
            if p.len() == 2 {
                (p[0].trim().parse().unwrap_or(0), p[1].trim().parse().unwrap_or(0))
            } else {
                (0, 0)
            }
        };
        let (native_res, max_hz_native, _max_hz_cur, modes) =
            match mode_stats.get(&d.id) {
                Some((native, max_hz, modes)) => {
                    let (_, _, mhc) = super::infer_from_modes(modes, cur_w, cur_h);
                    (
                        if native.is_empty() { None } else { Some(native.clone()) },
                        if *max_hz > 0 { Some(*max_hz) } else { None },
                        mhc,
                        modes.clone(),
                    )
                }
                None => (None, None, None, Vec::new()),
            };
        let _ = &modes;
        let native_wh = native_res.as_deref().and_then(|s| {
            s.split_once('x')
                .map(|(w, h)| (w.trim().parse().unwrap_or(0), h.trim().parse().unwrap_or(0)))
        });

        let ddc_capable = d.ddc;
        let ddc_readable = ddc_capable && d.brightness.is_some();
        // HDR：HDR API 的名字与友好名可能不完全一致，做包含匹配
        let hdr_on = hdr_map
            .iter()
            .find(|(n, _, _)| !n.is_empty() && (d.name.contains(n.as_str()) || n.contains(&d.name)))
            .map(|(_, _, on)| *on);

        let mut checks: Vec<HealthCheck> = Vec::new();

        // ① 分辨率不是原生
        if let Some((nw, nh)) = native_wh {
            if cur_w > 0 && cur_h > 0 && (cur_w != nw || cur_h != nh) {
                let stretch = aspect_mismatch((cur_w, cur_h), (nw, nh));
                checks.push(HealthCheck {
                    id: "resolution_not_native".into(),
                    severity: Severity::Warn,
                    title: if stretch { "当前分辨率与原生不一致（画面可能被拉伸）".into() } else { "当前分辨率不是面板原生分辨率".into() },
                    detail: if stretch {
                        "当前分辨率宽高比与面板原生宽高比不同，画面会被拉伸或压缩；建议切回原生分辨率。".into()
                    } else {
                        "非原生分辨率会让画面经历一次缩放，文字发虚。建议切回原生分辨率。".into()
                    },
                    evidence: vec![
                        format!("当前 {}x{}@{}", cur_w, cur_h, d.hz.map(|v| v.to_string()).unwrap_or_else(|| "?".into())),
                        format!("原生 {}x{}（按支持模式列表面积最大者推断）", nw, nh),
                        format!("宽高比 {}:{} vs {}:{}", cur_w / gcd(cur_w, cur_h), cur_h / gcd(cur_w, cur_h), nw / gcd(nw, nh), nh / gcd(nh, nh)),
                    ],
                    fix_ids: vec!["rescan_displays".into()],
                });
            }
        }

        // ② 当前分辨率下刷新率低于支持的最高值
        let max_hz_cur = modes
            .iter()
            .filter(|(w, h, _)| *w == cur_w && *h == cur_h)
            .map(|(_, _, hz)| *hz)
            .max();
        if let (Some(cur), Some(max)) = (d.hz, max_hz_cur) {
            if max > cur {
                checks.push(HealthCheck {
                    id: "refresh_below_max".into(),
                    severity: Severity::Info,
                    title: "刷新率低于该分辨率下支持的最高值".into(),
                    detail: "常见原因是线材带宽不足（HDMI 1.4 / 非 DP 线）或系统里选了低档。确认线材支持后，在系统显示设置里把刷新率调到最高档。".into(),
                    evidence: vec![
                        format!("当前 {}x{} @ {}Hz", cur_w, cur_h, cur),
                        format!("该分辨率下支持最高 {}Hz", max),
                    ],
                    fix_ids: vec![],
                });
            }
        }

        // ③ DDC/CI 不可达
        if ddc_capable && !ddc_readable {
            checks.push(HealthCheck {
                id: "ddc_unreachable".into(),
                severity: Severity::Warn,
                title: "DDC/CI 通道探测成功但读不到数据".into(),
                detail: "读 VCP 0x10（亮度）超时。常见于 KVM/扩展坞半转发状态：能建立通道但数据回不来。尝试重新插拔线缆，或用「显示器健康」里的修复动作。".into(),
                evidence: vec!["DDCProbe = 1（探测成功）".into(), "DDCRead(0x10) = 超时/空".into()],
                fix_ids: vec!["reopen_ddc_channel".into(), "ddc_power_cycle".into()],
            });
        } else if !ddc_capable && d.brightness.is_none() {
            checks.push(HealthCheck {
                id: "ddc_unreachable".into(),
                severity: Severity::Warn,
                title: "DDC/CI 通道不可达".into(),
                detail: "该屏读不到 DDC/CI，屏幕自身亮度/音量无法软调（这是链路问题，不是软件故障）。亮度可用「总亮度（gamma）」软件层兜底。".into(),
                evidence: vec!["DDCProbe = 0（探测失败）".into()],
                fix_ids: vec!["reopen_ddc_channel".into(), "ddc_power_cycle".into()],
            });
        }
        // 内屏（无 DDC 但 WMI 亮度可用）属正常，不产生检查项

        // ④ 色彩配置缺失或通用配置
        match &d.color_profile {
            Some(p) if looks_generic_profile(p) => checks.push(HealthCheck {
                id: "color_profile_generic".into(),
                severity: Severity::Info,
                title: "色彩配置为系统通用配置".into(),
                detail: "当前使用通用 sRGB / PnP 配置，未针对这块屏校准。对色彩要求高时可用「画质 → 色彩同步」对齐到标准色域。".into(),
                evidence: vec![format!("色彩配置：{}", p)],
                fix_ids: vec![],
            }),
            None => checks.push(HealthCheck {
                id: "color_profile_generic".into(),
                severity: Severity::Info,
                title: "未读取到色彩配置".into(),
                detail: "系统没有为这块屏报告 ICC 配置文件，颜色管理可能不完整。".into(),
                evidence: vec!["ICC 读取结果为空".into()],
                fix_ids: vec![],
            }),
            _ => {}
        }

        // ⑤ HDR 开启但亮度不可调（已知冲突）
        if hdr_on == Some(true) && d.brightness.is_none() {
            checks.push(HealthCheck {
                id: "hdr_brightness_conflict".into(),
                severity: Severity::Info,
                title: "HDR 开启且亮度不可调".into(),
                detail: "HDR 模式下系统接管背光，亮度滑块经常无效或表现异常，这是已知冲突。要调亮度可先关 HDR，或使用软件层「总亮度」。".into(),
                evidence: vec!["HDR = 开".into(), "亮度读取 = 不可用".into()],
                fix_ids: vec![],
            });
        }

        // 链路拓扑（P0-2）：uid → GDI 设备名（get_displays 已刷新 dev 缓存）；失败用 id 原样
        let dev_name = {
            let cache = dev_cache().lock().unwrap();
            cache
                .as_ref()
                .and_then(|m| m.get(&d.id))
                .cloned()
                .unwrap_or_else(|| d.id.clone())
        };
        let topology = Some(build_topology(d, topo_map.get(&dev_name.to_lowercase()), &gpus));

        // ⑥ 便携屏一线通功耗诊断（P1-4）：诚实边界——只解释"为什么暗/为什么闪"并给接线
        // 建议，绝不承诺"提升亮度"。判定依据：内建面板排除 + EDID 物理尺寸 < 18 英寸，
        // 或 EDID 尺寸缺失且分辨率 ≤2K。判定不了就不产生检查项（不猜）。
        let portable_hit = topo_map
            .get(&dev_name.to_lowercase())
            .and_then(|row| {
                let builtin = matches!(row.0, 7 | 11);
                let letters = edid_manuf_letters(row.1);
                let diag = letters.and_then(|l| {
                    edid_size_rows
                        .iter()
                        .find(|(k, ch, cv)| {
                            *ch > 0 && *cv > 0 && (k.starts_with(&l) || l.starts_with(k.as_str()))
                        })
                        .map(|(_, ch, cv)| super::diagonal_inches(*ch as f64, *cv as f64))
                });
                // 兜底用原生分辨率，拿不到退当前分辨率
                super::portable_reason(builtin, diag, native_wh.or(Some((cur_w, cur_h))))
            });
        if let Some(reason) = &portable_hit {
            // VCP 上限只在便携屏上读（一次 PS 桥调用，桌面屏不额外开销）
            let bri_max = if ddc_readable {
                vcp_cur_max(&dev_name).map(|(_, m)| m)
            } else {
                None
            };
            // USB-C 一线通"能判断时"才提示：HDMI/VGA/DVI 是独立线，不算一线通；
            // DP 呈现与间接输出常见于 C 口 DP Alt Mode / 扩展坞，其余情况不猜
            let usbc_ev = match topo_map.get(&dev_name.to_lowercase()).map(|r| r.0) {
                Some(10) => Some("连接类型 = DisplayPort（USB-C 一线通通常呈现为 DP）".to_string()),
                Some(t @ (12 | 15)) => Some(format!(
                    "连接类型 = {}（USB-C 一线通/扩展坞常呈现为间接输出）",
                    map_output_tech(t).unwrap_or("间接输出")
                )),
                _ => None,
            };
            checks.extend(super::build_portable_checks(
                reason,
                d.brightness,
                bri_max,
                hdr_on,
                usbc_ev,
                multi_bandwidth.clone(),
            ));
            portable_sections.push(super::build_portable_section(
                &d.id,
                &d.name,
                reason,
                d.brightness,
                bri_max,
                hdr_on,
                multi_bandwidth.is_some(),
            ));
        }

        displays.push(DisplayHealth {
            display_id: d.id.clone(),
            name: d.name.clone(),
            native_resolution: native_res,
            current_resolution: d.resolution.clone(),
            max_hz: max_hz_native,
            current_hz: d.hz,
            ddc_capable,
            ddc_readable,
            color_profile: d.color_profile.clone(),
            hdr: hdr_on,
            connected: d.connected,
            checks,
            topology,
        });
    }

    // ⑥ 系统认识但不在活动路径上的显示器（WMI Active=false）→ 虚拟条目
    for (inst, nm) in &inactive {
        let name = if nm.trim().is_empty() { "(未命名显示器)".to_string() } else { nm.clone() };
        displays.push(DisplayHealth {
            display_id: inst.clone(),
            name,
            native_resolution: None,
            current_resolution: String::new(),
            max_hz: None,
            current_hz: None,
            ddc_capable: false,
            ddc_readable: false,
            color_profile: None,
            hdr: None,
            connected: false,
            checks: vec![HealthCheck {
                id: "display_not_active".into(),
                severity: Severity::Error,
                title: "显示器未在活动路径上".into(),
                detail: "系统枚举到了这台显示器，但它不在当前活动显示路径上（常见于掉线后未恢复、KVM 切换残留）。尝试重新插拔，或点修复动作强制重新枚举。".into(),
                evidence: vec![format!("WmiMonitorID.InstanceName = {}（Active = false）", inst)],
                fix_ids: vec!["rescan_displays".into(), "force_redetect".into()],
            }],
            topology: None,
        });
    }

    if ds.is_empty() && inactive.is_empty() {
        return Err("一台显示器都没枚举到；请检查线材、供电与 KVM 切换".into());
    }

    let mut report = HealthReport {
        generated_at: ts,
        platform: "windows".into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        os_version,
        displays,
        portable: portable_sections,
        summary: super::HealthSummary {
            errors: 0,
            warnings: 0,
            infos: 0,
            headline: String::new(),
        },
    };
    report.summarize();
    Ok(report)
}

/// 把体检报告拼成 diag_scan 兼容的文本段（【1】显示器小节），避免两套逻辑各写一遍。
/// 返回 (文本, 正常数, 警告数, 问题数)。
pub(crate) fn format_display_section(r: &HealthReport) -> (String, u32, u32, u32) {
    let mut o = String::new();
    let (mut nok, mut nwarn, mut nbad) = (0u32, 0u32, 0u32);
    for d in &r.displays {
        if !d.connected {
            nbad += 1;
            o.push_str(&format!("[问题] {} · 未在活动路径上\n", d.name));
            for c in &d.checks {
                for ev in &c.evidence {
                    o.push_str(&format!("  ↳ {}\n", ev));
                }
            }
            continue;
        }
        o.push_str(&format!(
            "  {} · {} · {}Hz · DDC/CI {} · 色彩 {}\n",
            d.name,
            d.current_resolution,
            d.current_hz.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            if d.ddc_readable { "可用" } else if d.ddc_capable { "读不到数据" } else { "不可达" },
            d.color_profile.clone().unwrap_or_else(|| "未读取到".into())
        ));
        for c in &d.checks {
            match c.severity {
                Severity::Error => {
                    nbad += 1;
                    o.push_str(&format!("[问题] {}\n", c.title));
                }
                Severity::Warn => {
                    nwarn += 1;
                    o.push_str(&format!("[警告] {}\n", c.title));
                }
                Severity::Info => {
                    nwarn += 1;
                    o.push_str(&format!("[警告] {}\n", c.title));
                }
                Severity::Ok => nok += 1,
            }
            o.push_str(&format!("  ↳ {}\n", c.detail));
            for ev in &c.evidence {
                o.push_str(&format!("    · {}\n", ev));
            }
        }
        if d.checks.is_empty() {
            nok += 1;
        }
    }
    (o, nok, nwarn, nbad)
}

/// 一键修复显示异常（分级，从轻到重）：
///   level 1 —— 对每台屏强制重新协商（先降刷新率再切回原模式，不改变任何用户设置）；
///   level 2 —— 额外做一次系统级关屏+唤醒，强制整条链路重新握手，然后再协商一轮。
/// 返回逐台结果，每行：<设备名>|<结果>
pub fn repair_display(level: u32) -> Result<String, String> {
    let csp = core_asm_ref()?;
    let renegotiate = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\nAdd-Type -Path '{cs}';\n$ds = @();\nforeach ($m in ([SGCore]::ModeDetail().Split([char]10))) {{ $p = $m.Split([char]124); if ($p[0] -eq 'M') {{ $ds += $p[1] }} }};\nforeach ($d in $ds) {{ Write-Output ('R|' + $d + '|' + [SGCore]::ForceReNegotiate($d)) }}",
        cs = csp
    );

    let mut out: Vec<String> = Vec::new();
    let first = ps(&renegotiate).unwrap_or_default();
    let mut count = 0;
    for line in first.lines() {
        let p: Vec<&str> = line.split('|').collect();
        if p.len() >= 3 && p[0] == "R" {
            out.push(format!("{}|{}", p[1], p[2..].join("|")));
            count += 1;
        }
    }
    if count == 0 {
        return Err("未枚举到可用显示器，修复未执行".to_string());
    }

    if level >= 2 {
        // 更强：系统级关屏再唤醒（强制整条链路重新握手），然后重跑一轮协商
        let _ = screen_off();
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let _ = screen_wake();
        std::thread::sleep(std::time::Duration::from_millis(1800));
        let second = ps(&renegotiate).unwrap_or_default();
        let mut n2 = 0;
        for line in second.lines() {
            let p: Vec<&str> = line.split('|').collect();
            if p.len() >= 3 && p[0] == "R" {
                out.push(format!("重协商-2 {}|{}", p[1], p[2..].join("|")));
                n2 += 1;
            }
        }
        out.push(format!(
            "深度修复|已强制链路重握手（关屏+唤醒），二次协商 {}/{} 台成功",
            n2, count
        ));
    }

    Ok(out.join("\n"))
}

// ---------- 自愈动作集（P0-4）：统一入口 repair_action_impl，如实回报、失败不装成功 ----------

/// 同一动作在同一块屏上连续失败的计数（进程内；应用重启即清零）。
/// 连续失败两次后不再机械重试，改为提示手动操作路径。
fn repair_fail_counts() -> &'static Mutex<HashMap<String, u32>> {
    static COUNTS: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fail_key(action: &str, display_id: Option<&str>) -> String {
    format!("{}|{}", action, display_id.unwrap_or("-"))
}

/// 成功时清计数，失败时 +1；达到 2 次在 message 前加手动路径提示
fn note_fail(action: &str, display_id: Option<&str>, out: &mut RepairOutcome) {
    let key = fail_key(action, display_id);
    let mut guard = repair_fail_counts().lock().unwrap();
    let n = guard.entry(key).or_insert(0);
    *n += 1;
    if *n >= 2 {
        out.message = format!(
            "已连续失败 {} 次，先停止自动尝试。建议按这个顺序手动处理：①重新插拔视频线；②换一个接口/线缆；③显示器 OSD 菜单里恢复默认设置；④重启电脑。{}",
            n, out.message
        );
    }
}

fn note_ok(action: &str, display_id: Option<&str>) {
    repair_fail_counts()
        .lock()
        .unwrap()
        .remove(&fail_key(action, display_id));
}

/// uid（DISPLAY\XMI27B3\...）→ 设备名（\\.\DISPLAYn），并取当前 (宽, 高, 刷新率)
fn dev_and_mode(uid: &str) -> Result<(String, (u32, u32, u32)), String> {
    let dev = resolve_dev(uid)?;
    let raw = ps_core("[SGCore]::ListDisplays()")?;
    for line in raw.lines() {
        let p: Vec<&str> = line.split('|').collect();
        if p.len() >= 8 && p[0] == "D" && p[2].eq_ignore_ascii_case(uid) {
            let w = p[3].trim().parse().unwrap_or(0);
            let h = p[4].trim().parse().unwrap_or(0);
            let hz = p[5].trim().parse().unwrap_or(0);
            if w == 0 || h == 0 {
                return Err(format!("{} 当前模式读不到，无法重设", uid));
            }
            return Ok((dev, (w, h, hz)));
        }
    }
    Err(format!("未找到显示器 {}（可能刚被系统移除）", uid))
}

/// 显示器清单摘要（动作前后对比用）：每台一行 "uid|宽x高@Hz|是否主屏"
fn displays_fingerprint() -> String {
    ps_core("[SGCore]::ListDisplays()")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('|').collect();
            if p.len() >= 8 && p[0] == "D" {
                Some(format!("{}|{}x{}@{}|{}", p[2], p[3], p[4], p[5], p[6]))
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// VCP 0x60 输入源常用编号：DP=15、HDMI1=17、HDMI2=18、USB-C=27（MCCS 2.0 + 常见厂商扩展）
fn input_source_code(value: &str) -> Option<(u32, &'static str)> {
    match value.trim().to_lowercase().as_str() {
        "dp" | "displayport" | "15" => Some((15, "DisplayPort")),
        "hdmi1" | "17" => Some((17, "HDMI 1")),
        "hdmi2" | "18" => Some((18, "HDMI 2")),
        "usbc" | "usb-c" | "usb_c" | "27" => Some((27, "USB-C")),
        _ => None,
    }
}

/// 修复动作统一入口（P0-4）。system 级动作忽略 display_id；屏级动作缺 display_id 直接报错。
pub fn repair_action_impl(
    action: &str,
    display_id: Option<&str>,
    value: Option<&str>,
) -> Result<RepairOutcome, String> {
    let mut out = RepairOutcome {
        action: action.to_string(),
        result: RepairResult::Failed,
        message: String::new(),
        changed: Vec::new(),
    };
    let need_display = matches!(
        action,
        "reapply_mode" | "switch_input" | "ddc_power_cycle" | "reopen_ddc_channel"
    );
    if need_display && display_id.is_none() {
        out.result = RepairResult::NotApplicable;
        out.message = "这个动作需要指定一块显示器，请在体检页对应屏的卡片里点击修复".into();
        return Ok(out);
    }

    match action {
        // ① 重新枚举：让系统把当前注册表配置再应用一遍，触发显示器重新枚举
        "rescan_displays" => {
            let before = get_displays().len();
            let r = unsafe {
                ChangeDisplaySettingsExW(std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), 0, std::ptr::null_mut())
            };
            if r != 0 {
                out.message = format!("系统拒绝重新枚举（返回码 {}），通常重试一次即可", r);
                note_fail(action, display_id, &mut out);
                return Ok(out);
            }
            std::thread::sleep(std::time::Duration::from_millis(1200));
            let after = get_displays().len();
            if after != before {
                out.changed.push(format!("显示器数量 {} → {}", before, after));
            }
            out.result = RepairResult::Applied;
            out.message = format!(
                "已触发系统重新枚举显示器，当前 {} 台在活动路径上。新接的屏没出现的话，检查线缆两端是否插紧。",
                after
            );
            note_ok(action, display_id);
        }
        // ② 强制重识别：按数据库当前拓扑重新应用并强制重新枚举模式（比 ① 更重一档）
        "force_redetect" => {
            let before = displays_fingerprint();
            let raw = ps_core("[SGCore]::Redetect()")?;
            if !raw.trim().starts_with("OK") {
                out.message = format!("系统未接受重应用请求（{}），改试「重新枚举」或重启电脑", raw.trim());
                note_fail(action, display_id, &mut out);
                return Ok(out);
            }
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let after = displays_fingerprint();
            if after != before {
                out.changed.push("显示器配置发生变化（模式/主屏已更新）".into());
            }
            out.result = RepairResult::Applied;
            out.message = "已强制系统按当前拓扑重新应用显示配置并重新枚举模式。花屏/黑屏通常在这一步恢复。".into();
            note_ok(action, display_id);
        }
        // ③ 重设当前模式：备份当前模式 → 重设同一模式 → 回读核验，不符回滚（align_apply 事务写法）
        "reapply_mode" => {
            let uid = display_id.unwrap();
            let (dev, (w, h, hz)) = match dev_and_mode(uid) {
                Ok(v) => v,
                Err(e) => {
                    out.result = RepairResult::NotApplicable;
                    out.message = e;
                    return Ok(out);
                }
            };
            let raw = ps_core(&format!(
                "[SGCore]::SetMode({}, {}, {}, {})",
                ps_literal(&dev),
                w,
                h,
                hz
            ))?;
            if !raw.trim().starts_with("OK") {
                out.message = format!("系统未接受模式重设（{}）", raw.trim());
                note_fail(action, display_id, &mut out);
                return Ok(out);
            }
            std::thread::sleep(std::time::Duration::from_millis(1000));
            // 回读核验：不符则回滚到原模式
            match dev_and_mode(uid) {
                Ok((_, cur)) if cur == (w, h, hz) => {
                    out.changed.push(format!("模式重设为 {}x{}@{}", w, h, hz));
                    out.result = RepairResult::Applied;
                    out.message = format!(
                        "已按当前模式 {}x{}@{} 强制重新应用一遍，回读核验一致。信号链路完成了一次完整握手。",
                        w, h, hz
                    );
                    note_ok(action, display_id);
                }
                Ok((_, cur)) => {
                    // 回滚：重设回核验前的模式，尽力而为
                    let _ = ps_core(&format!(
                        "[SGCore]::SetMode({}, {}, {}, {})",
                        ps_literal(&dev),
                        w,
                        h,
                        hz
                    ));
                    out.message = format!(
                        "模式重设后回读不一致（实际 {}x{}@{}），已尝试恢复。显示器可能不稳定，建议重新插拔线缆。",
                        cur.0, cur.1, cur.2
                    );
                    note_fail(action, display_id, &mut out);
                }
                Err(e) => {
                    out.message = format!("模式重设后回读失败：{}", e);
                    note_fail(action, display_id, &mut out);
                }
            }
        }
        // ④ 显卡驱动软重置：Win+Ctrl+Shift+B 官方快捷键，不改任何设置
        "reset_graphics_driver" => {
            let raw = ps_core("[SGCore]::GpuReset()")?;
            if !raw.trim().starts_with("OK") {
                out.message = "驱动重置快捷键发送失败，请手动按 Win+Ctrl+Shift+B".into();
                note_fail(action, display_id, &mut out);
                return Ok(out);
            }
            std::thread::sleep(std::time::Duration::from_millis(2500));
            out.result = RepairResult::Applied;
            out.message = "已发送显卡驱动重置（屏幕黑几秒属正常）。花屏/色彩异常/抖动应已恢复；若没有改善，改试「重设当前模式」。".into();
            note_ok(action, display_id);
        }
        // ⑤ 切换输入源：DDC VCP 0x60 写入并回读确认
        "switch_input" => {
            let uid = display_id.unwrap();
            let src = match value.and_then(input_source_code) {
                Some(v) => v,
                None => {
                    out.result = RepairResult::NotApplicable;
                    out.message = "输入源只能是 DisplayPort / HDMI 1 / HDMI 2 / USB-C 之一".into();
                    return Ok(out);
                }
            };
            let (dev, _) = match dev_and_mode(uid) {
                Ok(v) => v,
                Err(e) => {
                    out.result = RepairResult::NotApplicable;
                    out.message = e;
                    return Ok(out);
                }
            };
            let raw = ps_core(&format!(
                "[SGCore]::DDCWrite({}, [byte]0x60, {})",
                ps_literal(&dev),
                src.0
            ))?;
            if !raw.trim().starts_with("OK") {
                out.message = "这台显示器不支持通过软件切换输入源（DDC/CI 0x60 不可用），请在显示器 OSD 菜单里手动切换".into();
                note_fail(action, display_id, &mut out);
                return Ok(out);
            }
            std::thread::sleep(std::time::Duration::from_millis(800));
            // 回读确认（部分显示器切换后要几秒才生效，读不到/不一致都如实报告）
            let back = ps_core(&format!(
                "[SGCore]::DDCRead({}, [byte]0x60)",
                ps_literal(&dev)
            ))
            .unwrap_or_else(|_| "ERR".into());
            match back.trim().parse::<u32>() {
                Ok(cur) if cur == src.0 => {
                    out.changed.push(format!("输入源 → {}", src.1));
                    out.result = RepairResult::Applied;
                    out.message = format!("已切到 {} 并经显示器确认。", src.1);
                    note_ok(action, display_id);
                }
                Ok(cur) => {
                    out.message = format!(
                        "切换命令已发送，但显示器当前回报的输入源是编号 {}（目标 {}）。部分显示器切换后需要几秒生效或需要在 OSD 里确认",
                        cur, src.0
                    );
                    note_fail(action, display_id, &mut out);
                }
                Err(_) => {
                    out.message = "切换命令已发送，但读不回显示器当前输入源（可能已生效，请看画面确认）".into();
                    note_fail(action, display_id, &mut out);
                }
            }
        }
        // ⑥ 显示器软关→开（DDC 0xD6），强制面板重新握手
        "ddc_power_cycle" => {
            let uid = display_id.unwrap();
            let (dev, _) = match dev_and_mode(uid) {
                Ok(v) => v,
                Err(e) => {
                    out.result = RepairResult::NotApplicable;
                    out.message = e;
                    return Ok(out);
                }
            };
            let w1 = ps_core(&format!("[SGCore]::DDCWrite({}, [byte]0xD6, 4)", ps_literal(&dev)))?;
            if !w1.trim().starts_with("OK") {
                out.message = "这台显示器不支持软件控制电源（DDC/CI 0xD6 不可用）".into();
                note_fail(action, display_id, &mut out);
                return Ok(out);
            }
            std::thread::sleep(std::time::Duration::from_millis(3000));
            let w2 = ps_core(&format!("[SGCore]::DDCWrite({}, [byte]0xD6, 1)", ps_literal(&dev)))?;
            std::thread::sleep(std::time::Duration::from_millis(1200));
            let state = ps_core(&format!(
                "[SGCore]::DDCPowerRead({})",
                ps_literal(&dev)
            ))
            .unwrap_or_else(|_| "ERR".into());
            if w2.trim().starts_with("OK") && state.trim() == "1" {
                out.changed.push("显示器电源状态 → 开".into());
                out.result = RepairResult::Applied;
                out.message = "已让显示器软关再开，面板完成了一次重新握手。".into();
                note_ok(action, display_id);
            } else {
                out.message = "关→开已发送，但显示器没有回到开机状态（它可能不支持软件唤醒）。请按显示器物理电源键或开关一次".into();
                note_fail(action, display_id, &mut out);
            }
        }
        // ⑦ 重建 DDC 通道：清句柄缓存（TryVCP 每次调用本就重建物理显示器句柄）→ 重新探测 → 回读亮度
        "reopen_ddc_channel" => {
            let uid = display_id.unwrap();
            let (dev, _) = match dev_and_mode(uid) {
                Ok(v) => v,
                Err(e) => {
                    out.result = RepairResult::NotApplicable;
                    out.message = e;
                    return Ok(out);
                }
            };
            // 动作前状态：VCP 0x10 亮度是否可读
            let probe_before = ps_core(&format!(
                "[SGCore]::DDCProbe({})",
                ps_literal(&dev)
            ))
            .unwrap_or_else(|_| "0".into());
            // 清掉 dev 缓存，强制后续调用重新枚举并重建物理显示器句柄
            *dev_cache().lock().unwrap() = None;
            std::thread::sleep(std::time::Duration::from_millis(400));
            let probe_after = ps_core(&format!(
                "[SGCore]::DDCProbe({})",
                ps_literal(&dev)
            ))
            .unwrap_or_else(|_| "0".into());
            if probe_after.trim() == "1" {
                let bri = ps_core(&format!(
                    "[SGCore]::DDCRead({}, [byte]0x10)",
                    ps_literal(&dev)
                ))
                .unwrap_or_else(|_| "ERR".into());
                if probe_before.trim() != "1" {
                    out.changed.push("DDC/CI 通道 不可用 → 可用".into());
                }
                out.result = RepairResult::Applied;
                out.message = format!(
                    "已重建 DDC/CI 通道并回读成功（当前亮度 {}）。屏幕亮度/音量控制现在应该可用了。",
                    if bri.trim() == "ERR" { "读不到" } else { &bri.trim() }
                );
                note_ok(action, display_id);
            } else {
                out.message = "重建通道后 DDC/CI 仍不可达。这台屏可能本来就不支持 DDC/CI，或信号经过了扩展坞/转接头；亮度可先用「总亮度（gamma）」兜底".into();
                note_fail(action, display_id, &mut out);
            }
        }
        _ => {
            out.result = RepairResult::NotApplicable;
            out.message = format!("未知的修复动作 {}", action);
        }
    }
    Ok(out)
}

// ================= 方案B：ADB 联网精细控制（可选增强） =================
// 前提：显示器开启「网络 ADB 调试」（一次性设置，类似手机开 USB 调试）。
// 找不到 adb 不影响方案A；找到后即可：遥控器按键实时联动、精确控制电源/音量/输入源。

use std::os::windows::process::CommandExt;

/// 探测 adb 可执行文件：程序同目录 → 我们的下载目录 → Android SDK → PATH
pub fn adb_exe() -> Option<std::path::PathBuf> {
    let mut cands: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("platform-tools").join("adb.exe"));
            cands.push(dir.join("adb.exe"));
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let l = std::path::PathBuf::from(&local);
        cands.push(l.join("screenguard_adb").join("platform-tools").join("adb.exe"));
        cands.push(l.join("Android").join("Sdk").join("platform-tools").join("adb.exe"));
    }
    // 本机已装工具自带的 adb：ASUS GlideX（ROG 机型预装，实测 1.0.41 可用）、Android SDK、scrcpy 等
    for p in [
        r"C:\Program Files\ASUS\GlideX\adb.exe",
        r"C:\Program Files (x86)\ASUS\GlideX\adb.exe",
        r"C:\Program Files\Android\platform-tools\adb.exe",
        r"C:\platform-tools\adb.exe",
    ] {
        cands.push(std::path::PathBuf::from(p));
    }
    for c in cands.iter() {
        if c.exists() {
            return Some(c.clone());
        }
    }
    None
}

/// 执行 adb 子命令（不弹黑窗）
fn adb_cmd(exe: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(exe)
        .args(args)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .map_err(|e| format!("adb 执行失败：{}", e))?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    let e = String::from_utf8_lossy(&out.stderr).to_string();
    if out.status.success() {
        Ok(s)
    } else {
        Err(format!("{}{}", s, e).trim().to_string())
    }
}

/// ADB 能力探测：是否找到 adb、已连接哪些设备
pub fn adb_status() -> super::AdbStatus {
    match adb_exe() {
        None => super::AdbStatus {
            available: false,
            path: String::new(),
            devices: Vec::new(),
            tip: "未找到 adb：方案B 的联网精细控制需要 Android Platform Tools（点「下载 ADB」自动获取）。方案A 本地功能不受影响。".to_string(),
        },
        Some(exe) => {
            let devices: Vec<String> = adb_cmd(&exe, &["devices"])
                .map(|s| {
                    s.lines()
                        .skip(1)
                        .filter_map(|l| {
                            let t = l.trim();
                            if t.is_empty() || t.starts_with('*') {
                                return None;
                            }
                            t.split('\t').next().map(|x| x.trim().to_string())
                        })
                        .filter(|x| !x.is_empty() && !x.contains("offline"))
                        .collect()
                })
                .unwrap_or_default();
            super::AdbStatus {
                available: true,
                path: exe.display().to_string(),
                devices,
                tip: "adb 就绪".to_string(),
            }
        }
    }
}

/// 连接显示器的 ADB（addr 形如 192.168.31.216:5555）
pub fn adb_connect(addr: &str) -> Result<String, String> {
    let exe = adb_exe().ok_or("未找到 adb，请先点「下载 ADB」")?;
    adb_cmd(&exe, &["connect", addr])
}

/// 在指定设备上执行 shell 命令
pub fn adb_shell(serial: &str, cmd: &str) -> Result<String, String> {
    let exe = adb_exe().ok_or("未找到 adb，请先点「下载 ADB」")?;
    adb_cmd(&exe, &["-s", serial, "shell", cmd])
}

/// 电源键（KEYCODE_POWER = 26）：待机/唤醒
pub fn adb_power(serial: &str) -> Result<String, String> {
    adb_shell(serial, "input keyevent 26")
}

/// 音量（KEYCODE_VOLUME_UP = 24 / VOLUME_DOWN = 25）
pub fn adb_volume(serial: &str, up: bool) -> Result<String, String> {
    adb_shell(serial, if up { "input keyevent 24" } else { "input keyevent 25" })
}

/// 一键下载 Android Platform Tools 到 %LOCALAPPDATA%\screenguard_adb（零依赖：PowerShell 下载 + 解压）
pub fn adb_download() -> Result<String, String> {
    let script = r#"$ErrorActionPreference='Stop';
$dst = Join-Path $env:LOCALAPPDATA 'screenguard_adb';
New-Item -ItemType Directory -Force -Path $dst | Out-Null;
$zip = Join-Path $dst 'pt.zip';
$urls = @('https://dl.google.com/android/repository/platform-tools-latest-windows.zip','https://googledownloads.cn/android/repository/platform-tools-latest-windows.zip');
$ok = $false;
foreach ($u in $urls) { try { Invoke-WebRequest -Uri $u -OutFile $zip -TimeoutSec 300 -UseBasicParsing; $ok = $true; break } catch { } }
if (-not $ok) { Write-Output 'ERR:下载 Android Platform-Tools 失败（请检查网络后在 Screenguard 中重试）。本地显示器控制不受影响。'; exit }
Expand-Archive -Path $zip -DestinationPath $dst -Force;
Remove-Item $zip -Force;
Write-Output ('OK:' + (Join-Path $dst 'platform-tools\adb.exe'))"#;
    let out = ps(script)?;
    let t = out.trim();
    if t.starts_with("OK:") {
        Ok(t[3..].to_string())
    } else if t.starts_with("ERR:") {
        Err(t[4..].to_string())
    } else {
        Err(t.to_string())
    }
}

// ================= 软件层总亮度（gamma ramp，对所有屏生效） =================
// 为什么用 gamma 而不是 DDC：本机小米显示器经 KVM 的 DP 链路不转发 DDC/CI
// （Mac 直连可读），DDC 读不到亮度/音量；gamma ramp 由 GDI 直接作用于显卡输出，
// 不依赖链路协议，因此对两块屏（含小米）都生效。
// 安全：三通道同曲线（不改色相）、范围锁 40~160、不写注册表（重启即失效）。

/// 各屏当前总亮度：行 G|<dev>|<pct>
pub fn gamma_get() -> String {
    ps_core("[SGCore]::GammaGetAll()").unwrap_or_default()
}

/// 设置所有屏的总亮度（40~160，100 = 原始）
pub fn gamma_set(pct: u32) -> Result<String, String> {
    let p = pct.clamp(40, 160);
    let out = ps_core(&format!("[SGCore]::GammaSetAll({})", p))?;
    let ok = out.lines().filter(|l| l.starts_with("OK|")).count();
    let fail = out.lines().filter(|l| l.starts_with("FAIL|")).count();
    if ok == 0 && fail == 0 {
        return Err("没有找到可调亮度的在用显示器".to_string());
    }
    Ok(format!("{} 台已应用{}", ok, if fail > 0 { format!("，{} 台失败", fail) } else { String::new() }))
}

// ================= HDR 同步 =================

/// 各屏 HDR 状态：行 H|<dev>|<支持>|<已启用>
pub fn hdr_states() -> String {
    ps_core("[SGCore]::HDRStates()").unwrap_or_default()
}

/// 把所有支持 HDR 的屏统一设为开/关
pub fn hdr_set(on: bool) -> Result<String, String> {
    let out = ps_core(&format!("[SGCore]::HDRSetAll({})", if on { 1 } else { 0 }))?;
    let ok = out.lines().filter(|l| l.contains("|OK")).count();
    let skip = out.lines().filter(|l| l.contains("SKIP")).count();
    let err: Vec<&str> = out.lines().filter(|l| l.contains("|ERR")).collect();
    if ok == 0 && skip > 0 && err.is_empty() {
        return Err("本机没有支持 HDR 的显示器".to_string());
    }
    // 如实汇报：本机实测「设置指令发出去了，但 Apply 被系统拒绝（错误 87）」，
    // 所以**不要**先报「已切换」再说注意 —— 那就是误导。有错就先说没生效。
    let apply_err = out.contains("apply=ERR");
    let mut msg = if apply_err {
        format!(
            "⚠️ HDR 未真正生效：切换指令已发给 {} 台屏，但系统拒绝 Apply（错误 87 = 参数无效）。本机 HDR 由厂商服务（AsHDRControl）接管，公开 API 改不动 —— 请用「设置 → 系统 → 显示 → HDR」或华硕工具切换；本程序只能读取并显示当前状态",
            ok
        )
    } else {
        format!("{} 台 HDR 已切换生效", ok)
    };
    if skip > 0 {
        msg.push_str(&format!("，{} 台不支持 HDR 已跳过", skip));
    }
    if !err.is_empty() {
        msg.push_str(&format!("，{} 台失败", err.len()));
    }
    // 把逐台明细一并回传：API 返回成功 != 屏幕真的切换了（这台机上实测过），
    // 与其给一个漂亮但可能不实的结论，不如让用户看到原始结果。
    let detail: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if !detail.is_empty() {
        msg.push_str(&format!("｜明细：{}", detail));
    }
    Ok(msg)
}

// ================= 双屏铺满（任意前台窗口） =================

/// 铺满状态文件（与播放器铺满分开，互不干扰）
fn span_fg_file() -> String {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    format!(r"{}\screenguard_span_fg.json", base)
}

fn rollback_span_alignment(aligned: bool) -> String {
    if !aligned {
        return String::new();
    }
    match align_restore_modes() {
        Ok(s) => format!("；显示布局已回滚：{}", s),
        Err(e) => format!("；显示布局回滚失败：{}", e),
    }
}

/// 把当前前台窗口铺满所有屏（浏览器里的抖音全屏、播放器、任意窗口都可以）
/// mode: 0 = 铺满整块桌面包围盒；1 = 以主屏为基准（高度取主屏，避免主屏底部被裁）
pub fn span_foreground(mode: u32) -> Result<String, String> {
    mark_change(); // 用户主动改的显示配置：自动修复 120 秒内不要插手
    // mode=2：**先把两块屏的清晰度自动对齐**（竖向像素高度一致），再按包围盒铺满 ——
    // 这样两块屏都会被完整覆盖，副屏底下/中间不会留下一块没铺到的画面。
    let auto_aligned = mode == 2;
    let (mode, pre) = if auto_aligned {
        match align_apply() {
            Ok(s) => {
                std::thread::sleep(std::time::Duration::from_millis(1800));
                (0u32, format!("已先自动对齐两屏清晰度 → {}；然后", s.replace('\n', " ")))
            }
            Err(e) => return Err(format!("自动对齐失败，未执行双屏铺满：{}", e)),
        }
    } else {
        (mode, String::new())
    };
    let out = match ps_core(&format!(
        "[SGCore]::SpanForeground('{}', {}, {})",
        span_fg_file(),
        std::process::id(),
        mode
    )) {
        Ok(out) => out,
        Err(e) => return Err(format!("铺满窗口命令失败：{}{}", e, rollback_span_alignment(auto_aligned))),
    };
    let t = out.trim();
    if let Some(rest) = t.strip_prefix("OK|") {
        return Ok(span_success_message(&pre, rest));
    }
    Err(format!("{}{}", t.strip_prefix("ERR:").unwrap_or(t), rollback_span_alignment(auto_aligned)))
}

/// 还原上一次被铺满的窗口
pub fn restore_foreground() -> Result<(), String> {
    let out = ps_core(&format!("[SGCore]::RestoreForeground('{}')", span_fg_file()))?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else {
        Err(t.strip_prefix("ERR:").unwrap_or(t).to_string())
    }
}

// ================= 全局快捷键（原生 RegisterHotKey，零额外依赖） =================
// 用户要求：不能用小米遥控器，就固定一个电脑快捷键，并在软件界面里显示出来。
// 固定键位：Ctrl+Alt+L —— 切换「全部屏幕待机 / 唤醒」。
// 实现：独立线程注册（NULL hwnd → 消息投递到线程队列）+ GetMessage 消息循环。
// 判断当前该关还是该开：记录「屏幕待机」时刻，配合 GetLastInputInfo——
// 若此后没有任何输入（鼠标/键盘），说明屏还是关着的 → 本次按就是唤醒；
// 若已有输入（用户自己动鼠标唤醒了）→ 本次按就是再关掉。

const WM_HOTKEY: u32 = 0x0312;
const MOD_ALT: u32 = 0x0001;
const MOD_CONTROL: u32 = 0x0002;
const MOD_NOREPEAT: u32 = 0x4000;
const VK_L: u32 = 0x4C;

#[repr(C)]
struct SgMsg {
    hwnd: *mut std::ffi::c_void,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
    l_private: u32,
}

#[repr(C)]
struct SgLastInput {
    cb_size: u32,
    dw_time: u32,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterHotKey(hwnd: *mut std::ffi::c_void, id: i32, modifiers: u32, vk: u32) -> i32;
    fn GetMessageW(msg: *mut SgMsg, hwnd: *mut std::ffi::c_void, min: u32, max: u32) -> i32;
    fn GetLastInputInfo(pli: *mut SgLastInput) -> i32;
}

fn hotkey_status() -> &'static Mutex<(bool, String)> {
    static S: OnceLock<Mutex<(bool, String)>> = OnceLock::new();
    S.get_or_init(|| Mutex::new((false, String::new())))
}

/// 屏幕待机时刻（GetTickCount 毫秒）；0 = 当前认为屏是亮的
fn asleep_tick() -> &'static Mutex<u32> {
    static S: OnceLock<Mutex<u32>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(0))
}

fn last_input_tick() -> u32 {
    unsafe {
        let mut li = SgLastInput { cb_size: std::mem::size_of::<SgLastInput>() as u32, dw_time: 0 };
        if GetLastInputInfo(&mut li) != 0 {
            li.dw_time
        } else {
            0
        }
    }
}

/// 热键动作：切换全部屏幕待机 / 唤醒
fn hotkey_toggle() {
    // off_at != 0 → 屏处于我们主动关掉的状态 → 本次按 = 唤醒。
    // 不能再用「off_at 之后有无新输入」判断：快捷键按键本身也是输入事件，
    // WM_HOTKEY 派发到这里时 last_input_tick 必然 > off_at，旧逻辑永远判成
    // 「屏幕已亮 → 该关」，快捷键只能熄屏、永远无法唤醒（实测踩过）。
    // 「用户是否已用鼠标自行唤醒」由 standby_state()（自动修复轮询）负责清标记。
    let off_at = *asleep_tick().lock().unwrap();
    if off_at != 0 {
        let _ = screen_wake();
        *asleep_tick().lock().unwrap() = 0;
    } else {
        let _ = screen_off();
        *asleep_tick().lock().unwrap() = last_input_tick().max(1);
    }
}

/// 注册全局快捷键（幂等：已注册则直接返回成功）。成功返回 "Ctrl+Alt+L"
pub fn hotkey_start() -> Result<String, String> {
    {
        let g = hotkey_status().lock().unwrap();
        if g.0 {
            return Ok("Ctrl+Alt+L".to_string());
        }
    }
    std::thread::spawn(|| unsafe {
        let id = 0x5347; // SG：Ctrl+Alt+L 全部屏幕待机 / 唤醒
        let id2 = 0x5348; // SG2：Ctrl+Alt+R 还原被铺满的窗口（不依赖界面，防止铺满后点不到按钮）
        // SG3..5：Ctrl+Alt+1..3 应用第 1..3 个场景档（P0-5；档位不存在时静默忽略）
        let idp1 = 0x5349;
        let idp2 = 0x534A;
        let idp3 = 0x534B;
        let idk = 0x534C; // SG6：Ctrl+Alt+K 软 KVM 一键切到对端（P1-2）
        if RegisterHotKey(std::ptr::null_mut(), id, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, VK_L) == 0 {
            return; // 注册失败：状态保持 (false, "")，由调用方读到失败
        }
        // 第二个键失败不影响第一个键（例如被别的软件占用）
        RegisterHotKey(std::ptr::null_mut(), id2, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 0x52);
        RegisterHotKey(std::ptr::null_mut(), idp1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 0x31);
        RegisterHotKey(std::ptr::null_mut(), idp2, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 0x32);
        RegisterHotKey(std::ptr::null_mut(), idp3, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 0x33);
        RegisterHotKey(std::ptr::null_mut(), idk, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 0x4B);
        {
            let mut g = hotkey_status().lock().unwrap();
            g.0 = true;
            g.1 = "Ctrl+Alt+L".to_string();
        }
        let mut m: SgMsg = std::mem::zeroed();
        while GetMessageW(&mut m, std::ptr::null_mut(), 0, 0) > 0 {
            if m.message == WM_HOTKEY {
                if m.wparam == id as usize {
                    // 只有电源联动开启时快捷键才生效：联动关闭时按 Ctrl+Alt+L
                    // 不做任何事（用户要求：没开联动不该把屏幕按灭）
                    if crate::platform::power_link_on() {
                        hotkey_toggle();
                    }
                } else if m.wparam == id2 as usize {
                    // 铺满之后的窗口可能盖住界面上的「还原」按钮，
                    // 所以必须有一个不依赖界面的还原入口
                    let _ = restore_foreground();
                } else if (idp1 as usize..=idp3 as usize).contains(&m.wparam) {
                    // 场景档应用耗时数秒，放独立线程，别堵住热键消息循环
                    let idx = (m.wparam - idp1 as usize) as usize;
                    std::thread::spawn(move || crate::platform::profile_apply_by_index(idx));
                } else if m.wparam == idk as usize {
                    // 软 KVM 切换含网络往返，独立线程执行
                    std::thread::spawn(move || crate::kvm::switch_from_hotkey());
                }
            }
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(160));
    let g = hotkey_status().lock().unwrap();
    if g.0 {
        Ok(g.1.clone())
    } else {
        Err("快捷键 Ctrl+Alt+L 注册失败（可能已被其它程序占用）".to_string())
    }
}

/// 当前生效的全局快捷键（空串 = 未注册）
pub fn hotkey_label() -> String {
    hotkey_status().lock().map(|g| g.1.clone()).unwrap_or_default()
}

// ================= v0.3.3：窗口选择 / 音频端点 / 状态快照与一键恢复默认 =================
// 这部分的由来（用户逐条要求）：
//  ① 「铺满」要能**让用户选哪个窗口**，铺满后要**方便还原**（铺满自己会找不到按钮）；
//  ② 凡是**软件层面**的改动（总亮度 gamma、HDR、色彩、窗口铺满、音量）都要能
//     **一键回到默认/原始状态**，避免用户乱调之后不知道该恢复成什么值；
//  ③ **退出软件时自动还原**成原样。
// 做法：应用启动时采集一次「基线快照」存 %APPDATA%\Screenguard\state.json
// （gamma 视作 100=原始曲线；HDR/ICC/音量记录当时真实值），
// 之后「恢复默认」「退出还原」都回到这份基线。硬件层面（显示器自己的亮度/音量、KVM）不动。

fn app_state_file() -> String {
    format!(
        "{}\\Screenguard\\state.json",
        std::env::var("APPDATA").unwrap_or_default()
    )
}

fn def_true() -> bool {
    true
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
pub struct AppState {
    /// 用户选定的输出端点 id（空 = 跟随系统默认）
    #[serde(default)]
    pub audio_dev: String,
    /// 退出软件时自动还原所有软件层面改动
    #[serde(default = "def_true")]
    pub revert_on_exit: bool,
    #[serde(default)]
    pub baseline_taken: bool,
    /// "设备名|0或1"
    #[serde(default)]
    pub base_hdr: Vec<String>,
    /// "设备名|ICC 路径"
    #[serde(default)]
    pub base_icc: Vec<String>,
    #[serde(default)]
    pub base_vol_id: String,
    #[serde(default)]
    pub base_vol: u32,
    #[serde(default)]
    pub base_mute: bool,
    /// 各屏当前旋转角度（"设备|角度"），供「恢复默认」把副屏方向切回原样
    #[serde(default)]
    pub base_rot: Vec<String>,
    /// 小米屏基线音量（0 = 未记录/不可用）
    #[serde(default)]
    pub base_mitv: u32,
    /// 手动填写的屏幕物理尺寸（"厂商代码|对角线英寸"）——EDID 没上报物理尺寸时用
    #[serde(default)]
    pub screen_in: Vec<String>,
    /// 原始分辨率基线（"设备|宽x高|刷新率"），用于「还原两块屏原始清晰度」
    #[serde(default)]
    pub base_mode: Vec<String>,
    /// 原始显示器位置基线（"设备|x|y"）——对齐时会把顶边拉齐，还原时要放回去
    #[serde(default)]
    pub base_pos: Vec<String>,
    /// 小米显示器 MiTV Assistant 的地址（空 = 用默认）
    #[serde(default)]
    pub mitv_ip: String,
}

pub fn app_state() -> AppState {
    std::fs::read_to_string(app_state_file())
        .ok()
        .and_then(|s| serde_json::from_str::<AppState>(&s).ok())
        .unwrap_or_default()
}

pub fn save_app_state(st: &AppState) -> Result<(), String> {
    let p = app_state_file();
    if let Some(d) = std::path::Path::new(&p).parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let js = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    std::fs::write(&p, js).map_err(|e| format!("写入状态文件失败：{}", e))
}

pub fn set_revert_on_exit(on: bool) -> Result<(), String> {
    let mut st = app_state();
    st.revert_on_exit = on;
    save_app_state(&st)
}

pub fn app_state_json() -> String {
    let st = app_state();
    serde_json::json!({
        "revert_on_exit": st.revert_on_exit,
        "audio_dev": st.audio_dev,
        "baseline_taken": st.baseline_taken,
        "base_hdr": st.base_hdr,
        "base_vol": st.base_vol,
        "base_vol_id": st.base_vol_id,
    })
    .to_string()
}

/// Export only portable user preferences. Device baselines are intentionally omitted because
/// they describe the current machine's display/audio hardware and must never be transplanted.
pub fn user_settings_json() -> String {
    let st = app_state();
    serde_json::json!({
        "revert_on_exit": st.revert_on_exit,
        "audio_dev": st.audio_dev,
        "screen_in": st.screen_in,
        "mitv_ip": st.mitv_ip,
    }).to_string()
}

/// Import an allowlisted preference object while preserving the local hardware baseline.
pub fn import_user_settings_json(raw: &str) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| format!("设置文件中的 Windows 偏好无效：{}", e))?;
    let mut st = app_state();
    if let Some(value) = value.get("revert_on_exit") {
        st.revert_on_exit = value.as_bool().ok_or("退出还原设置必须是布尔值")?;
    }
    if let Some(value) = value.get("audio_dev") {
        let value = value.as_str().ok_or("音频设备标识必须是文本")?;
        if value.len() > 2048 { return Err("音频设备标识过长".into()); }
        let exists = audio_endpoints().map(|items| items.iter().any(|endpoint| endpoint.id == value)).unwrap_or(false);
        // An endpoint ID is machine-specific. Ignore stale/foreign values instead of
        // carrying untrusted IDs into later PowerShell calls.
        st.audio_dev = if exists { value.to_string() } else { String::new() };
    }
    if let Some(value) = value.get("mitv_ip") {
        let value = value.as_str().ok_or("显示器地址必须是文本")?;
        if value.len() > 256 { return Err("显示器地址过长".into()); }
        if !value.trim().is_empty() && value.trim().parse::<std::net::Ipv4Addr>().is_err() {
            return Err("显示器地址必须是 IPv4 地址，例如 192.168.31.50".into());
        }
        st.mitv_ip = value.to_string();
    }
    if let Some(value) = value.get("screen_in") {
        let entries = value.as_array().ok_or("屏幕尺寸列表格式无效")?;
        if entries.len() > 32 { return Err("屏幕尺寸条目过多".into()); }
        st.screen_in = entries.iter().map(|entry| {
            let text = entry.as_str().ok_or("屏幕尺寸条目必须是文本")?;
            if text.len() > 128 { return Err("屏幕尺寸条目过长".into()); }
            Ok(text.to_string())
        }).collect::<Result<Vec<_>, String>>()?;
    }
    save_app_state(&st)?;
    Ok("Windows 偏好已导入；本机恢复基线保持不变".into())
}

/// 采集基线（启动时调用一次）。已有基线不覆盖 —— 否则第二次启动会把
/// 「上次改过的状态」当成本次原始值，越还原越偏。
pub fn capture_baseline() -> Result<String, String> {
    let mut st = app_state();
    if st.baseline_taken {
        // 基线只在首启采一次（否则第二次启动会把「上次改过的状态」当原始值）。
        // 但新增字段（旋转方向 / 小米音量）在老版本装机上是空的 —— 这里做一次**补齐**：
        // 只填空缺的字段，不动已有的 HDR / ICC / 音量基线。
        let mut filled = false;
        if st.base_rot.is_empty() {
            let raw = mode_detail();
            for line in raw.lines() {
                let p: Vec<&str> = line.split('|').collect();
                if p.len() >= 6 && p[0] == "M" {
                    st.base_rot.push(format!("{}|{}", p[1], p[5].trim()));
                }
            }
            filled = true;
        }
        if st.base_mitv == 0 {
            if let Ok(v) = mitv_volume_get() {
                st.base_mitv = v;
                filled = true;
            }
        }
        if filled {
            save_app_state(&st)?;
            return Ok("基线已存在，补齐了新增字段".to_string());
        }
        return Ok("基线已存在".to_string());
    }
    st.base_hdr = hdr_states()
        .lines()
        .filter(|l| l.starts_with("H|"))
        .filter_map(|l| {
            let p: Vec<&str> = l.split('|').collect();
            if p.len() >= 4 {
                Some(format!("{}|{}", p[1], p[3]))
            } else {
                None
            }
        })
        .collect();
    st.base_icc = Vec::new();
    for d in get_displays() {
        if let Ok(dev) = resolve_dev(&d.id) {
            if let Some(path) = icc_get(&dev) {
                st.base_icc.push(format!("{}|{}", dev, path));
            }
        }
    }
    st.base_vol_id = st.audio_dev.clone();
    // 各屏旋转角度 + 小米屏音量：这两项也是「软件改动」，一并记入基线，
    // 这样「一键恢复默认」才能真的把所有软件层改动都还原（用户要求）。
    st.base_rot = Vec::new();
    {
        // 注意：Windows 侧 mode_detail() 直接返回 String（不是 Result）
        let raw = mode_detail();
        for line in raw.lines() {
            let p: Vec<&str> = line.split('|').collect();
            if p.len() >= 6 && p[0] == "M" {
                st.base_rot.push(format!("{}|{}", p[1], p[5].trim()));
            }
        }
    }
    st.base_mitv = mitv_volume_get().unwrap_or(0);
    if let Ok(eps) = audio_endpoints() {
        let pick = eps
            .iter()
            .find(|e| !st.audio_dev.is_empty() && e.id == st.audio_dev)
            .or_else(|| eps.iter().find(|e| e.adjustable))
            .or_else(|| eps.first());
        if let Some(e) = pick {
            st.base_vol_id = e.id.clone();
            st.base_vol = e.volume;
            st.base_mute = e.mute;
        }
    }
    st.baseline_taken = true;
    save_app_state(&st)?;
    Ok(format!(
        "基线已采集：HDR {} 项 / ICC {} 项 / 音量 {}%",
        st.base_hdr.len(),
        st.base_icc.len(),
        st.base_vol
    ))
}

/// 一键恢复默认 = 回到基线快照（软件层面的改动全部撤销）
pub fn restore_defaults() -> Result<String, String> {
    let st = app_state();
    let mut done: Vec<String> = Vec::new();
    let mut warn: Vec<String> = Vec::new();

    if gamma_set(100).is_ok() {
        done.push("总亮度 → 100%（原始曲线）".to_string());
    }
    if restore_foreground().is_ok() {
        done.push("已还原被铺满的窗口".to_string());
    }
    let _ = screen_wake();

    // ---- 以下都是「软件层做过的改动」，一律回到基线（用户要求：一键恢复默认要全都能还原）----

    // ① 跨屏缩放对齐（写进 HKCU PerMonitorSettings 的那个）
    match match_dpi_restore() {
        Ok(o) => done.push(format!("缩放对齐已还原（{}）", o.chars().take(28).collect::<String>())),
        Err(e) => warn.push(format!("缩放对齐: {}", e)),
    }
    // ② 各屏旋转角度（副屏横竖屏）
    for ent in st.base_rot.clone() {
        let p: Vec<&str> = ent.splitn(2, '|').collect();
        if p.len() < 2 || p[1].trim().is_empty() {
            continue;
        }
        let call = format!("[SGCore]::Rotate('{}',[uint32]{})", p[0], p[1].trim());
        match ps_core(&call) {
            Ok(o) if o.trim().starts_with("OK") => done.push(format!("屏幕方向 {} 已还原", p[0])),
            Ok(o) => warn.push(format!("屏幕方向 {}: {}", p[0], o.trim())),
            Err(e) => warn.push(format!("屏幕方向 {}: {}", p[0], e)),
        }
    }
    // ③ 小米屏音量（走它自己的系统，也是软件改动）
    if st.base_mitv > 0 {
        match mitv_volume_set(st.base_mitv) {
            Ok(v) => done.push(format!("小米音量 → {}%", v)),
            Err(e) => warn.push(format!("小米音量: {}", e)),
        }
    }
    // ④ 音频「控制的设备」回到基线那一个
    if !st.base_vol_id.is_empty() {
        if set_audio_device(&st.base_vol_id).is_ok() {
            done.push("音频控制设备已还原".to_string());
        }
    }

    for ent in st.base_hdr.clone() {
        let p: Vec<&str> = ent.splitn(2, '|').collect();
        if p.len() < 2 {
            continue;
        }
        let want = p[1].trim() == "1";
        match hdr_set_dev(p[0], want) {
            Ok(_) => done.push(format!("HDR {} → {}", p[0], if want { "开" } else { "关" })),
            Err(e) => warn.push(format!("HDR {}: {}", p[0], e)),
        }
    }
    for ent in st.base_icc.clone() {
        let p: Vec<&str> = ent.splitn(2, '|').collect();
        if p.len() < 2 {
            continue;
        }
        match ps_core(&format!(
            "[SGCore]::IccSetPath({}, {})",
            ps_literal(p[0]),
            ps_literal(p[1])
        )) {
            Ok(o) if o.trim().starts_with("OK") => {
                done.push(format!("色彩配置 {} 已还原", p[0]));
            }
            Ok(o) => warn.push(format!("色彩 {}: {}", p[0], o.trim())),
            Err(e) => warn.push(format!("色彩 {}: {}", p[0], e)),
        }
    }
    if st.base_vol > 0 || !st.base_vol_id.is_empty() {
        if audio_set_on(&st.base_vol_id, st.base_vol).is_ok() {
            done.push(format!("音量 → {}%", st.base_vol));
        }
    }

    let mut msg = format!("已恢复默认：{}", done.join("；"));
    if !warn.is_empty() {
        msg.push_str(&format!("。未完成：{}", warn.join("；")));
    }
    Ok(msg)
}

// ---------- 窗口列表 / 指定窗口铺满 ----------

/// 可被铺满的窗口列表（"hwnd|宽x高|标题"）
pub fn list_windows() -> Result<String, String> {
    let out = ps_core(&format!("[SGCore]::ListWindows({})", std::process::id()))?;
    let t = out.trim();
    if t.starts_with("ERR:") {
        Err(t[4..].to_string())
    } else {
        Ok(t.to_string())
    }
}

fn span_success_message(prefix: &str, details: &str) -> String {
    match details.split_once('|') {
        Some((title, bounds)) => format!("{}{}（{}）", prefix, title, bounds),
        None => format!("{}{}", prefix, details),
    }
}

/// 铺满指定窗口（hwnd）；mode 含义同 span_foreground
pub fn span_window(hwnd: i64, mode: u32) -> Result<String, String> {
    mark_change(); // 用户主动改的显示配置：自动修复 120 秒内不要插手
    // 同 span_foreground：mode=2 先把两块屏清晰度对齐，再铺满，保证两块屏都不留没覆盖的地方
    let auto_aligned = mode == 2;
    let (mode, pre) = if auto_aligned {
        match align_apply() {
            Ok(s) => {
                std::thread::sleep(std::time::Duration::from_millis(1800));
                (0u32, format!("已先自动对齐两屏清晰度 → {}；然后", s.replace('\n', " ")))
            }
            Err(e) => return Err(format!("自动对齐失败，未执行双屏铺满：{}", e)),
        }
    } else {
        (mode, String::new())
    };
    let out = match ps_core(&format!(
        "[SGCore]::SpanWindow([int64]{}, '{}', {}, {})",
        hwnd,
        span_fg_file(),
        std::process::id(),
        mode
    )) {
        Ok(out) => out,
        Err(e) => return Err(format!("铺满窗口命令失败：{}{}", e, rollback_span_alignment(auto_aligned))),
    };
    let t = out.trim();
    if let Some(rest) = t.strip_prefix("OK|") {
        Ok(span_success_message(&pre, rest))
    } else if let Some(rest) = t.strip_prefix("ERR:") {
        Err(format!("{}{}", rest, rollback_span_alignment(auto_aligned)))
    } else {
        Err(format!("{}{}", t, rollback_span_alignment(auto_aligned)))
    }
}

// ---------- 音频输出端点 ----------

/// 列出所有活动的输出端点（Windows：CoreAudio）
pub fn audio_endpoints() -> Result<Vec<AudioEndpoint>, String> {
    let out = ps_core("[SGAudio]::AudioList()")?;
    let sel = app_state().audio_dev;
    let mut list = Vec::new();
    for line in out.lines() {
        let t = line.trim_end();
        let rest = match t.strip_prefix("A|") {
            Some(r) => r,
            None => continue,
        };
        let p: Vec<&str> = rest.split('|').collect();
        if p.len() < 6 {
            continue;
        }
        let id = p[0].trim().to_string();
        list.push(AudioEndpoint {
            selected: !sel.is_empty() && sel == id,
            id,
            name: p[1].trim().to_string(),
            volume: p[2].trim().parse().unwrap_or(0),
            mute: p[3].trim() == "1",
            adjustable: p[4].trim() == "1",
            form_factor: p[5].trim().parse().unwrap_or(0),
            is_default: p.len() > 6 && p[6].trim() == "1",
        });
    }
    if list.is_empty() {
        return Err(out.trim().to_string());
    }
    Ok(list)
}

fn audio_set_on(id: &str, v: u32) -> Result<(), String> {
    let out = ps_core(&format!(
        "[SGAudio]::AudioSetDev('{}', [uint32]{})",
        id.replace('\'', ""),
        v.min(100)
    ))?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else {
        Err(t.trim_start_matches("ERR:").to_string())
    }
}

/// 选定要控制的输出端点（空串 = 跟随系统默认）
pub fn set_audio_device(id: &str) -> Result<(), String> {
    let mut st = app_state();
    st.audio_dev = id.to_string();
    save_app_state(&st)
}

/// 把系统默认输出设备切到指定端点（软 KVM 用；与「选定控制端点」不同，这是真切换）
pub fn audio_set_default(id: &str) -> Result<(), String> {
    let out = ps_core(&format!(
        "[SGAudio]::AudioSetDefault({})",
        ps_literal(id)
    ))?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else {
        Err(t.trim_start_matches("ERR:").to_string())
    }
}

/// 读当前剪贴板文本（软 KVM 用；非文本/为空返回 None）
pub fn clipboard_get_text() -> Option<String> {
    let out = ps_core("[SGCore]::ClipboardGet()").ok()?;
    let t = out.trim_end_matches(['\r', '\n']);
    if t.is_empty() || t.starts_with("ERR:") || t == "NONE" {
        None
    } else {
        Some(t.to_string())
    }
}

/// 写剪贴板文本
pub fn clipboard_set_text(text: &str) -> Result<(), String> {
    let out = ps_core(&format!(
        "[SGCore]::ClipboardSet({})",
        ps_literal(text)
    ))?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else {
        Err(t.trim_start_matches("ERR:").to_string())
    }
}

/// 本机设备名（软 KVM 发现/配对展示用）
pub fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Windows 主机".to_string())
}

/// 设置音量：优先作用于「用户选定的端点」，没选就作用于系统默认端点
pub fn set_system_volume_sel(v: u32) -> Result<(), String> {
    let id = app_state().audio_dev;
    audio_set_on(&id, v)
}

/// 静音：同样遵循用户选定的端点
pub fn set_system_mute_sel(on: bool) -> Result<(), String> {
    let id = app_state().audio_dev;
    let out = ps_core(&format!(
        "[SGAudio]::AudioMuteDev('{}', [uint32]{})",
        id.replace('\'', ""),
        if on { 1 } else { 0 }
    ))?;
    let t = out.trim();
    if t == "OK" {
        Ok(())
    } else {
        Err(t.trim_start_matches("ERR:").to_string())
    }
}

/// 单台屏 HDR 开关（基线还原用）
pub fn hdr_set_dev(dev: &str, on: bool) -> Result<String, String> {
    mark_change(); // 用户主动改的显示配置：自动修复 120 秒内不要插手
    let out = ps_core(&format!(
        "[SGCore]::HDRSetDev('{}', {})",
        dev.replace('\'', ""),
        if on { 1 } else { 0 }
    ))?;
    let t = out.trim();
    if t.starts_with("OK") || t.starts_with("SKIP") {
        Ok(t.to_string())
    } else {
        Err(t.trim_start_matches("ERR:").to_string())
    }
}

// ---------- 小米显示器（REDMI G Pro 27U）：MiTV Assistant 音量 ----------
// 背景：小米这块屏不支持 DDC/CI（实测换过 Type-C 口、也换过 Intel/NVIDIA 分支，都不通），
// 所以它的**亮度**只能走软件层 gamma（见「总亮度」）。但它的**音量另有一条路**：
// 显示器本体是 Android(hyperOS)，6095 端口跑着 MiTV Assistant，实测：
//   GET /controller?action=getvolume                    -> {"data":{"volume":10,...}}
//   GET /controller?action=keyevent&keycode=volumeup    -> 每次 +1（实测 10→11）
//   GET /controller?action=keyevent&keycode=volumedown  -> 每次 -1（实测 11→10）
// 没有 setvolume（404），只能按键步进 → 用「读-步进-回读」逼近目标值。
// 只动音量键，不动电源/其它键；每次步进后回读校验，异常立即停止。

const MITV_IP_DEFAULT: &str = "192.168.31.216";

fn mitv_ip() -> String {
    let ip = app_state().mitv_ip;
    if ip.trim().is_empty() {
        MITV_IP_DEFAULT.to_string()
    } else {
        ip.trim().to_string()
    }
}

fn mitv_get(action: &str) -> Result<String, String> {
    let url = format!("http://{}:6095/controller?action={}", mitv_ip(), action);
    let script = format!(
        "$ProgressPreference='SilentlyContinue';try{{(Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -Uri '{}').Content}}catch{{'ERR:'+$_.Exception.Message}}",
        url
    );
    let out = ps(&script)?;
    let t = out.trim();
    if t.is_empty() {
        return Err("显示器无响应（确认显示器已联网且与电脑同网段）".to_string());
    }
    if let Some(rest) = t.strip_prefix("ERR:") {
        return Err(format!(
            "连接显示器失败：{}",
            rest.chars().take(90).collect::<String>()
        ));
    }
    Ok(t.to_string())
}

fn mitv_volume_parse(body: &str) -> Option<u32> {
    let key = "\"volume\"";
    let i = body.find(key)? + key.len();
    let rest = &body[i..];
    let c = rest.find(':')? + 1;
    let digits: String = rest[c..]
        .chars()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<u32>().ok()
}

/// 读小米屏当前音量（0-100）
pub fn mitv_volume_get() -> Result<u32, String> {
    let body = mitv_get("getvolume")?;
    mitv_volume_parse(&body).ok_or_else(|| {
        format!(
            "音量解析失败：{}",
            body.chars().take(80).collect::<String>()
        )
    })
}

/// 小米音量的「目标值」：前端每次拖动只更新这个值，由追赶循环读最新值决定方向。
/// 这样「拖回 20%」会立刻反向降，不会先把旧目标（30%）走完再降 —— 用户实测反馈的痛点。
static MITV_TARGET: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
static MITV_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 把小米屏音量调到 target；本函数会朝**最新目标**逐步逼近后返回最终值。
/// 并发调用时立即返回当前值（不阻塞界面），方向由最后一次设定的目标决定。
pub fn mitv_volume_set(target: u32) -> Result<u32, String> {
    use std::sync::atomic::Ordering;
    MITV_TARGET.store(target.min(100) as i32, Ordering::SeqCst);
    if MITV_BUSY.swap(true, Ordering::SeqCst) {
        return mitv_volume_get();
    }
    let r = (|| -> Result<u32, String> {
        let mut now = mitv_volume_get()?;
        let mut guard = 0;
        loop {
            let t = MITV_TARGET.load(Ordering::SeqCst);
            if t < 0 || t == now as i32 || guard > 150 {
                break;
            }
            let up = t > now as i32;
            // 步进间隔取 220ms：之前用 70ms 高频连打，实测把显示器的 MiTV 服务打到
            // 不应答（ping 通但 6095 端口超时）——1% 一步是固件限制，别把它逼到限流。
            // 失败时等 2 秒重试一次（限流通常是短时的），再失败才向上报错。
            if mitv_get(if up {
                "keyevent&keycode=volumeup"
            } else {
                "keyevent&keycode=volumedown"
            })
            .is_err()
            {
                std::thread::sleep(std::time::Duration::from_millis(2000));
                mitv_get(if up {
                    "keyevent&keycode=volumeup"
                } else {
                    "keyevent&keycode=volumedown"
                })?;
            }
            std::thread::sleep(std::time::Duration::from_millis(220));
            let nv = match mitv_volume_get() {
                Ok(v) => v,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                    match mitv_volume_get() {
                        Ok(v) => v,
                        Err(e) => return Err(e),
                    }
                }
            };
            if nv == now {
                break; // 到顶/到底或没生效
            }
            now = nv;
            guard += 1;
        }
        Ok(now)
    })();
    MITV_BUSY.store(false, Ordering::SeqCst);
    r
}

/// 主屏工作区（已排除任务栏）：(left, top, right, bottom)，物理像素
pub fn primary_work_area() -> Result<(i32, i32, i32, i32), String> {
    let out = ps_core("[SGAudio]::PrimaryWorkArea()")?;
    let t = out.trim();
    let body = t.strip_prefix("WA|").unwrap_or(t);
    let p: Vec<&str> = body.split('|').collect();
    if p.len() >= 4 {
        let n = |i: usize| p[i].trim().parse::<i32>().unwrap_or(0);
        let (l, tp, r, b) = (n(0), n(1), n(2), n(3));
        if r > l && b > tp {
            return Ok((l, tp, r, b));
        }
    }
    Err(format!("工作区解析失败：{}", t.chars().take(60).collect::<String>()))
}

/// 小米屏音量接口是否可用（界面据此决定是否显示这一项）
pub fn mitv_available() -> bool {
    mitv_volume_get().is_ok()
}

// ---------- 一键检测（诊断 + 日志） ----------
// 目的：点一下就把「所有与屏幕有关的信息」扫一遍，逐项判定（正常/警告/问题）并给出处置建议，
// 同时把结果**写进日志**，方便事后回溯（尤其是那种偶发的掉线、颜色不对之类）。

/// 日志目录：%APPDATA%\Screenguard\logs
fn diag_log_dir() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(std::env::var("APPDATA").unwrap_or_default());
    p.push("Screenguard");
    p.push("logs");
    let _ = std::fs::create_dir_all(&p);
    p
}

/// 日志目录路径（界面显示用）
pub fn diag_log_path() -> String {
    diag_log_dir().to_string_lossy().to_string()
}

/// 在文件资源管理器里打开日志目录
pub fn open_log_dir() -> Result<(), String> {
    let d = diag_log_path();
    std::process::Command::new("explorer")
        .arg(&d)
        .spawn()
        .map_err(|e| format!("打开日志目录失败：{}", e))?;
    Ok(())
}

#[allow(dead_code)] // 体检文本段暂不区分横竖屏，保留备用于后续视图
fn portrait_of(res: &str) -> String {
    let p: Vec<&str> = res.split('x').collect();
    if p.len() == 2 {
        if let (Ok(w), Ok(h)) = (p[0].trim().parse::<u32>(), p[1].trim().parse::<u32>()) {
            return if h > w { "竖屏".to_string() } else { "横屏".to_string() };
        }
    }
    "方向未知".to_string()
}
// 体检文本段（format_display_section）改为携带完整分辨率后，此函数暂无调用方；保留备用于后续视图

/// 一键检测：扫全部屏幕相关状态 → 逐项判定 + 建议 → 写日志 → 返回报告文本
pub fn diag_scan() -> Result<String, String> {
    let ts = ps("Get-Date -Format \"yyyy-MM-dd HH:mm:ss\"")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let (mut nok, mut nwarn, mut nbad) = (0u32, 0u32, 0u32);
    let mut o = String::new();
    o.push_str("屏幕守护 · 一键检测报告\n");
    o.push_str(&format!("时间：{}\n", ts));
    o.push_str("标记：[正常] 没问题 / [警告] 能用但要留意 / [问题] 需要处理\n");

    // 【1】显示器 —— 内部改为调用结构化体检（P0-1），检查逻辑只有一份
    o.push_str("\n【1】显示器\n");
    match health_report_impl() {
        Ok(r) => {
            let (sec, ok, warn, bad) = format_display_section(&r);
            nok += ok;
            nwarn += warn;
            nbad += bad;
            o.push_str(&sec);
        }
        Err(e) => {
            nbad += 1;
            o.push_str(&format!(
                "[问题] 显示器体检失败：{}\n  ↳ 检查线材、供电、KVM 是否切到本机；然后点「显示器健康 → 一键修复」\n",
                e
            ));
        }
    }

    // 【2】显示链路（近 24 小时）
    o.push_str("\n【2】显示链路（近 24 小时）\n");
    let ev = health_events(1440);
    let evn = ev.lines().filter(|l| !l.trim().is_empty()).count();
    if evn == 0 {
        nok += 1;
        o.push_str("[正常] 没有显示器掉线记录\n");
    } else {
        nwarn += 1;
        o.push_str(&format!(
            "[警告] 有 {} 条掉线相关记录\n  ↳ 常见于 KVM 切换/线材/供电；可用「显示器健康 → 一键修复」强制重协商\n",
            evn
        ));
    }

    // 【3】HDR
    o.push_str("\n【3】HDR\n");
    let hs = hdr_states();
    let mut sup = 0;
    for l in hs.lines().filter(|l| l.starts_with("H|")) {
        let p: Vec<&str> = l.split('|').collect();
        if p.len() >= 4 {
            let s = p[2] == "1";
            if s {
                sup += 1;
            }
            o.push_str(&format!(
                "  {} · {} · 当前 {}\n",
                p[1],
                if s { "支持 HDR" } else { "不支持 HDR" },
                if p[3] == "1" { "开" } else { "关" }
            ));
        }
    }
    if sup == 0 {
        nwarn += 1;
        o.push_str("[警告] 没有检测到支持 HDR 的屏\n");
    } else {
        nok += 1;
        o.push_str("[正常] HDR 状态已读取\n");
    }
    if let Ok(g) = ps("$p=Get-Process AsHDRControl -ErrorAction SilentlyContinue; if($p){\"YES\"}else{\"NO\"}")
    {
        if g.trim() == "YES" {
            nwarn += 1;
            o.push_str("[警告] 厂商 HDR 服务 AsHDRControl 正在运行\n  ↳ HDR 很可能被它接管：程序内切换的 API 会返回成功、但状态不变（本机已实测），要改 HDR 请用「设置 → 系统 → 显示 → HDR」或厂商工具\n");
        }
    }

    // 【4】总亮度（gamma）
    o.push_str("\n【4】总亮度（软件层 gamma）\n");
    let g = gamma_get();
    let mut vals: Vec<String> = Vec::new();
    for l in g.lines().filter(|l| l.starts_with("G|")) {
        let p: Vec<&str> = l.split('|').collect();
        if p.len() >= 3 {
            vals.push(format!("{} = {}%", p[1], p[2]));
        }
    }
    if vals.is_empty() {
        nwarn += 1;
        o.push_str("[警告] 总亮度读取失败（gamma 接口无响应）\n");
    } else {
        o.push_str(&format!("  {}\n", vals.join("　")));
        if vals.iter().all(|v| v.ends_with("= 100%")) {
            nok += 1;
            o.push_str("[正常] 所有屏都在原始曲线（100%）\n");
        } else {
            nwarn += 1;
            o.push_str("[警告] 有屏被软件调过亮度（不是原始曲线）\n  ↳ 点「亮度复位」或「一键恢复默认」可回到原样\n");
        }
    }

    // 【5】音频
    o.push_str("\n【5】音频输出\n");
    match audio_endpoints() {
        Ok(eps) => {
            for e in &eps {
                o.push_str(&format!(
                    "  {} {} · 音量 {}% · {} · {}\n",
                    if e.selected { "▶当前" } else { "　　　" },
                    e.name,
                    e.volume,
                    if e.adjustable { "可调" } else { "固定音量" },
                    if e.is_default { "系统默认设备" } else { "" }
                ));
            }
            match eps.iter().find(|e| e.selected) {
                Some(c) if c.adjustable => {
                    nok += 1;
                    o.push_str(&format!("[正常] 当前控制的「{}」可调音量\n", c.name));
                }
                Some(c) => {
                    nwarn += 1;
                    o.push_str(&format!("[警告] 当前控制的「{}」是**固定音量**端点\n  ↳ Windows 自己的音量条也调不动它。想调这块屏的喇叭用「小米音量」滑块，或在上面的「输出设备」里换一个\n", c.name));
                }
                None => {
                    nwarn += 1;
                    o.push_str("[警告] 没有选中的输出设备\n");
                }
            }
        }
        Err(e) => {
            nbad += 1;
            o.push_str(&format!("[问题] 音频端点枚举失败：{}\n", e));
        }
    }

    // 【6】小米屏（MiTV）
    o.push_str("\n【6】小米显示器（自带 Android 系统）\n");
    if mitv_available() {
        nok += 1;
        o.push_str(&format!(
            "[正常] 显示器音量接口可达，当前音量 {}%\n",
            mitv_volume_get().unwrap_or(0)
        ));
    } else {
        nwarn += 1;
        o.push_str("[警告] 显示器音量接口不可达\n  ↳ 该屏是 Android 系统：确认它与电脑同网段、6095 端口可达；高频率连打会被它限流（通常几分钟自恢复）\n");
    }

    // 【7】电池
    o.push_str("\n【7】电池\n");
    match get_batteries() {
        Ok(bs) if !bs.is_empty() => {
            for b in &bs {
                o.push_str(&format!(
                    "  {} · {}% · {} · {}{}\n",
                    b.name,
                    b.percent,
                    if b.charging { "充电中" } else { "未充电" },
                    b.conn,
                    if b.health > 0 { format!(" · 健康度 {}%", b.health) } else { String::new() }
                ));
                if b.health > 0 && b.health < 70 {
                    nwarn += 1;
                    o.push_str(&format!("  ↳ 健康度 {}% 偏低（若不是保养模式限充，建议关注电池）\n", b.health));
                }
            }
            nok += 1;
        }
        Ok(_) => {
            nwarn += 1;
            o.push_str("[警告] 没有枚举到电池（台式机属正常）\n");
        }
        Err(e) => {
            nwarn += 1;
            o.push_str(&format!("[警告] 电池读取失败：{}\n", e));
        }
    }

    // 【8】基线与一键恢复
    o.push_str("\n【8】基线与一键恢复\n");
    let st = app_state();
    o.push_str(&format!(
        "  基线已采集：{} · HDR {} 屏 · 色彩 {} 屏 · 方向 {} 屏 · 小米音量 {}\n",
        if st.baseline_taken { "是" } else { "否" },
        st.base_hdr.len(),
        st.base_icc.len(),
        st.base_rot.len(),
        st.base_mitv
    ));
    o.push_str(&format!(
        "  退出软件自动还原：{}\n",
        if st.revert_on_exit { "开" } else { "关（可在「一键恢复默认」里打开）" }
    ));
    if !st.baseline_taken {
        nbad += 1;
        o.push_str("[问题] 基线未采集 →「一键恢复默认」不完整\n  ↳ 重启一次本程序即可自动采集\n");
    } else if st.base_rot.is_empty() || st.base_mitv == 0 || st.base_icc.is_empty() {
        nwarn += 1;
        o.push_str("[警告] 基线缺少部分字段（方向 / 小米音量 / 色彩）\n  ↳ 重启一次本程序会补齐\n");
    } else {
        nok += 1;
        o.push_str("[正常] 基线字段完整，「一键恢复默认」可覆盖全部软件层改动\n");
    }

    // 结论
    let total = nok + nwarn + nbad;
    o.push_str(&format!(
        "\n===== 结论 =====\n共 {} 项：{} 正常 / {} 警告 / {} 问题\n",
        total, nok, nwarn, nbad
    ));
    if nbad == 0 && nwarn == 0 {
        o.push_str("全部正常。\n");
    } else if nbad == 0 {
        o.push_str("没有需要修的问题，警告项多为硬件/系统限制（DDC 链路、固定音量端点、厂商 HDR 服务），按建议处置即可。\n");
    } else {
        o.push_str("有问题项，请按上面的「↳」建议处理；处理完可以再点一次检测对比。\n");
    }

    // 写日志：每次检测一个文件 + 追加一行总览，方便回溯
    let fname = format!("diag-{}.log", ts.replace(':', "-").replace(' ', "_"));
    let path = diag_log_dir().join(&fname);
    let mut body = o.clone();
    body.push_str(&format!("日志文件：{}\n", path.to_string_lossy()));
    let _ = std::fs::write(&path, &body);
    let hist = diag_log_dir().join("diag-history.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&hist) {
        use std::io::Write;
        let _ = f.write_all(
            format!("{} | {} 正常 / {} 警告 / {} 问题\n", ts, nok, nwarn, nbad).as_bytes(),
        );
    }
    o.push_str(&format!("\n（已写入日志：{}\\diag-…log）", diag_log_path()));
    Ok(o)
}

/// 一键修复：把检测里「能安全自动处理」的问题逐条修掉，然后自动复查一遍。
/// 原则：只动软件层能确定的事（重协商链路、亮度复位、切到可调音频端点、补齐基线、
/// 还原被铺满的窗口）；硬件/系统限制类的（DDC 链路、厂商 HDR 服务、固定音量端点本身）
/// 不硬修，只如实说明。
pub fn diag_fix() -> Result<String, String> {
    mark_change(); // 修复过程本身会改显示配置，先标记，避免自动修复跟着掺和
    let mut done: Vec<String> = Vec::new();
    let mut skip: Vec<String> = Vec::new();

    // ① 没枚举到显示器 / 有掉线记录 → 强制重协商整条链路
    let evn = health_events(1440)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    let nmon = get_displays().len();
    if nmon == 0 || evn > 0 {
        let lvl = if nmon == 0 { 2 } else { 1 };
        match repair_display(lvl) {
            Ok(o) => done.push(format!(
                "重协商显示链路（{}）：{}",
                if lvl == 2 { "深度" } else { "轻修" },
                o.chars().take(60).collect::<String>()
            )),
            Err(e) => skip.push(format!("重协商显示链路：{}", e)),
        }
    } else {
        done.push("显示链路：近 24 小时无掉线记录，无需重协商".to_string());
    }

    // ② 总亮度不在原始曲线 → 复位
    let g = gamma_get();
    let not_100 = g
        .lines()
        .filter(|l| l.starts_with("G|"))
        .any(|l| !l.trim().ends_with("|100"));
    if not_100 {
        match gamma_set(100) {
            Ok(_) => done.push("总亮度已复位到 100%（原始曲线）".to_string()),
            Err(e) => skip.push(format!("总亮度复位：{}", e)),
        }
    }

    // ③ 当前控制的是「固定音量」端点 → 自动切到一个可调端点
    if let Ok(eps) = audio_endpoints() {
        let bad = eps
            .iter()
            .find(|e| e.selected)
            .map(|e| !e.adjustable)
            .unwrap_or(false);
        if bad {
            let good = eps
                .iter()
                .find(|e| e.adjustable && e.is_default)
                .or_else(|| eps.iter().find(|e| e.adjustable));
            match good {
                Some(t) => match set_audio_device(&t.id) {
                    Ok(_) => done.push(format!("音频控制设备已切到可调的「{}」", t.name)),
                    Err(e) => skip.push(format!("切换音频设备：{}", e)),
                },
                None => skip.push("音频：没有找到可调的输出端点".to_string()),
            }
        }
    }

    // ④ 基线不完整 → 补齐（旋转方向 / 小米音量 / 色彩）
    let st = app_state();
    if !st.baseline_taken || st.base_rot.is_empty() || st.base_mitv == 0 || st.base_icc.is_empty() {
        match capture_baseline() {
            Ok(o) => done.push(format!("恢复基线：{}", o)),
            Err(e) => skip.push(format!("恢复基线：{}", e)),
        }
    }

    // ⑤ 有窗口被铺满还没还原 → 还原
    let _ = restore_foreground();

    // ⑥ 小米屏接口（能修的是「等它自己恢复」，不硬修）
    if !mitv_available() {
        skip.push("小米屏音量接口当前不可达：确认同网段/6095 端口可达；高频连打会被限流，等几分钟再试".to_string());
    }

    let mut o = String::new();
    o.push_str("一键修复结果\n");
    o.push_str(&format!("已处理 {} 项：\n", done.len()));
    for d in &done {
        o.push_str(&format!("  ✓ {}\n", d));
    }
    if !skip.is_empty() {
        o.push_str(&format!("\n无法自动修 / 需你确认 {} 项：\n", skip.len()));
        for s in &skip {
            o.push_str(&format!("  ! {}\n", s));
        }
    }
    o.push_str("\n========== 修复后自动复查 ==========\n");
    match diag_scan() {
        Ok(r) => o.push_str(&r),
        Err(e) => o.push_str(&format!("复查失败：{}\n", e)),
    }
    Ok(o)
}

// ---------- 双屏对齐体检（只读分析，不改系统设置） ----------
// 原理：一个窗口跨两块屏时，Windows 只用**一个像素比例**、不做逐屏缩放。
// 所以「跨屏画面能对得齐」的唯一条件是：两屏的**竖向像素高度 ≈ 与各自物理高度成正比**，
// 等价于「竖向像素密度 PPI 一致」。物理高度接近的两块屏，只要竖向像素数相同就基本完美。

#[derive(Clone)]
struct AlignInfo {
    manuf: String,
    dev: String,
    name: String,
    is_main: bool,
    px: (u32, u32),
    signal_px: Option<(u32, u32)>,
    modes: Vec<(u32, u32)>,
    phys_w: f64,
    phys_h: f64,
    src: String,
    nat: f64,
}

/// 读注册表 EDID 的物理尺寸：返回 (厂商代码, cmH, cmV)
fn edid_sizes() -> Vec<(String, u32, u32)> {
    let script = "Get-ChildItem 'HKLM:\\SYSTEM\\CurrentControlSet\\Enum\\DISPLAY' -ErrorAction SilentlyContinue | ForEach-Object { $m=$_.PSChildName; Get-ChildItem $_.PSPath -ErrorAction SilentlyContinue | ForEach-Object { try { $e=(Get-ItemProperty ($_.PSPath + '\\Device Parameters') -Name EDID -ErrorAction Stop).EDID; if ($e.Length -ge 128) { 'E|' + $m + '|' + $e[21] + '|' + $e[22] } } catch {} } }";
    let mut v = Vec::new();
    if let Ok(out) = ps(script) {
        for l in out.lines().filter(|l| l.starts_with("E|")) {
            let p: Vec<&str> = l.split('|').collect();
            if p.len() >= 4 {
                v.push((
                    p[1].trim().to_string(),
                    p[2].trim().parse().unwrap_or(0),
                    p[3].trim().parse().unwrap_or(0),
                ));
            }
        }
    }
    v
}

/// 记下某块屏的物理尺寸（对角线英寸），供 EDID 没上报时使用
pub fn set_screen_inches(manuf: &str, inches: f64) -> Result<(), String> {
    let mut st = app_state();
    st.screen_in.retain(|e| e.split('|').next() != Some(manuf));
    st.screen_in.push(format!("{}|{}", manuf, inches));
    save_app_state(&st)
}

#[cfg(target_os = "windows")]
extern "system" {
    fn EnumDisplaySettingsW(dev: *const u16, mode: u32, dm: *mut u8) -> i32;
    fn ChangeDisplaySettingsExW(dev: *const u16, dm: *mut u8, hwnd: *mut core::ffi::c_void, flags: u32, param: *mut core::ffi::c_void) -> i32;
}

const DM_PELSWIDTH: u32 = 0x0008_0000;
const DM_PELSHEIGHT: u32 = 0x0010_0000;
const DM_DISPLAYFREQUENCY: u32 = 0x0040_0000;
const CDS_TEST: u32 = 2;
const DM_POSITION: u32 = 0x0000_0020;
const CDS_NORESET: u32 = 0x1000_0000;
const CDS_UPDATEREGISTRY: u32 = 0x0000_0001;

/// 读某块屏在虚拟桌面里的当前位置（x, y）—— dmPosition 在 DEVMODEW 偏移 76/80
fn cur_pos(dev: &str) -> (i32, i32) {
    let name = dev_name_ptr(dev);
    let mut b = [0u8; 220];
    b[68] = 220;
    let ok = unsafe { EnumDisplaySettingsW(name.as_ptr(), 0xFFFF_FFFF, b.as_mut_ptr()) };
    if ok == 0 {
        return (0, 0);
    }
    (
        i32::from_le_bytes([b[76], b[77], b[78], b[79]]),
        i32::from_le_bytes([b[80], b[81], b[82], b[83]]),
    )
}

/// 把一块屏移到 (x,y)：用 CDS_NORESET（先攒着，最后统一次生效）
/// 注意：**必须基于当前模式**（先 EnumDisplaySettings 取回完整 devmode）再改 dmPosition，
/// 否则只带 DM_POSITION 的调用会被驱动拒掉（实测返回 -4 = BADFLAGS）。
fn set_pos_norset(dev: &str, x: i32, y: i32) -> Result<(), String> {
    let name = dev_name_ptr(dev);
    let mut b = [0u8; 220];
    b[68] = 220;
    let ok = unsafe { EnumDisplaySettingsW(name.as_ptr(), 0xFFFF_FFFF, b.as_mut_ptr()) };
    if ok == 0 {
        return Err(format!("读 {} 当前模式失败", dev));
    }
    // 在已有模式基础上追加位置字段。保留驱动当前 dmFields 位，避免提交位置时
    // 丢掉缩放/旋转等驱动字段；默认刷新率 0/1 不显式写入，避免位置提交时
    // 把驱动默认刷新率误当成一个具体频率重新应用。
    let current_fields = u32::from_le_bytes([b[72], b[73], b[74], b[75]]);
    let current_hz = u32::from_le_bytes([b[184], b[185], b[186], b[187]]);
    let mut fields = current_fields | DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT;
    if current_hz > 1 {
        fields |= DM_DISPLAYFREQUENCY;
    } else {
        fields &= !DM_DISPLAYFREQUENCY;
    }
    b[72..76].copy_from_slice(&fields.to_le_bytes());
    b[76..80].copy_from_slice(&x.to_le_bytes());
    b[80..84].copy_from_slice(&y.to_le_bytes());
    let r = unsafe {
        // 注意：CDS_NORESET 必须和 CDS_UPDATEREGISTRY 一起用（0x10000001）。
        // 只给 CDS_NORESET 会被驱动拒掉（实测返回 -4 BADFLAGS）——这是官方样例的写法。
        ChangeDisplaySettingsExW(
            name.as_ptr(),
            b.as_mut_ptr(),
            std::ptr::null_mut(),
            CDS_NORESET | CDS_UPDATEREGISTRY,
            std::ptr::null_mut(),
        )
    };
    if r != 0 {
        return Err(format!("设置 {} 位置失败（码 {}）", dev, r));
    }
    Ok(())
}

/// 让两块屏的**顶边对齐**（消除上下错位）。
/// 为什么必须做：两屏若上下错开，虚拟桌面高度会变成两屏高度之和，
/// 跨屏铺满时画面会被**竖着切成两半**（一边上半幅、一边下半幅）。
/// 顶边拉齐后虚拟桌面 = (两屏宽度之和) × (公共高度)，一个窗口正好完整覆盖两块屏。
pub fn align_positions() -> Result<String, String> {
    let infos = align_gather();
    if infos.len() < 2 {
        return Err("需要两块以上已连接的屏".to_string());
    }
    // 把面积最大的那块当主屏，位置不动；其余屏顶边与主屏齐平、水平紧密相邻
    let mut main_idx = 0usize;
    let mut best = 0u64;
    for (i, it) in infos.iter().enumerate() {
        let a = (it.3).0 as u64 * (it.3).1 as u64;
        if a > best {
            best = a;
            main_idx = i;
        }
    }
    let (mdev, _, _, mcur, _, _) = infos[main_idx].clone();
    let (mx, my) = cur_pos(&mdev);
    let mut cursor_right = mx + mcur.0 as i32;
    let mut expected = vec![(mdev.clone(), mx, my)];
    let mut done = Vec::new();
    let mut changed = false;
    for (i, it) in infos.iter().enumerate() {
        if i == main_idx {
            continue;
        }
        let (dev, _, _, cur, _, _) = it;
        let (px, py) = cur_pos(dev);
        let on_left = px < mx;
        let tx = if on_left { mx - cur.0 as i32 } else { cursor_right };
        if !on_left {
            cursor_right += cur.0 as i32;
        }
        if px != tx || py != my {
            changed = true;
        }
        expected.push((dev.clone(), tx, my));
        done.push(format!("{} 从 y={} 拉到 y={}、x={}", dev, py, my, tx));
    }
    let mut apply_method = "位置未变化".to_string();
    if changed {
        let mapping = expected
            .iter()
            .map(|(dev, x, y)| format!("{}|{}|{}", dev, x, y))
            .collect::<Vec<_>>()
            .join(";");
        let result = ps_core(&format!(
            "$r=[SGCore]::SetDisplayPositions('{}',$true); Write-Output $r",
            mapping
        ));
        match result {
            Ok(output) if output.trim().starts_with("OK|") => {
                apply_method = "SetDisplayConfig 保留当前显示模式".to_string();
            }
            mode_preserving_error => {
                let mode_preserving_error = match mode_preserving_error {
                    Ok(output) => output.trim().to_string(),
                    Err(error) => error,
                };
                // 兼容拒绝 supplied-config 的旧驱动。旧 DEVMODE 路径仍可用，
                // 但它会重新提交当前宽高，明确标记回退以便诊断模式变化问题。
                for (dev, x, y) in &expected {
                    if dev == &mdev && *x == mx && *y == my {
                        continue;
                    }
                    let (px, py) = cur_pos(dev);
                    if px != *x || py != *y {
                        set_pos_norset(dev, *x, *y)?;
                    }
                }
                let r = unsafe {
                    ChangeDisplaySettingsExW(
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                    )
                };
                if r != 0 {
                    return Err(format!(
                        "SetDisplayConfig 失败（{}）；DEVMODE 回退应用位置失败（码 {}）",
                        mode_preserving_error,
                        r
                    ));
                }
                apply_method = format!(
                    "DEVMODE 兼容回退（SetDisplayConfig 未成功：{}）",
                    mode_preserving_error
                );
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1200));
    }
    for (dev, x, y) in &expected {
        let (actual_x, actual_y) = cur_pos(dev);
        if actual_x != *x || actual_y != *y {
            return Err(format!(
                "显示器位置未能应用：{} 目标 ({},{}), 实际 ({},{})",
                dev, x, y, actual_x, actual_y
            ));
        }
    }
    Ok(format!(
        "已把两屏顶边对齐（{}）：{}",
        apply_method,
        done.join("；")
    ))
}

/// 从当前完整 DEVMODEW 出发，仅改分辨率和刷新率，保留旋转、位置及驱动字段。
fn devmode_buf(dev: &str, w: u32, h: u32, hz: u32) -> Result<[u8; 220], String> {
    let name = dev_name_ptr(dev);
    let mut b = [0u8; 220];
    b[68] = 220;
    if unsafe { EnumDisplaySettingsW(name.as_ptr(), 0xFFFF_FFFF, b.as_mut_ptr()) } == 0 {
        return Err(format!("读取 {} 当前 DEVMODE 失败", dev));
    }
    let mut fields = u32::from_le_bytes([b[72], b[73], b[74], b[75]])
        | DM_PELSWIDTH | DM_PELSHEIGHT;
    // DEVMODE uses 0/1 to request the device's hardware-default refresh rate.
    // Keep that sentinel out of dmFields so the driver can choose its default.
    if hz > 1 {
        fields |= DM_DISPLAYFREQUENCY;
    } else {
        fields &= !DM_DISPLAYFREQUENCY;
    }
    b[72..76].copy_from_slice(&fields.to_le_bytes());
    b[172..176].copy_from_slice(&w.to_le_bytes());
    b[176..180].copy_from_slice(&h.to_le_bytes());
    b[184..188].copy_from_slice(&hz.to_le_bytes());
    Ok(b)
}

fn dev_name_ptr(dev: &str) -> Vec<u16> {
    let mut n: Vec<u16> = dev.encode_utf16().collect();
    n.push(0);
    n
}

/// 切某块屏的分辨率。`apply=false` 只"试算"（CDS_TEST，不改任何东西）。
/// 只接受**驱动已支持**的模式 —— 自定义分辨率必须先在显卡控制面板里添加
/// （实测：程序直接试算自定义值一律返回 -2 BADFLAGS，现有模式则返回 0）。
/// 应用时用 flags=0（**不写注册表**），所以重启/崩溃后会自动回到原模式，天然安全。
pub fn set_mode(dev: &str, w: u32, h: u32, hz: u32, apply: bool) -> Result<String, String> {
    let name = dev_name_ptr(dev);
    let mut b = devmode_buf(dev, w, h, hz)?;
    let t = unsafe { ChangeDisplaySettingsExW(name.as_ptr(), b.as_mut_ptr(), std::ptr::null_mut(), CDS_TEST, std::ptr::null_mut()) };
    if t != 0 {
        return Err(format!("{}x{}@{} 驱动不支持（试算码 {}）", w, h, hz, t));
    }
    if !apply {
        return Ok(format!("{}x{}@{} 可用", w, h, hz));
    }
    let mut b2 = devmode_buf(dev, w, h, hz)?;
    let r = unsafe { ChangeDisplaySettingsExW(name.as_ptr(), b2.as_mut_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut()) };
    if r != 0 {
        return Err(format!("切到 {}x{}@{} 失败（码 {}）", w, h, hz, r));
    }
    Ok(format!("{}x{}@{}", w, h, hz))
}

/// 读某块屏当前模式
fn cur_mode(dev: &str) -> (u32, u32, u32) {
    let name = dev_name_ptr(dev);
    let mut b = [0u8; 220];
    b[68] = 220;
    let ok = unsafe { EnumDisplaySettingsW(name.as_ptr(), 0xFFFF_FFFF, b.as_mut_ptr()) };
    if ok == 0 {
        return (0, 0, 0);
    }
    let w = u32::from_le_bytes([b[172], b[173], b[174], b[175]]);
    let h = u32::from_le_bytes([b[176], b[177], b[178], b[179]]);
    let f = u32::from_le_bytes([b[184], b[185], b[186], b[187]]);
    (w, h, f)
}

/// 收集两屏的对齐信息（dev / 名称 / 可用分辨率 / 当前模式 / 物理高度 / 原生比例）
fn align_gather() -> Vec<(String, String, Vec<(u32, u32)>, (u32, u32, u32), f64, f64)> {
    let edid = edid_sizes();
    let st = app_state();
    let md = mode_detail();
    let mut out = Vec::new();
    for d in get_displays().into_iter().filter(|d| d.connected) {
        let manuf = d.id.split('\\').nth(1).unwrap_or("").to_string();
        let dev = resolve_dev(&d.id).unwrap_or_default();
        if dev.is_empty() {
            continue;
        }
        let cur = cur_mode(&dev);
        if cur.0 == 0 {
            continue;
        }
        let mut modes = display_modes_ffi(&dev);
        if !modes.contains(&(cur.0, cur.1)) {
            modes.push((cur.0, cur.1));
        }
        let nat_mode = modes.iter().cloned().max_by_key(|m| (m.0 as u64) * (m.1 as u64)).unwrap_or((cur.0, cur.1));
        let native_ratio = nat_mode.0 as f64 / nat_mode.1 as f64;
        let portrait = cur.1 > cur.0;
        let nat = if portrait { 1.0 / native_ratio } else { native_ratio };
        // 物理高度（英寸）
        let mut ph = 0.0f64;
        if let Some((_, ch, cv)) = edid.iter().find(|(m, a, b)| *m == manuf && *a > 0 && *b > 0).cloned() {
            // EDID尺寸按面板原生横向记录；竖屏时当前垂直边对应原生物理宽度。
            ph = if portrait { ch as f64 } else { cv as f64 } / 2.54;
        }
        if ph <= 0.0 {
            if let Some(ent) = st.screen_in.iter().find(|e| e.split('|').next() == Some(manuf.as_str())) {
                let p: Vec<&str> = ent.split('|').collect();
                if p.len() >= 2 {
                    if let Ok(di) = p[1].trim().parse::<f64>() {
                        if di > 5.0 {
                            let nh = di / (native_ratio * native_ratio + 1.0).sqrt();
                            let nw = nh * native_ratio;
                            ph = if portrait { nw } else { nh };
                        }
                    }
                }
            }
        }
        let _ = &md;
        out.push((dev, d.name.clone(), modes, cur, ph, nat));
    }
    out
}

/// 记录两块屏的原始模式（只在基线为空时写一次）
fn snapshot_modes(infos: &[(String, String, Vec<(u32, u32)>, (u32, u32, u32), f64, f64)]) -> Result<(), String> {
    let mut st = app_state();
    if !st.base_mode.is_empty() {
        return Ok(());
    }
    st.base_mode = infos
        .iter()
        .map(|(dev, _, _, cur, _, _)| format!("{}|{}x{}|{}", dev, cur.0, cur.1, cur.2))
        .collect();
    if st.base_pos.is_empty() {
        st.base_pos = infos
            .iter()
            .map(|(dev, _, _, _, _, _)| {
                let (x, y) = cur_pos(dev);
                format!("{}|{}|{}", dev, x, y)
            })
            .collect();
    }
    save_app_state(&st)
}

/// 还原两块屏的原始模式（退出软件 / 关闭双屏铺满 / 一键恢复默认都会调它）
pub fn align_restore_modes() -> Result<String, String> {
    let st = app_state();
    if st.base_mode.is_empty() {
        return Ok("没有记录过原始分辨率，无需还原".to_string());
    }
    let mut done = Vec::new();
    let mut errors = Vec::new();
    for e in st.base_mode.iter() {
        let p: Vec<&str> = e.split('|').collect();
        if p.len() >= 3 {
            let dev = p[0];
            let wh: Vec<&str> = p[1].split('x').collect();
            if wh.len() == 2 {
                let w: u32 = wh[0].parse().unwrap_or(0);
                let h: u32 = wh[1].parse().unwrap_or(0);
                let hz: u32 = p[2].parse().unwrap_or(60);
                if cur_mode(dev) == (w, h, hz) {
                    done.push(format!("{} 已是原模式 {}x{}@{}，未重设", dev, w, h, hz));
                    continue;
                }
                match set_mode(dev, w, h, hz, true) {
                    Ok(s) => done.push(format!("{} → {}", dev, s)),
                    Err(e2) => errors.push(format!("{} 分辨率还原失败：{}", dev, e2)),
                }
            }
        }
    }
    // 位置也放回去（对齐时把两屏顶边拉齐过）
    if !st.base_pos.is_empty() {
        let mut changed = false;
        let mut expected = Vec::new();
        for e in st.base_pos.iter() {
            let p: Vec<&str> = e.split('|').collect();
            if p.len() >= 3 {
                let x: i32 = p[1].parse().unwrap_or(0);
                let y: i32 = p[2].parse().unwrap_or(0);
                let current = cur_pos(p[0]);
                if current != (x, y) {
                    if let Err(e) = set_pos_norset(p[0], x, y) {
                        errors.push(format!("{} 位置还原失败：{}", p[0], e));
                        continue;
                    }
                    changed = true;
                }
                expected.push((p[0], x, y));
            }
        }
        if changed {
            let apply = unsafe {
                ChangeDisplaySettingsExW(std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), 0, std::ptr::null_mut())
            };
            if apply != 0 {
                errors.push(format!("应用原始屏幕位置失败（码 {}）", apply));
            } else {
                std::thread::sleep(std::time::Duration::from_millis(900));
            }
        }
        for (dev, x, y) in expected {
            let actual = cur_pos(dev);
            if actual != (x, y) {
                errors.push(format!("{} 位置未恢复：目标 ({},{}), 实际 ({},{})", dev, x, y, actual.0, actual.1));
            }
        }
        if !changed && errors.is_empty() {
            done.push("屏幕位置本来已是原布局，未重设".to_string());
        } else if changed && errors.is_empty() {
            done.push("屏幕位置已恢复到原布局".to_string());
        }
    }
    if !errors.is_empty() {
        return Err(format!("显示器布局还原未完全成功：{}", errors.join("；")));
    }
    Ok(format!("已还原原始分辨率：{}", done.join("；")))
}

/// 双屏对齐：从**两屏都支持**的模式里挑出"竖向高度相同、各自最接近原生比例、分辨率尽量高"的组合，
/// 逐个试算，第一个两块屏都接受的直接应用。应用前自动记录原始模式以便还原。
pub fn align_apply() -> Result<String, String> {
    let infos = align_gather();
    if infos.len() < 2 {
        return Err("需要两块以上已连接的屏才能对齐".to_string());
    }
    snapshot_modes(&infos)?;
    let a = infos[0].clone();
    let b = infos[1].clone();
    // 候选只接受完全相同的竖向像素数。宽高比按当前屏幕方向比较；
    // 两屏物理高度已知时，再优先选择竖向 PPI 接近的组合。
    let mut cands: Vec<(f64, (u32, u32), (u32, u32), u32, u32, f64)> = Vec::new();
    for m1 in a.2.iter() {
        for m2 in b.2.iter() {
            if m1.1 != m2.1 {
                continue;
            }
            // Keep an already working pair (for example 2560x1600 + 1024x1600)
            // intact. Re-applying an unchanged mode through CDS can change how
            // some drivers scale the signal, even though Settings uses that mode.
            let current_penalty = (if (m1.0, m1.1) == (a.3.0, a.3.1) { 0.0 } else { 1.0 })
                + (if (m2.0, m2.1) == (b.3.0, b.3.1) { 0.0 } else { 1.0 });
            let d1 = ((m1.0 as f64 / m1.1 as f64) - a.5).abs() / a.5;
            let d2 = ((m2.0 as f64 / m2.1 as f64) - b.5).abs() / b.5;
            let ppi1 = if a.4 > 0.0 { m1.1 as f64 / a.4 } else { 0.0 };
            let ppi2 = if b.4 > 0.0 { m2.1 as f64 / b.4 } else { 0.0 };
            let pdiff = if ppi1 > 0.0 && ppi2 > 0.0 { (ppi1.max(ppi2) / ppi1.min(ppi2)) - 1.0 } else { 0.0 };
            // 保持两屏原生比例优先；PPI用于区分可行共同高度，分辨率只作为最后排序项。
            let area_pen = 1.0 / (m1.0 as f64 * m1.1 as f64 + m2.0 as f64 * m2.1 as f64).sqrt();
            // EnumDisplaySettings can report a rounded 59/60 Hz value that differs
            // from the active DEVMODE. If this resolution is already active, trust
            // that exact current mode and don't switch refresh rate just to align.
            let current_res1 = (m1.0, m1.1) == (a.3.0, a.3.1);
            let current_res2 = (m2.0, m2.1) == (b.3.0, b.3.1);
            // EnumDisplaySettings documents 0 and 1 as "hardware default" refresh,
            // not 0/1 Hz. Preserve that sentinel when resolution is already active;
            // replacing 0 with 60 would unnecessarily resubmit the current mode.
            let mut rates1 = if current_res1 {
                vec![a.3.2]
            } else {
                display_refresh_rates_ffi(&a.0, m1.0, m1.1)
            };
            let mut rates2 = if current_res2 {
                vec![b.3.2]
            } else {
                display_refresh_rates_ffi(&b.0, m2.0, m2.1)
            };
            if rates1.is_empty() { rates1.push(if a.3.2 > 1 { a.3.2 } else { 0 }); }
            if rates2.is_empty() { rates2.push(if b.3.2 > 1 { b.3.2 } else { 0 }); }
            for hz1 in &rates1 {
                for hz2 in &rates2 {
                    // A default-rate sentinel has no numeric Hz preference. If the
                    // resolution is unchanged, the rate is unchanged too; if it is
                    // changing, do not invent a preference for 60 Hz.
                    let refresh_delta1 = if current_res1 || (a.3.2 <= 1 && *hz1 <= 1) {
                        0
                    } else if a.3.2 <= 1 {
                        1 // Prefer the driver's default when its numeric rate is unknown.
                    } else {
                        hz1.abs_diff(a.3.2)
                    };
                    let refresh_delta2 = if current_res2 || (b.3.2 <= 1 && *hz2 <= 1) {
                        0
                    } else if b.3.2 <= 1 {
                        1
                    } else {
                        hz2.abs_diff(b.3.2)
                    };
                    let refresh_delta = refresh_delta1 + refresh_delta2;
                    let score = current_penalty * 10_000.0
                        + (d1 + d2) * 100.0 + pdiff * 20.0 + refresh_delta as f64 * 10.0 + area_pen;
                    cands.push((score, *m1, *m2, *hz1, *hz2, pdiff));
                }
            }
        }
    }
    cands.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
    if cands.is_empty() {
        return Err("两块屏没有任何竖向高度相同的模式组合 —— 需要在显卡控制面板里给其中一块屏添加自定义分辨率（见「对齐体检」里的理想方案）".to_string());
    }
    let mut tried: Vec<String> = Vec::new();
    let mut refresh_trials: HashMap<((u32, u32), (u32, u32)), usize> = HashMap::new();
    let mut attempted = 0usize;
    for (_, m1, m2, hz1, hz2, pdiff) in &cands {
        if attempted >= 48 { break; }
        let per_pair = refresh_trials.entry((*m1, *m2)).or_insert(0);
        if *per_pair >= 4 { continue; }
        *per_pair += 1;
        attempted += 1;
        if ((m1.0, m1.1, *hz1) != a.3 && set_mode(&a.0, m1.0, m1.1, *hz1, false).is_err())
            || ((m2.0, m2.1, *hz2) != b.3 && set_mode(&b.0, m2.0, m2.1, *hz2, false).is_err()) {
            continue;
        }
        if (m1.0, m1.1, *hz1) != a.3 {
            let r1 = set_mode(&a.0, m1.0, m1.1, *hz1, true);
            if r1.is_err() {
                tried.push(format!("{}x{} 失败", m1.0, m1.1));
                continue;
            }
        }
        if (m2.0, m2.1, *hz2) != b.3 {
            let r2 = set_mode(&b.0, m2.0, m2.1, *hz2, true);
            if r2.is_err() {
                let _ = align_restore_modes();
                tried.push(format!("{}x{} 失败（已回滚）", m2.0, m2.1));
                continue;
            }
        }
        let ca = cur_mode(&a.0);
        let cb = cur_mode(&b.0);
        let rate_matches = |actual: u32, requested: u32| actual == requested || (actual <= 1 && requested <= 1);
        let source_modes_match = ca.0 == m1.0 && ca.1 == m1.1 && rate_matches(ca.2, *hz1)
            && cb.0 == m2.0 && cb.1 == m2.1 && rate_matches(cb.2, *hz2)
            && ca.1 == cb.1 && ca.1 > 0;
        if source_modes_match {
            // 分辨率高度一致了，再把两块屏的**顶边拉齐**（否则虚拟桌面高度会翻倍、画面被竖切）
            let posmsg = match align_positions() {
                Ok(p) => p,
                Err(e) => {
                    let rollback = align_restore_modes()
                        .map(|s| format!("；回滚结果：{}", s))
                        .unwrap_or_else(|re| format!("；回滚也失败：{}", re));
                    return Err(format!("模式已对齐，但屏幕位置校验失败，未报告铺满成功：{}{}", e, rollback));
                }
            };
            // Position updates must preserve the selected desktop modes. The
            // target signal can legitimately be a different native timing on
            // each display, so it is diagnostic only and must not block an
            // otherwise valid equal-height desktop layout.
            let final_a = cur_mode(&a.0);
            let final_b = cur_mode(&b.0);
            if final_a.0 != m1.0 || final_a.1 != m1.1 || final_b.0 != m2.0
                || final_b.1 != m2.1 || final_a.1 != final_b.1
            {
                let rollback = align_restore_modes()
                    .map(|s| format!("；已尝试回滚：{}", s))
                    .unwrap_or_else(|e| format!("；自动回滚失败：{}", e));
                return Err(format!(
                    "屏幕位置调整后桌面模式发生变化（{}x{} / {}x{}），未报告对齐成功{}",
                    final_a.0, final_a.1, final_b.0, final_b.1, rollback
                ));
            }
            let final_detail = mode_detail();
            let signal_label = |signal: Option<(u32, u32)>| {
                signal.map(|(w, h)| format!("{}x{}", w, h)).unwrap_or_else(|| "无法读取".to_string())
            };
            let signal_msg = format!(
                "；活动输出信号（诊断信息，不作对齐门槛）：{} / {}",
                signal_label(active_signal_size(&final_detail, &a.0)),
                signal_label(active_signal_size(&final_detail, &b.0))
            );
            return Ok(format!(
                "已对齐：{} {} → {}x{}@{}；{} {} → {}x{}@{}\n两屏桌面竖向像素高度一致（{}）{}{}\n{}",
                a.1, a.0, ca.0, ca.1, ca.2, b.1, b.0, cb.0, cb.1, cb.2, ca.1,
                if *pdiff > 0.0 { format!("；估算竖向像素密度差 {:.1}%", pdiff * 100.0) } else { String::new() },
                signal_msg,
                posmsg
            ));
        }
        let _ = align_restore_modes();
        tried.push(format!("{}x{}+{}x{} 应用后模式与目标不一致（已回滚）", m1.0, m1.1, m2.0, m2.1));
    }
    Err(format!("试了 {} 组都不行：{}", tried.len(), tried.join("；")))
}

/// 枚举某块屏支持的所有分辨率 —— **Rust 原生直接调 user32**，不经过 PowerShell/C#。
/// 为什么绕开 C#：同一个 220 字节缓冲在 Python 里能通，走 C#+PowerShell 却一直返回
/// 失败（203），怀疑是命令行/托管编组环节的问题；原生 FFI 最直接也最快。
/// DEVMODEW(Unicode) 关键偏移：dmSize=68、dmPelsWidth=172、dmPelsHeight=176、
/// dmDisplayFrequency=184（总长 220）。
fn display_modes_ffi(dev: &str) -> Vec<(u32, u32)> {
    let mut name: Vec<u16> = dev.encode_utf16().collect();
    name.push(0);
    let mut out: Vec<(u32, u32)> = Vec::new();
    for i in 0..2000u32 {
        let mut buf = [0u8; 220];
        buf[68] = 220;
        let ok = unsafe { EnumDisplaySettingsW(name.as_ptr(), i, buf.as_mut_ptr()) };
        if ok == 0 {
            break;
        }
        let w = u32::from_le_bytes([buf[172], buf[173], buf[174], buf[175]]);
        let h = u32::from_le_bytes([buf[176], buf[177], buf[178], buf[179]]);
        let f = u32::from_le_bytes([buf[184], buf[185], buf[186], buf[187]]);
        if w >= 640 && h >= 480 && (f <= 1 || f >= 24) && !out.contains(&(w, h)) {
            out.push((w, h));
        }
    }
    out
}

fn active_signal_size(mode_detail: &str, dev: &str) -> Option<(u32, u32)> {
    mode_detail.lines().find_map(|line| {
        let p: Vec<&str> = line.split('|').collect();
        if p.len() < 7 || p[0] != "M" || !p[1].eq_ignore_ascii_case(dev) {
            return None;
        }
        let (w, h) = p[6].split_once('x')?;
        let size = (w.parse().ok()?, h.parse().ok()?);
        if size.0 > 0 && size.1 > 0 { Some(size) } else { None }
    })
}

#[cfg(target_os = "windows")]
fn display_refresh_rates_ffi(dev: &str, width: u32, height: u32) -> Vec<u32> {
    let mut name: Vec<u16> = dev.encode_utf16().collect();
    name.push(0);
    let mut out = Vec::new();
    for i in 0..2000u32 {
        let mut buf = [0u8; 220];
        buf[68] = 220;
        let ok = unsafe { EnumDisplaySettingsW(name.as_ptr(), i, buf.as_mut_ptr()) };
        if ok == 0 { break; }
        let w = u32::from_le_bytes([buf[172], buf[173], buf[174], buf[175]]);
        let h = u32::from_le_bytes([buf[176], buf[177], buf[178], buf[179]]);
        let hz = u32::from_le_bytes([buf[184], buf[185], buf[186], buf[187]]);
        if w == width && h == height && (hz <= 1 || (24..=1000).contains(&hz)) && !out.contains(&hz) {
            out.push(hz);
        }
    }
    out
}

#[cfg(not(target_os = "windows"))]
fn display_modes_ffi(_dev: &str) -> Vec<(u32, u32)> {
    Vec::new()
}

#[cfg(not(target_os = "windows"))]
fn display_refresh_rates_ffi(_dev: &str, _width: u32, _height: u32) -> Vec<u32> {
    Vec::new()
}

pub fn align_report() -> Result<String, String> {
    let edid = edid_sizes();
    let st = app_state();
    let md = mode_detail();
    let mut infos: Vec<AlignInfo> = Vec::new();
    for d in get_displays().into_iter().filter(|d| d.connected) {
        let manuf = d.id.split('\\').nth(1).unwrap_or("").to_string();
        let dev = resolve_dev(&d.id).unwrap_or_default();
        let mut px = (0u32, 0u32);
        for l in md.lines() {
            let p: Vec<&str> = l.split('|').collect();
            if p.len() >= 4 && p[0] == "M" && p[1] == dev {
                px = (p[2].trim().parse().unwrap_or(0), p[3].trim().parse().unwrap_or(0));
            }
        }
        if px.0 == 0 || px.1 == 0 {
            continue;
        }
        let mut modes: Vec<(u32, u32)> = display_modes_ffi(&dev);
        if !modes.contains(&px) {
            modes.push(px);
        }
        let nat_mode = modes
            .iter()
            .cloned()
            .max_by_key(|m| (m.0 as u64) * (m.1 as u64))
            .unwrap_or(px);
        let portrait = px.1 > px.0;
        let native_ratio = nat_mode.0 as f64 / nat_mode.1 as f64;
        // 模式枚举和 EDID 尺寸以面板原生方向为基准；报告值改成当前方向，
        // 这样竖屏的比例和物理画布高都与实际纵向像素对应。
        let nat = if portrait { 1.0 / native_ratio } else { native_ratio };
        let (mut pw, mut ph, mut src) = (0.0f64, 0.0f64, String::from("未知（EDID 未上报，请手动填英寸）"));
        if let Some((_, ch, cv)) = edid.iter().find(|(m, a, b)| *m == manuf && *a > 0 && *b > 0).cloned() {
            // EDID 按面板原生方向上报尺寸；竖屏时当前纵向边是原生物理宽度。
            pw = if portrait { cv as f64 } else { ch as f64 } / 2.54;
            ph = if portrait { ch as f64 } else { cv as f64 } / 2.54;
            src = format!("EDID 上报 {}×{} cm", ch, cv);
        }
        if ph <= 0.0 {
            if let Some(ent) = st
                .screen_in
                .iter()
                .find(|e| e.split('|').next() == Some(manuf.as_str()))
            {
                let p: Vec<&str> = ent.split('|').collect();
                if p.len() >= 2 {
                    if let Ok(di) = p[1].trim().parse::<f64>() {
                        if di > 5.0 {
                            // 由对角线 + 原生比例推物理宽高，再按当前方向换算。
                            let nh = di / (native_ratio * native_ratio + 1.0).sqrt();
                            let nw = nh * native_ratio;
                            pw = if portrait { nh } else { nw };
                            ph = if portrait { nw } else { nh };
                            src = format!("按你填的 {:.0} 英寸 + 原生比例推算", di);
                        }
                    }
                }
            }
        }
        let signal_px = active_signal_size(&md, &dev);
        infos.push(AlignInfo { manuf, dev, name: d.name.clone(), is_main: d.main, px, signal_px, modes, phys_w: pw, phys_h: ph, src, nat });
    }

    let mut o = String::new();
    o.push_str("双屏对齐体检（只读分析，不会修改任何显示设置）\n");
    if infos.len() < 2 {
        o.push_str("\n只检测到 1 块屏（或读取失败），无法做对齐分析。\n");
        return Ok(o);
    }
    o.push_str("\n【每块屏】\n");
    for i in &infos {
        let portrait = i.px.1 > i.px.0;
        let ppi = if i.phys_h > 0.0 { i.px.1 as f64 / i.phys_h } else { 0.0 };
        o.push_str(&format!(
            "  {} {} · {}x{} · {} · {}\n",
            if i.is_main { "主屏" } else { "副屏" },
            if i.name.trim().is_empty() { i.manuf.as_str() } else { i.name.as_str() },
            i.px.0, i.px.1,
            if portrait { "竖屏" } else { "横屏" },
            i.dev
        ));
        o.push_str(&format!(
            "     活动输出信号 {} · 物理 {} · 画布高 {:.1} 英寸 · 竖向 {:.0} 像素/英寸 · 原生比例 {:.3} · 可用分辨率 {} 种（最大 {}x{}）\n",
            i.signal_px.map(|(w,h)| format!("{}x{}",w,h)).unwrap_or_else(|| "无法读取".to_string()),
            i.src,
            i.phys_h,
            ppi,
            i.nat,
            i.modes.len(),
            i.modes.iter().map(|m| m.0).max().unwrap_or(0),
            i.modes.iter().map(|m| m.1).max().unwrap_or(0)
        ));
    }

    // ---- 现行诊断 ----
    o.push_str("\n【现状诊断】\n");
    let h_max = infos.iter().map(|i| i.px.1).max().unwrap_or(0);
    let ppis: Vec<f64> = infos
        .iter()
        .filter(|i| i.phys_h > 0.0)
        .map(|i| i.px.1 as f64 / i.phys_h)
        .collect();
    let mut bad = 0;
    for i in &infos {
        if i.px.1 < h_max {
            bad += 1;
            o.push_str(&format!(
                "  [!] 竖向像素偏少：{} = {}（虚拟屏高取 {}）→ 跨屏时它**底部会被切掉 {:.0}%** 的画面\n",
                i.name, i.px.1, h_max,
                100.0 * (1.0 - i.px.1 as f64 / h_max as f64)
            ));
        }
    }
    if bad == 0 {
        o.push_str("  [正常] 各屏竖向像素一致 → 跨屏不会丢画面\n");
    }
    let signal_heights: Vec<(String, u32)> = infos.iter().filter_map(|i| {
        i.signal_px.map(|(_, h)| (i.name.clone(), h))
    }).collect();
    if signal_heights.len() >= 2 {
        let min = signal_heights.iter().map(|(_, h)| *h).min().unwrap_or(0);
        let max = signal_heights.iter().map(|(_, h)| *h).max().unwrap_or(0);
        if min != max {
            o.push_str(&format!(
                "  [i] 活动输出信号高度不同（{}）；这是各屏实际扫描时序，不等同 Windows 桌面画布高度，不能单独据此判定黑边或裁切。自动对齐按桌面模式高度与屏幕位置核验\n",
                signal_heights.iter().map(|(name,h)| format!("{} {}px", name, h)).collect::<Vec<_>>().join("；")
            ));
        } else {
            o.push_str(&format!("  [i] 各屏当前活动输出信号高度均为 {}px（仅供诊断）\n", max));
        }
    }
    if ppis.len() >= 2 {
        let pmax = ppis.iter().cloned().fold(0.0f64, f64::max);
        let pmin = ppis.iter().cloned().fold(f64::MAX, f64::min);
        let diff = (pmax / pmin - 1.0) * 100.0;
        if diff > 3.0 {
            o.push_str(&format!(
                "  [!] 像素密度差 {:.0}%（{:.0} vs {:.0} 像素/英寸）→ 同一条横线在两屏的物理高度不同，画面比例对不齐\n",
                diff, pmin, pmax
            ));
        } else {
            o.push_str(&format!("  [正常] 像素密度差仅 {:.1}% → 对齐基本无感\n", diff));
        }
    }
    if infos.iter().any(|i| i.phys_h <= 0.0) {
        o.push_str("  [i] 有屏 EDID 没上报物理尺寸，已尽量用「手动填英寸」兜底；没填就只给出像素层面的结论\n");
    }
    o.push_str("  [i] Windows 的「缩放 100%/150%」不影响这件事：跨屏窗口走的是物理像素，只有「分辨率组合」能改\n");

    // ---- 候选组合：两屏都能取到的共同竖向高度 ----
    o.push_str("\n【推荐组合（按“对得齐”排序）】\n");
    let mut cands: Vec<(u32, Vec<(u32, u32)>, f64, f64, f64)> = Vec::new(); // (H, 每屏模式, 高度误差, 变形, PPI差)
    let mut seen_h: Vec<u32> = Vec::new();
    for (idx, i) in infos.iter().enumerate() {
        if idx > 0 {
            break;
        }
        for m in &i.modes {
            if seen_h.iter().any(|x| (*x as f64 - m.1 as f64).abs() / m.1 as f64 <= 0.02) {
                continue;
            }
            let mut picks: Vec<(u32, u32)> = Vec::new();
            let mut ok = true;
            for j in &infos {
                // 在该屏模式里挑高度最接近 H、其次最接近原生比例、再次面积最大的
                let best = j.modes.iter().cloned().min_by(|a, b| {
                    let ka = ((a.1 as f64 - m.1 as f64).abs(), ((a.0 as f64 / a.1 as f64) - j.nat).abs(), -((a.0 as u64 * a.1 as u64) as f64));
                    let kb = ((b.1 as f64 - m.1 as f64).abs(), ((b.0 as f64 / b.1 as f64) - j.nat).abs(), -((b.0 as u64 * b.1 as u64) as f64));
                    ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
                });
                match best {
                    Some(b) => {
                        if (b.1 as f64 - m.1 as f64).abs() / m.1 as f64 > 0.02 {
                            ok = false;
                        }
                        picks.push(b);
                    }
                    None => ok = false,
                }
            }
            if !ok {
                continue;
            }
            let herr = picks
                .iter()
                .map(|p| (p.1 as f64 - m.1 as f64).abs() / m.1 as f64)
                .fold(0.0f64, f64::max);
            let dist = picks
                .iter()
                .zip(infos.iter())
                .map(|(p, i)| ((p.0 as f64 / p.1 as f64) - i.nat).abs() / i.nat)
                .fold(0.0f64, f64::max);
            let p2: Vec<f64> = picks
                .iter()
                .zip(infos.iter())
                .filter(|(_, i)| i.phys_h > 0.0)
                .map(|(p, i)| p.1 as f64 / i.phys_h)
                .collect();
            let pdiff = if p2.len() >= 2 {
                p2.iter().cloned().fold(0.0f64, f64::max) / p2.iter().cloned().fold(f64::MAX, f64::min) - 1.0
            } else {
                0.0
            };
            seen_h.push(m.1);
            cands.push((m.1, picks, herr, dist, pdiff));
        }
    }
    cands.sort_by(|a, b| {
        let sa = a.3 * 12.0 + a.2 * 3.0 + a.4;
        let sb = b.3 * 12.0 + b.2 * 3.0 + b.4;
        sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
    });
    if cands.is_empty() {
        o.push_str("  两屏的可用分辨率里**没有竖向高度能对上的组合** → 想对齐就必须有一屏用「自定义分辨率」\n");
    }
    for (n, (h, picks, herr, dist, pdiff)) in cands.iter().take(4).enumerate() {
        let mut line = format!("  {}) 共同高度 {}：", (b'A' + n as u8) as char, h);
        for (k, p) in picks.iter().enumerate() {
            line.push_str(&format!("{} {}x{}", if k == 0 { "" } else { " +" }, p.0, p.1));
        }
        o.push_str(&line);
        o.push_str("\n");
        o.push_str(&format!(
            "     高度误差 {:.1}% · 宽高比偏离原生 {:.1}%{} · PPI 差 {:.1}%{}\n",
            herr * 100.0,
            dist * 100.0,
            if *dist > 0.02 { "（会拉伸/留边）" } else { "（不变形）" },
            pdiff * 100.0,
            if *pdiff < 0.03 { " ✅ 对齐良好" } else { "" }
        ));
    }

    // ---- 理想方案：保留最高分辨率，给需要的一屏用自定义分辨率 ----
    o.push_str("\n【理想方案（保留高分辨率）】\n");
    let h_native_max = infos.iter().map(|i| i.px.1.max(i.modes.iter().map(|m| m.1).max().unwrap_or(0))).max().unwrap_or(0);
    for h_try in [h_native_max, 1440u32] {
        if h_try < 600 {
            continue;
        }
        let mut plan: Vec<String> = Vec::new();
        let mut need_custom = false;
        for i in &infos {
            let ideal_w = (((h_try as f64) * i.nat).round() as u32) & !1;
            let exist = i
                .modes
                .iter()
                .any(|m| m.1 == h_try && (m.0 as f64 / ideal_w as f64 - 1.0).abs() < 0.03);
            if exist {
                plan.push(format!("{} {:.0}×{}（已有此模式）", i.name, ideal_w as f64, h_try));
                let _ = ideal_w;
            } else {
                need_custom = true;
                plan.push(format!("{} **{}×{}**（需自定义分辨率）", i.name, ideal_w, h_try));
            }
        }
        o.push_str(&format!("  共同高度 {}：{}\n", h_try, plan.join("　+　")));
        if need_custom {
            o.push_str("     ↳ 自定义分辨率位置：NVIDIA 控制面板 →「更改分辨率 → 自定义」，或 Intel 显卡命令中心 →「显示 → 自定义分辨率」。\n");
            o.push_str("       建议只「添加」、先不应用；设错一般 15 秒内会自动回滚到原模式。\n");
        } else {
            o.push_str("     ↳ 两屏都有现成模式，直接在系统显示设置里选即可，不需要自定义分辨率。\n");
        }
    }
    o.push_str("\n【结论】\n");
    let has_good = cands.iter().any(|c| c.3 < 0.02 && c.4 < 0.03 && c.2 < 0.02);
    if has_good {
        o.push_str("  上面的 A 组就能做到「高度一致 + 不变形 + PPI 接近」→ 在系统显示设置里改成这组分辨率，跨屏画面即可对得齐。\n");
    } else {
        o.push_str("  现有模式组合都会牺牲一点（变形或对齐），**推荐用自定义分辨率**：让两屏竖向像素数相同、各自保持原生比例，即可完全对得齐。\n");
    }
    o.push_str("  提示：两块屏物理高度越接近，越容易对齐；竖向像素数相同是「不丢画面」的关键。\n");
    Ok(o)
}

// ================= 场景档 Profile（P0-5，Windows 侧） =================
// 捕获：模式/旋转走 ModeDetail（M|dev|宽|高|刷新率|方向），EDID 键走 TopologyInfo，
//       亮度/音量/ICC 名复用 get_displays，音频走 audio_endpoints 的选定项。
// 应用：依赖顺序 模式→旋转→HDR→亮度→音量→色彩→音频；每步回读核验；
//       关键步骤（模式/旋转）失败 → 整体回滚到应用前快照（align_apply 的事务模式）。
// 主屏切换 Windows 无可靠 API，捕获时不记录 main_display（macOS 才记）。

/// uid → EDID 键（厂商码字母 + 产品码十六进制，如 "JKA27B3"）。拿不到的 uid 不在表里。
pub fn edid_keys() -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let csp = match core_asm_ref() {
        Ok(p) => p,
        Err(_) => return out,
    };
    let script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8;\nAdd-Type -Path '{cs}';\n$lines = [SGCore]::ListDisplays() -split ([char]10);\nforeach ($l in $lines) {{ $p = $l -split '\\|'; if ($p[0] -eq 'D' -and $p.Count -ge 3) {{ Write-Output ('D|' + $p[1] + '|' + $p[2]) }} }};\n[SGCore]::TopologyInfo()",
        cs = csp
    );
    if let Ok(raw) = ps(&script) {
        let mut uid_by_dev: HashMap<String, String> = HashMap::new();
        let mut edid_by_dev: HashMap<String, (u32, u32)> = HashMap::new();
        for line in raw.lines() {
            let p: Vec<&str> = line.split('|').collect();
            match p[0] {
                "D" if p.len() >= 3 => {
                    uid_by_dev.insert(p[1].to_lowercase(), p[2].to_string());
                }
                "T" if p.len() >= 6 => {
                    let dev = p[1].trim().to_lowercase();
                    let m: u32 = p[3].trim().parse().unwrap_or(0);
                    let pr: u32 = p[4].trim().parse().unwrap_or(0);
                    edid_by_dev.insert(dev, (m, pr));
                }
                _ => {}
            }
        }
        for (dev, uid) in uid_by_dev {
            if let Some((m, pr)) = edid_by_dev.get(&dev) {
                if let Some(l) = edid_manuf_letters(*m) {
                    out.insert(uid, format!("{}{:04X}", l, pr));
                }
            }
        }
    }
    out
}

/// ModeDetail 的 M 行解析：dev(小写) → (宽, 高, 刷新率, 方向)
fn mode_map() -> HashMap<String, (u32, u32, u32, u32)> {
    let mut out = HashMap::new();
    for l in mode_detail().lines() {
        let p: Vec<&str> = l.split('|').collect();
        if p.first() == Some(&"M") && p.len() >= 6 {
            out.insert(
                p[1].to_lowercase(),
                (
                    p[2].trim().parse().unwrap_or(0),
                    p[3].trim().parse().unwrap_or(0),
                    p[4].trim().parse().unwrap_or(0),
                    p[5].trim().parse().unwrap_or(0),
                ),
            );
        }
    }
    out
}

pub fn profile_capture_impl_platform() -> Result<ProfileTargets, String> {
    let ds = get_displays();
    if ds.is_empty() {
        return Err("没有枚举到显示器，无法捕获".to_string());
    }
    let keys = edid_keys();
    let modes = mode_map();
    let mut displays: Vec<DisplayTarget> = Vec::new();
    for d in &ds {
        if !d.connected {
            continue;
        }
        let dev = resolve_dev(&d.id).unwrap_or_default().to_lowercase();
        let (w, h, hz, _ori) = modes.get(&dev).copied().unwrap_or((0, 0, 0, 0));
        // 旋转：mode_map 的 ori 与捕获值一致就存，读不到不存（应用时跳过）
        let ori = modes.get(&dev).map(|m| m.3);
        displays.push(DisplayTarget {
            edid_key: keys.get(&d.id).filter(|k| !k.is_empty()).cloned(),
            captured_id: Some(d.id.clone()),
            resolution: if w > 0 { Some(format!("{}x{}", w, h)) } else { None },
            hz: if hz > 0 { Some(hz) } else { None },
            rotation: ori,
            brightness: d.brightness,
            volume: d.volume,
            color_space: d.color_profile.clone(),
        });
    }
    // HDR：任一屏开着就算「开」；没有任何屏支持 HDR 时不记目标（应用阶段无从核验）
    let hdr_raw = hdr_states();
    let hdr_on = hdr_raw
        .lines()
        .filter(|l| l.starts_with("H|"))
        .any(|l| l.split('|').nth(3) == Some("1"));
    let hdr_capable = hdr_raw.lines().any(|l| l.starts_with("H|"));
    // 音频：当前用户选定的端点
    let audio = audio_endpoints()
        .ok()
        .and_then(|eps| eps.iter().find(|e| e.selected).map(|e| e.id.clone()));
    Ok(ProfileTargets {
        displays,
        main_display: None, // Windows 主屏切换无可靠 API，不捕获（macOS 才记）
        audio_device_id: audio,
        hdr: if hdr_capable { Some(hdr_on) } else { None },
    })
}

/// 读回某 dev 的当前 (w,h,hz,ori)
fn read_mode(dev: &str) -> Option<(u32, u32, u32, u32)> {
    mode_map().get(&dev.to_lowercase()).copied()
}

fn set_mode_call(dev: &str, w: u32, h: u32, hz: u32) -> Result<(), String> {
    let out = ps_core(&format!(
        "[SGCore]::SetMode('{}',[uint32]{},[uint32]{},[uint32]{})",
        dev, w, h, hz
    ))?;
    let t = out.trim();
    if t.starts_with("ERR:") {
        Err(t[4..].to_string())
    } else if t != "OK" {
        Err(if t.is_empty() { "系统未响应模式切换".to_string() } else { t.to_string() })
    } else {
        Ok(())
    }
}

fn rotate_call(dev: &str, ori: u32) -> Result<(), String> {
    let out = ps_core(&format!("[SGCore]::Rotate('{}',[uint32]{})", dev, ori))?;
    let t = out.trim();
    if t.starts_with("ERR:") {
        Err(t[4..].to_string())
    } else if t != "OK" {
        Err(if t.is_empty() { "系统未响应旋转".to_string() } else { t.to_string() })
    } else {
        Ok(())
    }
}

pub fn profile_apply_impl_platform(profile: &Profile) -> Result<ApplyReport, String> {
    mark_change();
    let mut steps: Vec<ApplyStep> = Vec::new();
    let ds = get_displays();
    if ds.is_empty() {
        return Err("没有枚举到显示器，无法应用".to_string());
    }
    let keys = edid_keys();

    // 回滚快照：每块屏应用前的模式（关键步骤失败时恢复）
    let snapshot: Vec<(String, u32, u32, u32, u32)> = mode_map()
        .into_iter()
        .map(|(dev, (w, h, hz, ori))| (dev, w, h, hz, ori))
        .collect();

    // 候选池：connected 且能解析到设备的 (uid, dev)；EDID 命中或顺序兜底都从这里取
    let mut pool: Vec<(String, String)> = ds
        .iter()
        .filter(|d| d.connected)
        .filter_map(|d| resolve_dev(&d.id).ok().map(|dev| (d.id.clone(), dev)))
        .collect();

    let mut critical_failed = false;
    let mut color_done = false;

    for t in &profile.targets.displays {
        // 选屏：EDID 键命中优先；否则顺序兜底（并如实标注）
        let chosen = t
            .edid_key
            .as_ref()
            .and_then(|k| {
                pool.iter()
                    .position(|(uid, _)| keys.get(uid).map(|kk| kk == k).unwrap_or(false))
                    .map(|pos| pool.remove(pos))
            })
            .or_else(|| if pool.is_empty() { None } else { Some(pool.remove(0)) });
        let (uid, dev) = match chosen {
            Some(v) => v,
            None => {
                steps.push(ApplyStep {
                    target: t.captured_id.clone().unwrap_or_else(|| "?".into()),
                    field: "选屏".into(),
                    want: t.edid_key.clone().unwrap_or_else(|| "顺序下一个".into()),
                    got: "没有可用的显示器".into(),
                    ok: false,
                    error: Some("这块屏当前不在线（按 EDID 与顺序都没匹配到）".into()),
                });
                critical_failed = true;
                continue;
            }
        };
        let target_name = dname(&ds, &uid);
        let fallback = t
            .edid_key
            .as_ref()
            .map(|k| keys.get(&uid).map(|kk| kk != k).unwrap_or(true))
            .unwrap_or(true);

        // ① 模式（关键步骤）
        if let Some(res) = &t.resolution {
            let (tw, th) = match res.split_once('x') {
                Some((w, h)) => (
                    w.trim().parse::<u32>().unwrap_or(0),
                    h.trim().parse::<u32>().unwrap_or(0),
                ),
                None => (0, 0),
            };
            let thz = t.hz.unwrap_or(0);
            if tw > 0 && th > 0 {
                let mut got = String::new();
                let mut err = None;
                let mut ok = false;
                match set_mode_call(&dev, tw, th, thz) {
                    Ok(()) => {
                        std::thread::sleep(std::time::Duration::from_millis(900));
                        match read_mode(&dev) {
                            Some((w, h, hz, _)) => {
                                got = format!("{}x{}@{}", w, h, hz);
                                ok = w == tw && h == th && (thz == 0 || hz == thz);
                                if !ok {
                                    err = Some("回读与目标不一致".to_string());
                                }
                            }
                            None => {
                                err = Some("应用后读不到当前模式".to_string());
                            }
                        }
                    }
                    Err(e) => err = Some(e),
                }
                if !ok {
                    critical_failed = true;
                }
                steps.push(ApplyStep {
                    target: format!("{}{}", target_name, if fallback { "（顺序兜底）" } else { "" }),
                    field: "分辨率".into(),
                    want: format!("{}x{}@{}", tw, th, if thz > 0 { thz.to_string() } else { "?".into() }),
                    got: if got.is_empty() { "未读取".into() } else { got },
                    ok,
                    error: err,
                });
            }
        }
        // ② 旋转（关键步骤）
        if let Some(ori) = t.rotation {
            let mut got = String::new();
            let mut err = None;
            let mut ok = false;
            match rotate_call(&dev, ori) {
                Ok(()) => {
                    std::thread::sleep(std::time::Duration::from_millis(900));
                    match read_mode(&dev) {
                        Some((_, _, _, cur)) => {
                            got = format!("方向 {}", cur);
                            ok = cur == ori;
                            if !ok {
                                err = Some("回读方向与目标不一致".to_string());
                            }
                        }
                        None => err = Some("应用后读不到当前方向".to_string()),
                    }
                }
                Err(e) => err = Some(e),
            }
            if !ok {
                critical_failed = true;
            }
            steps.push(ApplyStep {
                target: target_name.clone(),
                field: "旋转".into(),
                want: format!("方向 {}", ori),
                got: if got.is_empty() { "未读取".into() } else { got },
                ok,
                error: err,
            });
        }
        // ③ 亮度（非关键：DDC 不支持时如实报告，不回滚）
        if let Some(bri) = t.brightness {
            let mut err = None;
            let mut ok = false;
            let mut got = "未读取".to_string();
            match set_brightness(&uid, bri) {
                Ok(()) => {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let cur = get_displays()
                        .into_iter()
                        .find(|d| d.id == uid)
                        .and_then(|d| d.brightness);
                    match cur {
                        Some(b) => {
                            got = format!("{}%", b);
                            ok = b == bri;
                            if !ok {
                                err = Some("回读与目标不一致".to_string());
                            }
                        }
                        None => err = Some("应用后读不到亮度".to_string()),
                    }
                }
                Err(e) => err = Some(e),
            }
            steps.push(ApplyStep {
                target: target_name.clone(),
                field: "亮度".into(),
                want: format!("{}%", bri),
                got,
                ok,
                error: err,
            });
        }
        // ④ 音量（非关键）
        if let Some(vol) = t.volume {
            let mut err = None;
            let mut ok = false;
            let mut got = "未读取".to_string();
            match set_volume(&uid, vol) {
                Ok(()) => {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let cur = get_displays()
                        .into_iter()
                        .find(|d| d.id == uid)
                        .and_then(|d| d.volume);
                    match cur {
                        Some(v) => {
                            got = format!("{}%", v);
                            ok = v == vol;
                            if !ok {
                                err = Some("回读与目标不一致".to_string());
                            }
                        }
                        None => err = Some("应用后读不到音量".to_string()),
                    }
                }
                Err(e) => err = Some(e),
            }
            steps.push(ApplyStep {
                target: target_name.clone(),
                field: "音量".into(),
                want: format!("{}%", vol),
                got,
                ok,
                error: err,
            });
        }
        // ⑤ 色彩配置（非关键，全局动作只做一次，逐屏回读）
        if let Some(space) = &t.color_space {
            if !color_done {
                color_done = true;
                let mut err = None;
                let mut ok = false;
                let mut got = "未读取".to_string();
                match apply_color_space(space) {
                    Ok(()) => {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        match resolve_dev(&uid).ok().and_then(|d| icc_get(&d)) {
                            Some(cur) => {
                                // icc_get 可能返回完整路径；比较与展示都取文件名段
                                let name = cur.rsplit(['\\', '/']).next().unwrap_or(&cur).to_string();
                                got = name.clone();
                                ok = name.eq_ignore_ascii_case(space);
                                if !ok {
                                    err = Some("回读的 ICC 名与目标不一致".to_string());
                                }
                            }
                            None => err = Some("应用后读不到当前 ICC 配置".to_string()),
                        }
                    }
                    Err(e) => err = Some(e),
                }
                steps.push(ApplyStep {
                    target: target_name.clone(),
                    field: "色彩配置".into(),
                    want: space.clone(),
                    got,
                    ok,
                    error: err,
                });
            }
        }
    }

    // ⑥ HDR（非关键，全局）
    if let Some(on) = profile.targets.hdr {
        {
            let mut err = None;
            let mut ok = false;
            let mut got = String::new();
            match hdr_set(on) {
                Ok(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(700));
                    let rows: Vec<(bool, bool)> = hdr_states()
                        .lines()
                        .filter(|l| l.starts_with("H|"))
                        .filter_map(|l| {
                            let p: Vec<&str> = l.split('|').collect();
                            if p.len() >= 4 {
                                Some((p[2] == "1", p[3] == "1"))
                            } else {
                                None
                            }
                        })
                        .collect();
                    got = format!(
                        "{} 台屏：{}",
                        rows.len(),
                        rows.iter().map(|(_, on)| if *on { "开" } else { "关" }).collect::<Vec<_>>().join("/")
                    );
                    // 有屏不支持 HDR 时不能一票否决：只看支持 HDR 的屏
                    let capable: Vec<bool> = rows.iter().filter(|(sup, _)| *sup).map(|(_, on)| *on).collect();
                    ok = if capable.is_empty() {
                        !on // 没有任何屏支持 HDR：目标为关算成功，目标为开算失败
                    } else {
                        capable.iter().any(|b| *b == on)
                    };
                    if !ok {
                        err = Some("支持 HDR 的屏回读与目标不一致".to_string());
                    }
                }
                Err(e) => {
                    if e.contains("没有支持 HDR") {
                        // 本机没有 HDR 屏：目标为关 = 状态本来就是关，算成功；目标为开才算失败
                        got = "本机没有支持 HDR 的显示器".into();
                        ok = !on;
                        if !ok {
                            err = Some("没有任何屏支持 HDR，无法开启".to_string());
                        }
                    } else {
                        err = Some(e);
                    }
                }
            }
            steps.push(ApplyStep {
                target: "全局".into(),
                field: "HDR".into(),
                want: if on { "开".into() } else { "关".into() },
                got: if got.is_empty() { "未读取".into() } else { got },
                ok,
                error: err,
            });
        }
    }

    // ⑦ 音频设备（非关键）
    if let Some(aid) = &profile.targets.audio_device_id {
        {
            let mut err = None;
            let mut ok = false;
            let mut got = "未读取".to_string();
            match set_audio_device(aid) {
                Ok(()) => {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    match audio_endpoints() {
                        Ok(eps) => match eps.iter().find(|e| e.selected) {
                            Some(e) => {
                                got = e.name.clone();
                                ok = e.id == *aid;
                                if !ok {
                                    err = Some("回读的选定设备与目标不一致".to_string());
                                }
                            }
                            None => err = Some("读不到当前选定的音频设备".to_string()),
                        },
                        Err(e) => err = Some(e),
                    }
                }
                Err(e) => err = Some(e),
            }
            steps.push(ApplyStep {
                target: "全局".into(),
                field: "音频设备".into(),
                want: aid.clone(),
                got,
                ok,
                error: err,
            });
        }
    }

    // 关键步骤失败 → 整体回滚到应用前快照
    if critical_failed {
        let mut rollback_ok = true;
        for (dev, w, h, hz, ori) in &snapshot {
            if *w > 0 && *h > 0 {
                if set_mode_call(dev, *w, *h, *hz).is_err() {
                    rollback_ok = false;
                }
            }
            if *ori != 0 && rotate_call(dev, *ori).is_err() {
                rollback_ok = false;
            }
        }
        mark_change();
        steps.push(ApplyStep {
            target: "整体".into(),
            field: "回滚".into(),
            want: "应用前的模式与方向".into(),
            got: if rollback_ok { "已回滚".into() } else { "部分未恢复".into() },
            ok: rollback_ok,
            error: if rollback_ok { None } else { Some("个别屏未能恢复，请在系统显示设置里检查".into()) },
        });
        return Ok(ApplyReport { ok: false, rolled_back: true, steps });
    }

    let ok = steps.iter().all(|s| s.ok);
    Ok(ApplyReport { ok, rolled_back: false, steps })
}

/// uid → 友好名（找不到就用 uid 尾段）
fn dname(ds: &[DisplayInfo], uid: &str) -> String {
    ds.iter()
        .find(|d| d.id == uid)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| short_id(uid))
}

#[cfg(test)]
mod powershell_literal_tests {
    use super::ps_literal;

    #[test]
    fn escapes_apostrophes_in_windows_user_and_profile_paths() {
        assert_eq!(
            ps_literal(r"C:\Users\O'Brien\AppData\Local\Screenguard\icc-reference\Display.icc"),
            r"'C:\Users\O''Brien\AppData\Local\Screenguard\icc-reference\Display.icc'"
        );
    }
}

#[cfg(test)]
mod health_tests {
    use super::*;

    /// 手动冒烟（需真实显示器环境）：cargo test diag_scan_smoke -- --ignored
    /// 走一遍完整 diag_scan（内部已改为调用 health_report_impl），验证显示器小节来自结构化体检
    #[test]
    #[ignore]
    fn diag_scan_smoke() {
        let out = diag_scan().expect("diag_scan 失败");
        assert!(out.contains("【1】显示器"), "缺少显示器小节");
        assert!(out.contains("===== 结论 ====="), "缺少结论");
        println!("{}", out);
    }

    /// 手动冒烟：cargo test health_report_smoke -- --ignored
    /// 验证结构化体检命令本体：至少一块屏、summary 已聚合、每条检查带 evidence
    #[test]
    #[ignore]
    fn health_report_smoke() {
        let r = health_report_impl().expect("health_report_impl 失败");
        assert!(!r.displays.is_empty(), "没有显示器条目");
        assert!(!r.summary.headline.is_empty(), "summary 未聚合");
        for d in &r.displays {
            for c in &d.checks {
                assert!(!c.evidence.is_empty(), "检查项 {} 缺少 evidence", c.id);
            }
        }
        println!("headline: {}", r.summary.headline);
        for d in &r.displays {
            println!(
                "- {} · {} · checks={}",
                d.name,
                d.current_resolution,
                d.checks.len()
            );
        }
    }

    /// 手动冒烟：cargo test topology_smoke -- --ignored
    /// 验证链路拓扑（P0-2）：在用屏有 5 跳拓扑，接口/线缆不出现瞎猜的结论
    #[test]
    #[ignore]
    fn topology_smoke() {
        let r = health_report_impl().expect("health_report_impl 失败");
        for d in &r.displays {
            println!("== {} (connected={})", d.name, d.connected);
            match &d.topology {
                Some(t) => {
                    assert_eq!(t.hops.len(), 5, "拓扑应为 5 跳：{}", t.hops.len());
                    for h in &t.hops {
                        println!(
                            "  {:?} | {} | {:?} | {}",
                            h.kind,
                            h.label,
                            h.status,
                            h.detail.clone().unwrap_or_default()
                        );
                    }
                    if let Some(n) = &t.note {
                        println!("  note: {}", n);
                    }
                }
                None => println!("  (无拓扑：未在活动路径上)"),
            }
        }
    }

    /// 手动冒烟：cargo test health_md_smoke -- --ignored
    /// 真机验证 Markdown 导出（P0-3）：默认脱敏——无路径反斜杠残留、无序列号、无用户名
    #[test]
    #[ignore]
    fn health_md_smoke() {
        let md = crate::platform::health_report_markdown_impl(false).expect("导出失败");
        assert!(
            md.starts_with("# 屏幕连接助手 · 显示体检报告"),
            "标题不符"
        );
        assert!(
            !md.contains('\\'),
            "默认导出不应残留任何反斜杠路径"
        );
        assert!(md.contains("未输出（不含设备序列号）"));
        assert!(md.contains("由 屏幕连接助手 v"));
        println!("{}", md);
    }

    /// 手动冒烟：cargo test repair_smoke -- --ignored --nocapture
    /// 真机验证修复动作集（P0-4）：结构完整（action/message 非空、result 合法）、
    /// 非法参数被拒（not_applicable）、安全动作真实执行。
    #[test]
    #[ignore]
    fn repair_smoke() {
        use super::{RepairResult, repair_action_impl};

        // ① 未知名动作 → not_applicable 且原因可读
        let o = repair_action_impl("no_such_action", None, None).expect("未知动作应返回 Ok");
        assert_eq!(o.result, RepairResult::NotApplicable);
        assert!(!o.message.is_empty());
        println!("① 未知名动作: {:?}", o.message);

        // ② 需要指定屏的动作不给 display_id → not_applicable
        for a in ["reapply_mode", "switch_input", "ddc_power_cycle", "reopen_ddc_channel"] {
            let o = repair_action_impl(a, None, None).expect("缺参应返回 Ok");
            assert_eq!(o.result, RepairResult::NotApplicable, "动作 {} 缺参未被拒", a);
        }
        println!("② 缺 display_id 的 4 个动作全部被拒（附人话原因）");

        // ③ 非法输入源值 → not_applicable
        let o = repair_action_impl("switch_input", Some("dev-1"), Some("vga")).expect("非法值应返回 Ok");
        assert_eq!(o.result, RepairResult::NotApplicable);
        println!("③ 非法输入源: {:?}", o.message);

        // ④ 重新枚举（真机执行）：应 applied 且消息可读
        let o = repair_action_impl("rescan_displays", None, None).expect("rescan 失败");
        assert_eq!(o.result, RepairResult::Applied, "rescan: {}", o.message);
        assert!(!o.message.is_empty());
        println!("④ rescan_displays: {}", o.message);

        // ⑤ 强制重识别（真机执行）：应 applied
        let o = repair_action_impl("force_redetect", None, None).expect("redetect 失败");
        assert_eq!(o.result, RepairResult::Applied, "redetect: {}", o.message);
        println!("⑤ force_redetect: {}", o.message);

        // ⑥ 对真实第一块屏重建 DDC 通道：无论结果如何，消息必须可读（失败路径验收）
        let displays = get_displays();
        assert!(!displays.is_empty(), "没有显示器");
        let uid = displays[0].id.clone();
        let o = repair_action_impl("reopen_ddc_channel", Some(&uid), None).expect("reopen 失败");
        assert!(!o.message.is_empty());
        println!("⑥ reopen_ddc_channel({}): {:?} — {}", uid, o.result, o.message);
        println!("   changed: {:?}", o.changed);
    }

    /// 手动冒烟：cargo test profile_smoke -- --ignored --nocapture
    /// 真机验证场景档（P0-5）：捕获当前状态 → 立即应用同一档 → 应全绿不回滚
    /// （应用的就是当前状态本身，任何回读不一致都说明核验或应用通道有 bug）
    #[test]
    #[ignore]
    fn profile_smoke() {
        let targets = profile_capture_impl_platform().expect("捕获失败");
        println!("捕获到 {} 块屏：", targets.displays.len());
        for t in &targets.displays {
            println!(
                "  {:?} | edid={:?} | res={:?}@{:?} | ori={:?} | bri={:?} | color={:?}",
                t.captured_id, t.edid_key, t.resolution, t.hz, t.rotation, t.brightness, t.color_space
            );
        }
        println!("audio={:?} hdr={:?}", targets.audio_device_id, targets.hdr);
        let profile = Profile {
            id: "smoke".into(),
            name: "冒烟测试".into(),
            created_at: String::new(),
            updated_at: String::new(),
            targets,
            match_: None,
            auto: false,
        };
        let r = profile_apply_impl_platform(&profile).expect("应用失败");
        for s in &r.steps {
            println!(
                "{} {} | want={} got={} ok={} {}",
                s.target,
                s.field,
                s.want,
                s.got,
                s.ok,
                s.error.clone().unwrap_or_default()
            );
        }
        assert!(!r.rolled_back, "应用当前状态自身不应触发回滚");
        assert!(r.ok, "应用当前状态自身的档应全绿");
    }

    /// 手动冒烟：cargo test profiles_corrupt_smoke -- --ignored --nocapture
    /// 验证 profiles.json 损坏时：不崩、返回空列表、原文件保留为 .bak
    #[test]
    #[ignore]
    fn profiles_corrupt_smoke() {
        use crate::platform::{load_profiles, profiles_file};
        let path = profiles_file();
        // 先备份用户已有数据（如果有）
        let user_backup = path.with_extension("json.user-bak");
        let had_user_data = path.exists() && std::fs::copy(&path, &user_backup).is_ok();
        // 写入损坏内容
        std::fs::write(&path, "{{{ 不是合法 JSON").expect("写入损坏文件失败");
        let list = load_profiles();
        assert!(list.is_empty(), "损坏文件应返回空列表");
        let bak = path.with_extension("json.bak");
        assert!(bak.exists(), "损坏文件应被改名为 .bak 保留");
        println!(".bak 已生成：{}", bak.display());
        // 清理演练痕迹：恢复用户数据或删掉测试产物
        let _ = std::fs::remove_file(&bak);
        let _ = std::fs::remove_file(&path);
        if had_user_data {
            std::fs::rename(&user_backup, &path).ok();
        }
    }

    /// 手动冒烟：cargo test auto_smoke -- --ignored --nocapture
    /// 验证 P1-1 轮询原语真机可用 + 配置读写/暂停 roundtrip（用户数据保护同上）
    #[test]
    #[ignore]
    fn auto_smoke() {
        use crate::platform::{auto_pause_impl, auto_rules_get_impl, auto_rules_set_impl};
        // ① 原生指纹：非空、含设备名、两次读取稳定、且必须快（<50ms）
        let t0 = std::time::Instant::now();
        let fp1 = auto_display_fingerprint();
        let dt = t0.elapsed();
        println!("指纹（{}ms）：{:?}", dt.as_millis(), fp1.lines().collect::<Vec<_>>());
        assert!(!fp1.is_empty(), "原生 EnumDisplayDevicesW 应至少读到 1 块活动屏");
        assert!(fp1.contains("DISPLAY"), "指纹应含设备名");
        assert!(dt.as_millis() < 200, "轮询原语必须便宜（实际 {}ms）", dt.as_millis());
        let fp2 = auto_display_fingerprint();
        assert_eq!(fp1, fp2, "无插拔时指纹应稳定");

        // ② 电源状态：打印（笔记本 Some(true/false)，拿不到就是 None，都不算失败）
        let ac = auto_on_ac();
        println!("接通电源：{:?}", ac);

        // ③ 本地时刻：应等于系统当前时间（HHMM）
        let hhmm = auto_local_hhmm().expect("GetLocalTime 应成功");
        println!("本地时刻 HHMM：{}", hhmm);
        assert!(hhmm < 2400);

        // ④ 配置读写 roundtrip：auto_pause 暂停/恢复 + last_applied 保留
        let user_backup = std::path::PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
            .join("Screenguard")
            .join("auto.json");
        let had_user = user_backup.exists() && std::fs::copy(&user_backup, user_backup.with_extension("json.user-bak")).is_ok();
        let mut cfg = auto_rules_get_impl();
        let keep = cfg.last_applied.clone();
        cfg.enabled = true;
        cfg.sources.displays_changed = true;
        cfg.last_applied = Some(crate::platform::AutoLastApplied {
            profile_id: "smoke".into(),
            profile_name: "冒烟".into(),
            at: String::new(),
            reason: "测试".into(),
            ok: true,
        });
        // last_applied 由引擎负责（auto_rules_set 会保留磁盘旧值），这里直写文件再演练暂停
        crate::platform::auto_cfg_save(&cfg).expect("保存配置失败");
        auto_rules_set_impl(cfg.clone()).expect("set 不应失败");
        let via_set = auto_rules_get_impl();
        assert!(via_set.last_applied.is_some(), "auto_rules_set 应保留磁盘上的 last_applied");
        let paused = auto_pause_impl(1).expect("暂停失败");
        assert!(paused.paused_until_epoch.is_some(), "暂停后应有截止时间");
        let resumed = auto_pause_impl(0).expect("恢复失败");
        assert!(resumed.paused_until_epoch.is_none(), "恢复后应无截止时间");
        assert!(resumed.enabled, "auto_pause 不应动总开关");
        assert!(resumed.last_applied.is_some(), "last_applied 应保留");
        assert_eq!(resumed.last_applied.unwrap().profile_id, "smoke");
        // 还原用户数据
        if had_user {
            std::fs::rename(user_backup.with_extension("json.user-bak"), &user_backup).ok();
        } else {
            let _ = std::fs::remove_file(&user_backup);
        }
        let _ = keep;
        println!("auto_smoke 全部通过");
    }

    #[test]
    #[ignore]
    fn kvm_smoke() {
        // ① 剪贴板 roundtrip（动真实剪贴板：先备份、测完恢复）
        let saved = clipboard_get_text();
        clipboard_set_text("SG-KVM-测试 12345 ✓").expect("写剪贴板失败");
        assert_eq!(clipboard_get_text().as_deref(), Some("SG-KVM-测试 12345 ✓"));
        match saved {
            Some(t) => {
                clipboard_set_text(&t).ok();
            }
            None => {
                clipboard_set_text(" ").ok();
            }
        }
        // ② 音频端点枚举 + AudioSetDefault 幂等调用（切回当前默认端点，不产生变化）
        let eps = audio_endpoints().expect("音频端点枚举失败");
        if let Some(def) = eps.iter().find(|e| e.is_default) {
            audio_set_default(&def.id).expect("IPolicyConfig SetDefaultEndpoint 调用失败");
            println!("audio_set_default 幂等调用 OK：{}", def.name);
        } else {
            println!("警告：没有 is_default 端点，跳过音频验证");
        }
        // ③ 设备名非空
        assert!(!device_name().is_empty());
        println!("kvm_smoke 全部通过（设备名：{}）", device_name());
    }
}

// ===== 场景自动触发轮询原语（P1-1）=====
// 约束：这几条在 2 秒轮询里跑，必须走原生 API（绝不许走 PS 桥 —— 单次几百毫秒）。

const DISPLAY_DEVICE_ACTIVE: u32 = 0x0000_0001;
const EDD_GET_DEVICE_INTERFACE_NAME: u32 = 0x0000_0001;

#[repr(C)]
struct AutoDisplayDeviceW {
    cb: u32,
    device_name: [u16; 32],
    device_string: [u16; 128],
    state_flags: u32,
    device_id: [u16; 128],
    device_key: [u16; 128],
}

impl AutoDisplayDeviceW {
    fn zeroed() -> Self {
        AutoDisplayDeviceW {
            cb: std::mem::size_of::<AutoDisplayDeviceW>() as u32,
            device_name: [0; 32],
            device_string: [0; 128],
            state_flags: 0,
            device_id: [0; 128],
            device_key: [0; 128],
        }
    }

    fn text(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }
}

extern "system" {
    fn EnumDisplayDevicesW(dev: *const u16, num: u32, out: *mut AutoDisplayDeviceW, flags: u32) -> i32;
    fn GetSystemPowerStatus(s: *mut AutoPowerStatus) -> i32;
    fn GetLocalTime(t: *mut AutoSystemTime) -> i32;
}

#[repr(C)]
struct AutoPowerStatus {
    ac_line: u8,
    battery_flag: u8,
    battery_percent: u8,
    reserved: u8,
    battery_seconds: u32,
    battery_full_seconds: u32,
}

#[repr(C)]
struct AutoSystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millis: u16,
}

/// 在用显示器集合指纹（设备名 + 设备接口路径）。原生 EnumDisplayDevicesW，
/// 微秒级；路径对「同一块屏插同一个口」稳定，插拔/换口会变。
pub fn auto_display_fingerprint() -> String {
    let mut out: Vec<String> = Vec::new();
    for i in 0..64u32 {
        let mut dd = AutoDisplayDeviceW::zeroed();
        if unsafe { EnumDisplayDevicesW(std::ptr::null(), i, &mut dd, 0) } == 0 {
            break;
        }
        if dd.state_flags & DISPLAY_DEVICE_ACTIVE == 0 {
            continue;
        }
        let name = AutoDisplayDeviceW::text(&dd.device_name);
        if name.is_empty() {
            continue;
        }
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut dd2 = AutoDisplayDeviceW::zeroed();
        let ok = unsafe {
            EnumDisplayDevicesW(wide.as_ptr(), 0, &mut dd2, EDD_GET_DEVICE_INTERFACE_NAME)
        };
        let path = if ok == 1 { AutoDisplayDeviceW::text(&dd2.device_id) } else { String::new() };
        out.push(format!("{}|{}", name, path));
    }
    out.sort();
    out.join("\n")
}

/// 是否接通交流电：Some(是/否)；None = 系统报不上来（台式机通常恒 Some(true)）
pub fn auto_on_ac() -> Option<bool> {
    let mut s = AutoPowerStatus {
        ac_line: 255,
        battery_flag: 0,
        battery_percent: 0,
        reserved: 0,
        battery_seconds: 0,
        battery_full_seconds: 0,
    };
    if unsafe { GetSystemPowerStatus(&mut s) } == 0 {
        return None;
    }
    match s.ac_line {
        1 => Some(true),
        0 => Some(false),
        _ => None,
    }
}

/// 本地时刻 HH*100+MM（时间段触发用）
pub fn auto_local_hhmm() -> Option<u32> {
    let mut t = AutoSystemTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        millis: 0,
    };
    if unsafe { GetLocalTime(&mut t) } == 0 {
        return None;
    }
    Some(u32::from(t.hour) * 100 + u32::from(t.minute))
}

/// 当前屏幕的匹配键集合：EDID 键（JKA27B3）优先，拿不到用显示器 uid。
/// 只许在触发评估时调用（edid_keys 走 PS 桥，贵；轮询里严禁）。
pub fn auto_current_keys() -> Vec<String> {
    let edids = edid_keys();
    let mut out: Vec<String> = Vec::new();
    for (uid, key) in &edids {
        out.push(key.clone());
        out.push(uid.clone()); // captured_id 存的是 uid，一并放进候选集兜底
    }
    out.sort();
    out.dedup();
    out
}
