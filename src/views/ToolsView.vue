<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { invoke } from '../api/tauri';
import { act, store } from '../store';
import { CMD, REPAIR_LABELS } from '../types';
import type { RepairOutcome } from '../types';
import Card from '../components/Card.vue';
import Icon from '../components/Icon.vue';

const diagOut = ref('');
const diagWhen = ref('尚未检测');
const backupStatus = ref('');
const revertOnExit = ref(true);
const autostart = ref(false);
const fileInput = ref<HTMLInputElement | null>(null);

const runDiag = (): void => {
  void act(async () => {
    const r = String(await invoke<string>('diag_scan'));
    diagOut.value = r;
    diagWhen.value = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  }, '检测完成');
};

/** 一键修复（P0-4 起统一走 repair_action，不再有独立的 diag_fix 流程）：
 *  串行执行安全组合「重新枚举 → 强制重识别」，逐动作回报结果。 */
const fixDiag = (): void => {
  void act(async () => {
    const seq = ['rescan_displays', 'force_redetect'];
    const lines: string[] = [];
    for (const action of seq) {
      const o = await invoke<RepairOutcome>(CMD.repairAction, { action });
      const name = REPAIR_LABELS[action]?.label ?? action;
      const tag = o.result === 'applied' ? '✓' : o.result === 'not_applicable' ? '－' : '✗';
      lines.push(`【${name}】${tag} ${o.message}`);
      if (o.changed.length) lines.push(`  回读核验：${o.changed.join('；')}`);
    }
    lines.push('', '提示：这里只做安全的全局动作；针对单块屏的修复（切输入源/软关再开/重建通道等）请在「体检」页逐条执行。');
    diagOut.value = lines.join('\n');
    diagWhen.value = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  }, '修复流程已执行');
};

const openLogs = (): void => {
  void act(() => invoke('open_log_dir'), '已打开日志文件夹');
};

const exportSettings = (): void => {
  void act(async () => {
    const json = String(await invoke<string>('export_user_settings_json'));
    const blob = new Blob([json], { type: 'application/json' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = 'screen-connection-settings.json';
    a.click();
    URL.revokeObjectURL(a.href);
  }, '设置已导出');
};

const pickImport = (): void => {
  fileInput.value?.click();
};

const importSettings = async (e: Event): Promise<void> => {
  const file = (e.target as HTMLInputElement).files?.[0];
  if (!file) return;
  const text = await file.text();
  await act(
    () => invoke('import_user_settings_json', { settingsJson: text }),
    '设置已导入',
  );
  (e.target as HTMLInputElement).value = '';
};

const restoreDefaults = (): void => {
  void act(
    () => invoke<string>('restore_defaults'),
    (r) => r || '已恢复默认状态',
  );
};

const toggleRevert = (on: boolean): void => {
  void act(() => invoke('set_revert_on_exit', { on }), on ? '退出时将自动还原' : '退出时不再自动还原');
};

const toggleAutostart = (on: boolean): void => {
  void act(() => invoke('set_autostart', { enabled: on }), on ? '已开启开机自启' : '已关闭开机自启');
};

const openPrivacy = (): void => {
  void act(() => invoke('open_external_page', { page: 'privacy' }), '已在浏览器打开隐私说明');
};

const checkUpdate = (): void => {
  void act(() => invoke('open_external_page', { page: 'releases' }), '已在浏览器打开版本发布页');
};

onMounted(async () => {
  try {
    const st = JSON.parse(String(await invoke<string>('app_state_json')) || '{}');
    revertOnExit.value = st.revert_on_exit === true;
  } catch {
    /* 状态缺失不阻塞 */
  }
  autostart.value = Boolean(await invoke<boolean>('get_autostart').catch(() => false));
});
</script>

<template>
  <div class="view">
    <div class="view-head">
      <h1 class="view-title">工具</h1>
      <span class="view-sub">检测、备份与恢复</span>
    </div>

    <Card title="一键检测" icon="gauge">
      <template #actions>
        <span class="tag">{{ diagWhen }}</span>
      </template>
      <div class="btn-grid">
        <button class="btn primary" :disabled="store.busy" @click="runDiag">开始检测</button>
        <button class="btn" :disabled="store.busy" @click="fixDiag">
          <Icon name="wrench" :size="13" />修复问题
        </button>
        <button class="btn" :disabled="store.busy" @click="openLogs">
          <Icon name="clipboard" :size="13" />日志
        </button>
      </div>
      <pre class="output" style="margin-top: 12px" data-placeholder="检测结果会显示在这里">{{ diagOut }}</pre>
    </Card>

    <Card title="设置备份" icon="backup">
      <div class="btn-grid">
        <button class="btn" :disabled="store.busy" @click="exportSettings">导出设置</button>
        <button class="btn" :disabled="store.busy" @click="pickImport">导入设置</button>
        <input ref="fileInput" type="file" accept="application/json,.json" hidden @change="importSettings" />
      </div>
      <p v-if="backupStatus" class="status-line">{{ backupStatus }}</p>
    </Card>

    <Card title="版本与更新" icon="update">
      <div style="display: flex; align-items: center; justify-content: space-between; gap: 12px">
        <span style="font-family: var(--mono); font-size: 12px; color: var(--muted)">
          当前版本 {{ store.version || 'v0.1.0' }}
        </span>
        <button class="btn" :disabled="store.busy" @click="checkUpdate">检查更新</button>
      </div>
    </Card>

    <Card title="恢复与隐私" icon="shield">
      <div class="btn-grid">
        <button class="btn primary" :disabled="store.busy" @click="restoreDefaults">恢复默认状态</button>
        <button class="btn" :disabled="store.busy" @click="openPrivacy">
          <Icon name="lock" :size="13" />隐私说明
        </button>
      </div>
      <label class="check-row">
        <input type="checkbox" v-model="revertOnExit" @change="toggleRevert(revertOnExit)" />
        <span>退出软件时自动还原</span>
      </label>
      <label class="check-row">
        <input type="checkbox" v-model="autostart" @change="toggleAutostart(autostart)" />
        <span>开机自动启动</span>
      </label>
    </Card>
  </div>
</template>
