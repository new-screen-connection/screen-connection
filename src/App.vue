<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { store, setStatus, dismissError, offerAdminRelaunch } from './store';
import { invoke, isTauri, listenEvent } from './api/tauri';
import { startWindowGuard } from './api/windowGuard';
import { currentTheme, initTheme, toggleTheme, type ThemeMode } from './theme';
import Icon from './components/Icon.vue';
import appIcon from './assets/app-icon.png';
import OverviewView from './views/OverviewView.vue';
import HealthView from './views/HealthView.vue';
import QualityView from './views/QualityView.vue';
import LayoutView from './views/LayoutView.vue';
import DevicesView from './views/DevicesView.vue';
import ToolsView from './views/ToolsView.vue';
import AgentView from './views/AgentView.vue';
import KvmView from './views/KvmView.vue';

const views = {
  quality: { label: '画质', icon: 'palette', comp: QualityView },
  layout: { label: '布局', icon: 'layout', comp: LayoutView },
  kvm: { label: '切换', icon: 'keyboard', comp: KvmView },
  overview: { label: '总览', icon: 'monitor', comp: OverviewView },
  health: { label: '体检', icon: 'gauge', comp: HealthView },
  devices: { label: '设备', icon: 'battery', comp: DevicesView },
  tools: { label: '工具', icon: 'wrench', comp: ToolsView },
  agent: { label: '助手', icon: 'chat', comp: AgentView },
} as const;

type ViewKey = keyof typeof views;
const current = ref<ViewKey>('overview');

const platformLabel = ref('检测中');

// CDP 自动化验证钩子（仅 dev 构建生效）：测试脚本需要能直接触发错误横幅、
// 读取状态；生产构建不暴露任何东西。
if (import.meta.env.DEV) {
  (window as unknown as Record<string, unknown>).__sc = { store, setStatus, dismissError, offerAdminRelaunch };
}

// 白天 / 夜晚模式
initTheme();
const theme = ref<ThemeMode>(currentTheme());
function flipTheme(): void {
  theme.value = toggleTheme();
}

onMounted(async () => {
  store.platform = await invoke<string>('get_platform');
  platformLabel.value =
    store.platform === 'darwin' ? 'macOS' : store.platform === 'windows' ? 'Windows' : store.platform || '未知平台';

  // B15 视口守卫：任何操作把主窗口写宽/写错后数秒内自愈
  if (isTauri()) startWindowGuard();

  // 电源联动状态事件（后端推送）
  await listenEvent('power-link-status', (payload) => {
    const msg = typeof payload === 'string' ? payload : JSON.stringify(payload);
    store.status = msg;
    store.statusOk = true;
  });

  // 排障对话「查看完整体检报告」跳转（ChatPanel 派发）
  window.addEventListener('sc:navigate', (e) => {
    const key = (e as CustomEvent<string>).detail;
    if (key in views) current.value = key as ViewKey;
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
        <div class="side-foot-row">
          <span
            class="side-tag"
            :title="isTauri() ? '已连接后端' : '浏览器预览（模拟数据）'"
          >
            <span class="dot" :class="{ mock: !isTauri() }"></span>
            {{ platformLabel }}
          </span>
          <button
            class="theme-toggle"
            :title="theme === 'dark' ? '切换白天模式' : '切换夜晚模式'"
            @click="flipTheme"
          >
            <Icon :name="theme === 'dark' ? 'moon' : 'sun'" :size="15" />
          </button>
        </div>
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

    <!-- 错误横幅：状态栏一行会被截断，长错误（含操作指引）在这里完整展示 -->
    <div v-if="store.errorOpen && !store.statusOk" class="error-banner" role="alert">
      <div class="error-banner-text">{{ store.status }}</div>
      <div class="error-banner-actions">
        <button v-if="store.action" class="eb-btn eb-primary" @click="store.action.run()">
          {{ store.action.label }}
        </button>
        <button class="eb-btn" @click="dismissError">知道了</button>
      </div>
    </div>
  </div>
</template>
