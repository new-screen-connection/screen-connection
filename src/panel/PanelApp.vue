<script setup lang="ts">
/**
 * 托盘快捷面板（无边框小窗 380x568）。
 * 定位：高频单手操作——亮度、音量、快捷动作、屏幕电源。
 * 复杂配置仍放主窗口，这里只保留"一眼可达"的能力。
 */
import { computed, nextTick, onMounted, ref } from 'vue';
import { invoke, isTauri, listenEvent, errText } from '../api/tauri';
import { sendVCP } from '../api/vcp';
import type { DisplayInfo, SystemAudio } from '../types';
import ChatPanel from '../components/ChatPanel.vue';
import appIcon from '../assets/app-icon.png';

interface PowerRow {
  id: string;
  name: string;
  mode: number | null;
  pending: boolean;
}

const version = ref('');
const status = ref('');
const statusOk = ref(true);
const statusTimer = ref<ReturnType<typeof setTimeout> | null>(null);

const displays = ref<DisplayInfo[]>([]);
const briVals = ref<Record<string, number>>({});
const powerRows = ref<PowerRow[]>([]);

const audio = ref<SystemAudio | null>(null);
const volume = ref(0);
const muted = ref(false);

const hdrOn = ref(false);
const hdrBusy = ref(false);
const powerLink = ref(false);
const autostart = ref(false);
const busy = ref('');

let statusSeq = 0;

function flash(msg: string, ok = true): void {
  statusSeq += 1;
  const seq = statusSeq;
  status.value = msg;
  statusOk.value = ok;
  if (statusTimer.value) clearTimeout(statusTimer.value);
  statusTimer.value = setTimeout(() => {
    if (seq === statusSeq) status.value = '';
  }, 2600);
}

/** 统一动作包装：成功闪提示，失败闪错误 */
async function act(fn: () => Promise<unknown>, okMsg?: string): Promise<boolean> {
  busy.value = 'run';
  try {
    const r = await fn();
    if (okMsg) flash(okMsg);
    if (typeof r === 'string' && r) flash(r);
    return true;
  } catch (e) {
    flash(errText(e), false);
    return false;
  } finally {
    busy.value = '';
  }
}

const ddcDisplays = computed(() => displays.value.filter((d) => d.ddc && d.brightness !== null));

async function loadDisplays(): Promise<void> {
  try {
    const ds = await invoke<DisplayInfo[]>('get_displays');
    displays.value = ds ?? [];
    const vals: Record<string, number> = {};
    for (const d of displays.value) vals[d.id] = d.brightness ?? 50;
    briVals.value = vals;
    const rows = await Promise.all(
      displays.value.map(async (d): Promise<PowerRow> => {
        try {
          const m = await invoke<number>('get_display_power', { displayId: d.id });
          return { id: d.id, name: d.name, mode: m, pending: false };
        } catch {
          return { id: d.id, name: d.name, mode: null, pending: false };
        }
      }),
    );
    powerRows.value = rows;
  } catch (e) {
    flash(errText(e), false);
  }
}

function commitBrightness(d: DisplayInfo): void {
  // DDC 写入必须串行合并（旧版经验：并发写会卡死 PowerShell 桥）
  sendVCP('brightness', d.id, briVals.value[d.id] ?? 50, (cmd, args) => invoke(cmd, args));
}

async function commitAllBrightness(): Promise<void> {
  const v = Number(allBrightness.value);
  for (const d of ddcDisplays.value) {
    briVals.value[d.id] = v;
    sendVCP('brightness', d.id, v, (cmd, args) => invoke(cmd, args));
  }
  flash(`已把全部屏幕亮度调整到 ${v}%`);
}

const allBrightness = computed<number>({
  get: () => {
    const ds = ddcDisplays.value;
    if (!ds.length) return 50;
    const sum = ds.reduce((acc, d) => acc + (briVals.value[d.id] ?? 50), 0);
    return Math.round(sum / ds.length);
  },
  set: () => {
    /* 由 commitAllBrightness 统一下发 */
  },
});

/** 切换单屏电源；点亮后 DDC 通道需 2~5 秒恢复（旧版实测），延迟回读避免读到瞬态 */
async function togglePower(row: PowerRow, on: boolean): Promise<void> {
  const mode = on ? 1 : 4;
  row.pending = true;
  try {
    await invoke('set_display_power', { displayId: row.id, mode });
    flash(on ? `已点亮「${row.name}」` : `已关闭「${row.name}」`);
    const wait = on ? 4000 : 1200;
    setTimeout(async () => {
      try {
        row.mode = await invoke<number>('get_display_power', { displayId: row.id });
      } catch {
        row.mode = null;
      }
      row.pending = false;
    }, wait);
  } catch (e) {
    row.pending = false;
    flash(errText(e), false);
  }
}

