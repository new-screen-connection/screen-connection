<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { invoke } from '../api/tauri';
import { act, store } from '../store';
import Card from '../components/Card.vue';
import Icon from '../components/Icon.vue';

const windows = ref<string[]>([]);
const spanTarget = ref('');
const spanMode = ref(2);
const secondaryPortrait = ref(false);
const alignOut = ref('');
const alignSizes = ref('');
const alignManuf = ref('');
const alignInches = ref('');
const isMac = ref(false);

const SPAN_MODES = [
  { v: 2, label: '自动对齐两屏清晰度（推荐）' },
  { v: 1, label: '以主屏为基准' },
  { v: 0, label: '铺满整块桌面' },
];

async function refreshWindows(): Promise<void> {
  await act(async () => {
    const raw = String(await invoke<string>('list_windows'));
    windows.value = raw.split('\n').map((s) => s.trim()).filter(Boolean);
  }, `已刷新，找到 ${windows.value.length} 个可铺满窗口`);
}

const spanSelected = (): void => {
  if (!spanTarget.value) return;
  void act(
    () => invoke<string>('span_window', { hwnd: spanTarget.value, mode: spanMode.value }),
    (r) => r || '已铺满选中窗口',
  );
};

const matchPpi = (): void => {
  void act(
    () => invoke<string>(isMac.value ? 'match_ppi' : 'match_dpi_apply'),
    (r) => r || '副屏已对齐逻辑档位，窗口跨屏不变小',
  );
};

const dpiRestore = (): void => {
  void act(
    () => invoke<string>(isMac.value ? 'match_ppi' : 'match_dpi_restore'),
    (r) => r || '已还原缩放设置',
  );
};

const rotate = (): void => {
  void act(async () => {
    if (secondaryPortrait.value) {
      const r = await invoke<string>('restore_secondary');
      secondaryPortrait.value = false;
      return r;
    }
    const r = await invoke<string>('rotate_secondary');
    secondaryPortrait.value = true;
    return r;
  }, (r) => r || '副屏方向已切换');
};

const runAlign = (): void => {
  void act(async () => {
    alignOut.value = String(await invoke<string>('align_report'));
  }, '双屏体检完成');
};

const applyAlign = (): void => {
  void act(async () => {
    const r = String(await invoke<string>('align_apply'));
    alignOut.value = r;
  }, '已应用推荐组合');
};

const restoreAlign = (): void => {
  void act(async () => {
    const r = String(await invoke<string>('align_restore'));
    alignOut.value = r;
  }, '已还原原始分辨率');
};

const copyAlign = (): void => {
  void navigator.clipboard.writeText(alignOut.value).then(
    () => act(async () => undefined, '结论已复制到剪贴板'),
  );
};

const saveSizes = (): void => {
  void act(
    () => invoke('display_action', { action: 'align-save-size', displayId: '', targetId: null, value: `${alignManuf.value}|${alignInches.value}` }),
    '已保存显示器尺寸',
  );
};

onMounted(async () => {
  isMac.value = (await invoke<string>('get_platform')) === 'macos';
  try {
    const st = JSON.parse(String(await invoke<string>('app_state_json')) || '{}');
    alignSizes.value = st.align_sizes ? `已存尺寸：${st.align_sizes}` : '';
    secondaryPortrait.value = st.secondary_portrait === true;
  } catch {
    /* 状态缺失不阻塞 */
  }
});
</script>

<template>
  <div class="view">
    <div class="view-head">
      <h1 class="view-title">布局</h1>
      <span class="view-sub">窗口铺满、缩放对齐与双屏体检</span>
    </div>

    <Card title="窗口与缩放" icon="window">
      <div class="btn-grid">
        <button class="btn primary" :disabled="store.busy" @click="matchPpi">窗口跨屏不变小</button>
        <button class="btn" :disabled="store.busy" @click="dpiRestore">
          <Icon name="restore" :size="13" />还原缩放
        </button>
        <button class="btn" :disabled="store.busy" @click="rotate">
          <Icon name="rotate" :size="13" />{{ secondaryPortrait ? '副屏恢复横向' : '副屏切换竖向' }}
        </button>
      </div>
    </Card>

    <Card title="双屏铺满" icon="span">
      <div class="field-grid">
        <div class="field">
          <span>目标窗口</span>
          <select v-model="spanTarget">
            <option value="" disabled>{{ windows.length ? '选择要铺满的窗口' : '先刷新窗口列表' }}</option>
            <option v-for="w in windows" :key="w" :value="w">{{ w }}</option>
          </select>
        </div>
        <div class="field">
          <span>铺满方式</span>
          <select v-model.number="spanMode">
            <option v-for="m in SPAN_MODES" :key="m.v" :value="m.v">{{ m.label }}</option>
          </select>
        </div>
      </div>
      <div class="btn-grid" style="margin-top: 12px">
        <button class="btn" :disabled="store.busy" @click="refreshWindows">
          <Icon name="refresh" :size="13" />刷新窗口列表
        </button>
        <button class="btn primary" :disabled="store.busy || !spanTarget" @click="spanSelected">
          <Icon name="span" :size="13" />铺满选中窗口
        </button>
      </div>
    </Card>

    <Card title="双屏对齐" icon="layout">
      <template #actions>
        <button class="btn sm" @click="copyAlign" :disabled="!alignOut">复制结论</button>
      </template>
      <div class="btn-grid">
        <button class="btn" :disabled="store.busy" @click="runAlign">
          <Icon name="gauge" :size="13" />开始体检
        </button>
        <button class="btn primary" :disabled="store.busy" @click="applyAlign">
          <Icon name="target" :size="13" />应用推荐组合
        </button>
        <button class="btn" :disabled="store.busy" @click="restoreAlign">
          <Icon name="restore" :size="13" />还原原始分辨率
        </button>
      </div>
      <pre class="output" style="margin-top: 12px" data-placeholder="体检结果会显示在这里">{{ alignOut }}</pre>
      <div class="inline-field">
        <input v-model="alignManuf" placeholder="厂商代码" />
        <input v-model="alignInches" placeholder="对角线英寸" />
        <button class="btn" :disabled="store.busy" @click="saveSizes">保存尺寸</button>
      </div>
      <p v-if="alignSizes" class="status-line">{{ alignSizes }}</p>
    </Card>
  </div>
</template>
