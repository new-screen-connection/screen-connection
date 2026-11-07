<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { invoke, listenEvent, isTauri } from '../api/tauri';
import { act, store } from '../store';
import { CMD, REPAIR_LABELS } from '../types';
import type { RepairOutcome, UpdateInfo } from '../types';
import Card from '../components/Card.vue';
import Icon from '../components/Icon.vue';

const diagOut = ref('');
const diagWhen = ref('尚未检测');
const backupStatus = ref('');
const revertOnExit = ref(true);
const autostart = ref(false);
const fileInput = ref<HTMLInputElement | null>(null);
// 联网分析（P2-1）：默认关闭；key 只存本机 localStorage，不随设置导出
const llmEnabled = ref(localStorage.getItem('sc_llm_enabled') === '1');
const llmKey = ref(localStorage.getItem('sc_llm_key') ?? '');

// 应用内更新（v0.4.1）：不再跳转 GitHub 网页
const updateInfo = ref<UpdateInfo>();
const updateStatus = ref('');
const downloading = ref(false);
const updateProgress = ref(0);

function toggleLlm(on: boolean): void {
  llmEnabled.value = on;
  localStorage.setItem('sc_llm_enabled', on ? '1' : '0');
}
function saveLlmKey(): void {
  localStorage.setItem('sc_llm_key', llmKey.value.trim());
}

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
    // B3：真机环境走系统「另存为」对话框，由用户选保存位置；
    // 浏览器预览（无 Tauri 对话框）时回退到原静默下载。
    if (isTauri()) {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const path = await save({
        title: '导出设置',
        defaultPath: 'screen-connection-settings.json',
        filters: [{ name: 'JSON 设置文件', extensions: ['json'] }],
      });
      if (!path) return ''; // 用户取消了另存为
      await invoke('write_text_file', { path, contents: json });
      return path;
    }
    const blob = new Blob([json], { type: 'application/json' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = 'screen-connection-settings.json';
    a.click();
    URL.revokeObjectURL(a.href);
    return '';
  }, (p) => (p ? `设置已导出到 ${p}` : '设置已导出'));
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
  void act(async () => {
    updateInfo.value = undefined;
    const info = await invoke<UpdateInfo>(CMD.updateCheck);
    updateInfo.value = info;
    if (!info.hasUpdate) {
      updateStatus.value = `已是最新版本（v${info.current}）`;
    } else {
      updateStatus.value = '';
    }
  }, '正在检查更新…');
};

/** 应用内下载安装：进度走 update-progress 事件，完成后一键启动安装程序 */
const runUpdate = (): void => {
  const info = updateInfo.value;
  if (!info) return;
  void act(async () => {
    downloading.value = true;
    updateProgress.value = 0;
    const path = await invoke<string>(CMD.updateDownload, {
      url: info.assetUrl,
      size: info.assetSize,
    });
    downloading.value = false;
    await invoke(CMD.updateInstall, { path });
    updateStatus.value = '安装程序已启动，应用即将退出并开始升级…';
  }, '正在下载更新…');
};

onMounted(async () => {
  try {
    const st = JSON.parse(String(await invoke<string>('app_state_json')) || '{}');
    revertOnExit.value = st.revert_on_exit === true;
  } catch {
    /* 状态缺失不阻塞 */
  }
  autostart.value = Boolean(await invoke<boolean>('get_autostart').catch(() => false));
  // 下载进度事件 → 进度条
  await listenEvent('update-progress', (payload: unknown) => {
    const p = payload as { downloaded: number; total: number } | null;
    if (!p) return;
    const total = p.total || updateInfo.value?.assetSize || 0;
    updateProgress.value = total > 0 ? Math.min(100, Math.round((p.downloaded / total) * 100)) : 0;
  });
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

    <Card title="AI 助手（默认关闭）" icon="wrench">
      <label class="check-row">
        <input type="checkbox" :checked="llmEnabled" @change="toggleLlm(($event.target as HTMLInputElement).checked)" />
        <span>允许排障助手使用 AI 联网分析（本地体检无法定位时才询问，发送前先展示完整内容，你确认后才发）</span>
      </label>
      <div v-if="llmEnabled" class="llm-key-row">
        <input
          v-model="llmKey"
          type="password"
          placeholder="API Key（DeepSeek 等兼容 OpenAI 接口的服务）"
          autocomplete="new-password"
          @change="saveLlmKey"
        />
        <span class="llm-key-tip">Key 只保存在本机，不会上传，也不会随设置导出</span>
      </div>
      <p class="llm-offline-tip">关闭时排障完全离线可用，不发出任何网络请求。</p>
    </Card>

    <Card title="版本与更新" icon="update">
      <div style="display: flex; align-items: center; justify-content: space-between; gap: 12px">
        <span style="font-family: var(--mono); font-size: 12px; color: var(--muted)">
          当前版本 {{ store.version || 'v0.1.0' }}
        </span>
        <button class="btn" :disabled="store.busy || downloading" @click="checkUpdate">检查更新</button>
      </div>
      <p v-if="updateStatus" class="status-line">{{ updateStatus }}</p>
      <template v-if="updateInfo?.hasUpdate">
        <p class="status-line">
          发现新版本 <b>v{{ updateInfo.latest }}</b>
          （安装包 {{ (updateInfo.assetSize / 1024 / 1024).toFixed(0) }} MiB）
        </p>
        <pre v-if="updateInfo.notes" class="output" style="max-height: 140px; overflow: auto">{{ updateInfo.notes }}</pre>
        <div v-if="downloading" class="update-progress-row">
          <div class="update-progress-bar"><div class="update-progress-fill" :style="{ width: updateProgress + '%' }"></div></div>
          <span class="update-progress-text">{{ updateProgress }}%</span>
        </div>
        <div class="btn-grid" style="margin-top: 10px">
          <button class="btn primary" :disabled="downloading" @click="runUpdate">
            {{ downloading ? '下载中…' : '下载并安装' }}
          </button>
        </div>
      </template>
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