async function toggleHdr(): Promise<void> {
  hdrBusy.value = true;
  const turningOn = !hdrOn.value;
  await act(
    async () => {
      await invoke('hdr_set', { on: turningOn });
      await refreshHdr();
    },
    turningOn ? '已开启全部屏幕 HDR' : '已关闭全部屏幕 HDR',
  );
  hdrBusy.value = false;
}

async function refreshHdr(): Promise<void> {
  try {
    const s = await invoke<string>('hdr_states');
    hdrOn.value = /:on/i.test(s) || /:1/.test(s);
  } catch {
    hdrOn.value = false;
  }
}

async function refreshAudio(): Promise<void> {
  try {
    audio.value = await invoke<SystemAudio>('get_system_audio');
    volume.value = audio.value?.volume ?? 0;
    muted.value = audio.value?.mute ?? false;
  } catch {
    audio.value = null;
  }
}

function commitVolume(): void {
  void act(() => invoke('set_system_volume', { value: volume.value }));
}

async function toggleMute(): Promise<void> {
  muted.value = !muted.value;
  await act(() => invoke('set_system_mute', { on: muted.value }), muted.value ? '已静音' : '已取消静音');
}

async function togglePowerLink(): Promise<void> {
  powerLink.value = !powerLink.value;
  await act(
    () => invoke('set_power_link_preference', { enabled: powerLink.value }),
    powerLink.value ? '电源联动已开启' : '电源联动已关闭',
  );
}

async function toggleAutostart(): Promise<void> {
  autostart.value = !autostart.value;
  await act(
    () => invoke('set_autostart', { enabled: autostart.value }),
    autostart.value ? '已开启开机自启' : '已关闭开机自启',
  );
}

const quickActions = computed(() => [
  { key: 'span', label: '铺满窗口', icon: 'layout', run: () => act(() => invoke('span_foreground')) },
  { key: 'restore', label: '还原窗口', icon: 'rotate', run: () => act(() => invoke('restore_foreground')) },
  { key: 'wake', label: '唤醒屏幕', icon: 'sun', run: () => act(() => invoke('screen_wake'), '已唤醒屏幕') },
  { key: 'off', label: '全部熄屏', icon: 'moon', run: () => act(() => invoke('screen_off'), '已全部熄屏') },
  { key: 'rotate', label: '旋转副屏', icon: 'rotate', run: () => act(() => invoke('rotate_secondary'), '副屏已旋转') },
  { key: 'color', label: '色彩同步', icon: 'palette', run: () => act(() => invoke('apply_color_space', { space: 'sRGB' })) },
  { key: 'repair', label: '修复显示器', icon: 'wrench', run: () => act(() => invoke('repair_display')) },
]);

/** 把窗口高度校准为内容实际高度，消除底部留白（浏览器预览下跳过） */
async function fitWindow(): Promise<void> {
  if (!isTauri()) return;
  await nextTick();
  try {
    const t = (window as unknown as {
      __TAURI__?: {
        window: { getCurrentWindow: () => { setSize: (s: unknown) => Promise<void> } };
        dpi: { LogicalSize: new (w: number, h: number) => unknown };
      };
    }).__TAURI__;
    if (!t?.window) return;
    const shell = document.querySelector('.panel-shell');
    if (!shell) return;
    const h = Math.ceil(shell.getBoundingClientRect().height);
    // 宽度取窗口当前宽，高度校准为内容实际高度
    await t.window.getCurrentWindow().setSize(new t.dpi.LogicalSize(Math.round(window.innerWidth), Math.min(Math.max(h, 320), 860)));
  } catch {
    /* 权限或 API 不可用时保持配置的固定高度 */
  }
}

const openMain = (): void => {
  void act(() => invoke('open_main'));
};
const hidePanel = (): void => {
  void invoke('hide_panel');
};
const quitApp = (): void => {
  void invoke('quit_app');
};

