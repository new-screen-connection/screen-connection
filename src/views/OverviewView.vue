<script setup lang="ts">
import { onMounted, ref } from 'vue';
import type { DisplayInfo } from '../types';
import { invoke } from '../api/tauri';
import { act, store } from '../store';
import Card from '../components/Card.vue';
import Icon from '../components/Icon.vue';
import ToggleSwitch from '../components/ToggleSwitch.vue';
import DisplayCard from '../components/DisplayCard.vue';

const displays = ref<DisplayInfo[]>([]);
const loading = ref(true);
const powerLink = ref(false);
const hotkey = ref('');
const isMac = ref(false);

const loadDisplays = async (): Promise<void> => {
  loading.value = true;
  try {
    displays.value = (await invoke<DisplayInfo[]>('get_displays')) ?? [];
  } finally {
    loading.value = false;
  }
};

const openPanel = (): void => {
  void act(
    () => invoke('open_control_panel', { powerLinkEnabled: powerLink.value }),
    '已打开快捷控制面板',
  );
};

const screenOff = (): void => {
  void act(() => invoke('screen_off'), '两块屏幕即将待机，晃动鼠标或敲键盘即可唤醒');
};

const screenWake = (): void => {
  void act(() => invoke('screen_wake'), '已唤醒屏幕');
};

const spanForeground = (): void => {
  void act(
    async () => invoke<string>('span_foreground', { mode: 2 }),
    (r) => r || '已铺满前台窗口',
  );
};

const restoreForeground = (): void => {
  void act(() => invoke('restore_foreground'), '已还原上一个被铺满的窗口');
};

const togglePowerLink = (on: boolean): void => {
  void act(async () => {
    await invoke('set_power_link_preference', { enabled: on });
    powerLink.value = on;
  }, on ? '电源联动已开启' : '电源联动已关闭');
};

onMounted(async () => {
  isMac.value = (await invoke<string>('get_platform')) === 'macos';
  store.version = String(await invoke<string>('get_version'));
  powerLink.value = Boolean(await invoke<boolean>('get_power_link_preference'));
  hotkey.value = String(await invoke<string>('hotkey_start'));
  await loadDisplays();
});
</script>

<template>
  <div class="view">
    <div class="view-head">
      <h1 class="view-title">总览</h1>
      <span class="view-sub">显示器状态与常用动作</span>
    </div>

    <!-- 快捷动作 -->
    <div class="quickbar">
      <button class="quick-btn" :disabled="store.busy" @click="screenOff">
        <Icon name="moon" :size="20" />
        <span class="quick-label">全部待机</span>
      </button>
      <button class="quick-btn" :disabled="store.busy" @click="screenWake">
        <Icon name="sun" :size="20" />
        <span class="quick-label">唤醒屏幕</span>
      </button>
      <button class="quick-btn" :disabled="store.busy" @click="spanForeground">
        <Icon name="span" :size="20" />
        <span class="quick-label">铺满前台窗口</span>
      </button>
      <button class="quick-btn" :disabled="store.busy" @click="restoreForeground">
        <Icon name="restore" :size="20" />
        <span class="quick-label">还原窗口</span>
      </button>
    </div>

    <!-- 显示器列表 -->
    <Card title="显示器" icon="monitor">
      <template #actions>
        <button class="btn sm" @click="openPanel" :disabled="store.busy">快捷面板</button>
        <button class="btn sm" @click="loadDisplays" :disabled="loading">
          <Icon name="refresh" :size="13" />刷新
        </button>
      </template>
      <div v-if="loading" class="empty">正在读取显示器…</div>
      <div v-else-if="displays.length === 0" class="empty">未检测到显示器</div>
      <div v-else class="display-grid">
        <DisplayCard v-for="d in displays" :key="d.id" :display="d" @refreshed="loadDisplays" />
      </div>
    </Card>

    <!-- 电源联动 -->
    <Card title="电源联动" icon="power">
      <template #actions>
        <ToggleSwitch v-model="powerLink" on-text="" off-text="" @update:model-value="togglePowerLink" />
      </template>
      <div style="display: flex; align-items: center; gap: 10px; font-size: 12.5px; color: var(--muted)">
        <Icon name="keyboard" :size="16" />
        <span>检测到设备接入或断开时自动待机 / 唤醒，快捷键</span>
        <span class="keycap">{{ hotkey || '未注册' }}</span>
      </div>
    </Card>

    <!-- macOS 高级显示控制（仅 macOS 显示） -->
    <Card v-if="isMac" title="高级显示控制" icon="wrench">
      <p class="status-line">macOS 专属：主屏切换、镜像、排列与输入源控制（在后续版本中接入）。</p>
    </Card>
  </div>
</template>
