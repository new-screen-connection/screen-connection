<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { store } from './store';
import { invoke, isTauri, listenEvent } from './api/tauri';
import Icon from './components/Icon.vue';
import appIcon from './assets/app-icon.png';
import OverviewView from './views/OverviewView.vue';
import QualityView from './views/QualityView.vue';
import LayoutView from './views/LayoutView.vue';
import DevicesView from './views/DevicesView.vue';
import ToolsView from './views/ToolsView.vue';

const views = {
  quality: { label: '画质', icon: 'palette', comp: QualityView },
  layout: { label: '布局', icon: 'layout', comp: LayoutView },
  overview: { label: '总览', icon: 'monitor', comp: OverviewView },
  devices: { label: '设备', icon: 'battery', comp: DevicesView },
  tools: { label: '工具', icon: 'wrench', comp: ToolsView },
} as const;

type ViewKey = keyof typeof views;
const current = ref<ViewKey>('overview');

const platformLabel = ref('检测中');

onMounted(async () => {
  store.platform = await invoke<string>('get_platform');
  platformLabel.value =
    store.platform === 'darwin' ? 'macOS' : store.platform === 'windows' ? 'Windows' : store.platform || '未知平台';

  // 电源联动状态事件（后端推送）
  await listenEvent('power-link-status', (payload) => {
    const msg = typeof payload === 'string' ? payload : JSON.stringify(payload);
    store.status = msg;
    store.statusOk = true;
  });
});
</script>

<template>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand">
        <img class="brand-mark" :src="appIcon" alt="屏幕连接助手" />
        <div class="brand-text">
          <div class="brand-name">屏幕连接助手</div>
          <div class="brand-ver">v0.1.0</div>
        </div>
      </div>

      <nav class="nav">
        <button
          v-for="(v, key) in views"
          :key="key"
          class="nav-item"
          :class="[{ active: current === key }, 'nav-' + key]"
          @click="current = key as ViewKey"
        >
          <Icon :name="v.icon" :size="17" />
          <span>{{ v.label }}</span>
        </button>
      </nav>

      <div class="side-foot">
        <span class="side-tag">
          <span class="dot" :class="{ mock: !isTauri() }"></span>
          {{ isTauri() ? '已连接后端' : '浏览器预览' }}
        </span>
        <span class="side-tag">{{ platformLabel }}</span>
      </div>
    </aside>

    <main class="main">
      <component :is="views[current].comp" />
    </main>

    <footer class="statusbar">
      <span class="status-text" :class="{ bad: !store.statusOk }">{{ store.status }}</span>
      <span class="statusbar-right">
        <span class="version-tag">{{ store.version || 'v0.1.0' }}</span>
      </span>
    </footer>
  </div>
</template>