onMounted(async () => {
  // Rust 端 open_control_panel 会 eval 这个全局函数同步联动开关状态
  (window as unknown as { applyPowerLinkPreference: (v: boolean) => void }).applyPowerLinkPreference = (
    enabled: boolean,
  ) => {
    powerLink.value = enabled;
  };

  version.value = String(await invoke('get_version').catch(() => ''));
  await Promise.all([loadDisplays(), refreshHdr(), refreshAudio()]);
  try {
    powerLink.value = Boolean(await invoke<boolean>('get_power_link_preference'));
  } catch {
    /* 保持默认 */
  }
  try {
    autostart.value = Boolean(await invoke<boolean>('get_autostart'));
  } catch {
    /* 保持默认 */
  }
  // 电源联动状态由后端原生服务推送，面板只展示
  await listenEvent('power-link-status', (payload) => {
    flash(typeof payload === 'string' ? payload : JSON.stringify(payload));
  });
  await fitWindow();
});
</script>

<template>
  <div class="panel-shell">
    <header class="p-head">
      <img class="p-icon" :src="appIcon" alt="" />
      <div class="p-title">
        <strong>屏幕连接助手</strong>
      </div>
      <div class="p-head-btns">
        <button class="p-btn ghost" title="打开主窗口" @click="openMain">主窗</button>
        <button class="p-btn ghost" title="收起面板" @click="hidePanel">收起</button>
      </div>
    </header>

    <div class="p-scroll">
      <section class="p-sec">
        <div class="p-sec-label">快捷操作</div>
        <div class="p-grid">
          <button v-for="a in quickActions" :key="a.key" class="p-cell" :disabled="!!busy" @click="a.run">
            {{ a.label }}
          </button>
          <button class="p-cell" :class="{ on: hdrOn }" :disabled="hdrBusy" @click="toggleHdr">
            HDR{{ hdrOn ? ' 开' : '' }}
          </button>
        </div>
      </section>

      <section v-if="ddcDisplays.length" class="p-sec">
        <div class="p-sec-label">亮度</div>
        <div class="p-slider-row">
          <span class="p-slider-name">全部</span>
          <input
            v-model.number="allBrightness"
            class="p-range"
            type="range"
            min="0"
            max="100"
            @change="commitAllBrightness"
          />
          <span class="p-slider-val">{{ allBrightness }}</span>
        </div>
        <div v-for="d in ddcDisplays" :key="d.id" class="p-slider-row">
          <span class="p-slider-name" :title="d.name">{{ d.name }}</span>
          <input
            v-model.number="briVals[d.id]"
            class="p-range"
            type="range"
            min="0"
            max="100"
            @change="commitBrightness(d)"
          />
          <span class="p-slider-val">{{ briVals[d.id] }}</span>
        </div>
      </section>

      <section v-if="audio" class="p-sec">
        <div class="p-sec-label">音量</div>
        <div class="p-slider-row">
          <span class="p-slider-name" :title="audio.name">{{ audio.name }}</span>
          <input v-model.number="volume" class="p-range" type="range" min="0" max="100" @change="commitVolume" />
          <span class="p-slider-val">{{ volume }}</span>
          <button class="p-btn sm" :class="{ active: muted }" @click="toggleMute">{{ muted ? '取消' : '静音' }}</button>
        </div>
      </section>

      <section v-if="powerRows.length" class="p-sec">
        <div class="p-sec-label">屏幕电源</div>
        <div v-for="row in powerRows" :key="row.id" class="p-power-row">
          <span class="p-slider-name" :title="row.name">{{ row.name }}</span>
          <button class="p-btn sm" :disabled="row.pending" @click="togglePower(row, true)">开</button>
          <button class="p-btn sm" :disabled="row.pending" @click="togglePower(row, false)">关</button>
        </div>
      </section>

      <section class="p-sec">
        <div class="p-toggle-row">
          <div>
            <div class="p-toggle-name">电源联动</div>
            <div class="p-toggle-desc">电脑睡眠或唤醒时，同步关闭、点亮显示器</div>
          </div>
          <button class="p-switch" :class="{ on: powerLink }" role="switch" :aria-checked="powerLink" @click="togglePowerLink">
            <span class="knob" />
          </button>
        </div>
      </section>

      <section class="p-sec p-chat-sec">
        <div class="p-sec-label">对话助手</div>
        <div class="p-chat-embed">
          <ChatPanel compact />
        </div>
      </section>
    </div>

    <footer class="p-foot">
      <button class="p-foot-link" @click="toggleAutostart">
        开机自启<span class="dot" :class="{ on: autostart }" />{{ autostart ? '已开启' : '已关闭' }}
      </button>
      <div class="p-foot-right">
        <span v-if="version" class="p-ver">{{ version }}</span>
        <button class="p-foot-link danger" @click="quitApp">退出应用</button>
      </div>
    </footer>

    <transition name="toast">
      <div v-if="status" class="p-toast" :class="{ err: !statusOk }">{{ status }}</div>
    </transition>
  </div>
</template>
