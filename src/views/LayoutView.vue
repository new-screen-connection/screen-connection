<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { invoke } from '../api/tauri';
import { act, store } from '../store';
import { CMD } from '../types';
import type { ApplyReport, Profile } from '../types';
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

// ---------- 场景档（P0-5） ----------

const profiles = ref<Profile[]>([]);
const loadedProfiles = ref(false);
const applyingId = ref('');
const profileResults = ref<Record<string, { report: ApplyReport; when: string }>>({});
const menuFor = ref('');
const creating = ref(false);
const creatingName = ref('');
const renamingId = ref('');
const renameValue = ref('');

const menuProfile = computed(() => profiles.value.find((p) => p.id === menuFor.value) ?? null);

/** 输入框自动聚焦（打开创建/重命名后直接可打字，回车即保存） */
const vFocus = { mounted: (el: HTMLInputElement) => el.focus() };

async function loadProfiles(): Promise<void> {
  try {
    profiles.value = await invoke<Profile[]>(CMD.profilesList);
  } catch {
    /* 列表加载失败不阻塞页面 */
  }
  loadedProfiles.value = true;
}

function profileResultText(id: string): string {
  const r = profileResults.value[id];
  if (!r) return '';
  const { report, when } = r;
  if (report.rolled_back) {
    const fail = report.steps.find((s) => !s.ok && s.field !== '回滚');
    return `${when} 已回滚：${fail ? `${fail.target} ${fail.field} 未成功` : '应用失败'}，原状态未变`;
  }
  const pass = report.steps.filter((s) => s.ok).length;
  if (report.ok) return `${when} 已应用，${pass} 项全部核验一致`;
  const fail = report.steps.find((s) => !s.ok);
  return `${when} 部分成功（${pass}/${report.steps.length} 项）：${fail?.target ?? ''} ${fail?.field ?? ''} 未生效`;
}

function profileResultTip(id: string): string {
  const r = profileResults.value[id];
  if (!r) return '';
  return r.report.steps
    .map((s) => `${s.ok ? '✓' : '✗'} ${s.target} ${s.field}：要 ${s.want}，实际 ${s.got}${s.error ? `（${s.error}）` : ''}`)
    .join('\n');
}

function createProfile(): void {
  const name = creatingName.value.trim();
  if (!name) return;
  void act(async () => {
    const p = await invoke<Profile>(CMD.profileCapture, { name });
    profiles.value = [...profiles.value, p];
    creating.value = false;
    creatingName.value = '';
    return p;
  }, (p: Profile) => `已保存当前状态为「${p.name}」`);
}

function applyProfile(p: Profile): void {
  if (applyingId.value) return;
  applyingId.value = p.id;
  void act(async () => {
    const r = await invoke<ApplyReport>(CMD.profileApply, { id: p.id });
    profileResults.value = {
      ...profileResults.value,
      [p.id]: { report: r, when: new Date().toLocaleTimeString('zh-CN', { hour12: false }) },
    };
    return r;
  }, (r: ApplyReport) => {
    if (r.rolled_back) return `「${p.name}」应用失败，已整体回滚，显示状态未变`;
    if (r.ok) return `「${p.name}」已应用，全部项回读核验一致`;
    return `「${p.name}」部分成功：个别项这台机器不支持（详情见档位下方）`;
  }).finally(() => {
    applyingId.value = '';
  });
}

function closeMenu(e: MouseEvent): void {
  if (!(e.target as HTMLElement).closest('.profile-wrap')) menuFor.value = '';
}

function renameStart(): void {
  renameValue.value = menuProfile.value?.name ?? '';
  renamingId.value = menuFor.value;
  menuFor.value = '';
}

function renameConfirm(): void {
  const id = renamingId.value;
  const name = renameValue.value.trim();
  renamingId.value = '';
  if (!id || !name) return;
  void act(async () => {
    const p = await invoke<Profile>(CMD.profileUpdate, { id, name, recapture: false, auto: null });
    profiles.value = profiles.value.map((x) => (x.id === id ? p : x));
    return p;
  }, `已重命名为「${name}」`);
}

function updateToCurrent(): void {
  const id = menuFor.value;
  menuFor.value = '';
  void act(async () => {
    const p = await invoke<Profile>(CMD.profileUpdate, { id, name: null, recapture: true, auto: null });
    profiles.value = profiles.value.map((x) => (x.id === id ? p : x));
    return p;
  }, '已把该档更新为当前状态');
}

function removeProfile(): void {
  const id = menuFor.value;
  menuFor.value = '';
  void act(async () => {
    await invoke(CMD.profileDelete, { id });
    profiles.value = profiles.value.filter((x) => x.id !== id);
  }, '已删除该场景档');
}

function exportProfile(): void {
  const id = menuFor.value;
  const p = profiles.value.find((x) => x.id === id);
  menuFor.value = '';
  void act(async () => {
    const json = await invoke<string>(CMD.profileExportJson, { id });
    const blob = new Blob([json], { type: 'application/json;charset=utf-8' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = `profile-${p?.name ?? id}.json`;
    a.click();
    URL.revokeObjectURL(a.href);
  }, '已导出场景档 JSON');
}

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
  void loadProfiles();
  document.addEventListener('click', closeMenu);
});
onBeforeUnmount(() => {
  document.removeEventListener('click', closeMenu);
});
</script>

<template>
  <div class="view">
    <div class="view-head">
      <h1 class="view-title">布局</h1>
      <span class="view-sub">窗口铺满、缩放对齐与双屏体检</span>
    </div>

    <Card title="场景档" icon="target">
      <template #actions>
        <span class="tag" v-if="profiles.length >= 1">前 3 档可用 Ctrl/Cmd+Alt+1..3 快速应用</span>
      </template>

      <div v-if="loadedProfiles && !profiles.length" class="profile-empty">
        还没有场景档。先手动调好你理想的状态（分辨率 / 亮度 / 色彩 / 音频），再点下方「保存当前状态」存成一键档位。
      </div>

      <div class="profile-strip" :style="loadedProfiles && !profiles.length ? 'margin-top: 10px' : ''">
        <div v-for="(p, i) in profiles" :key="p.id" class="profile-wrap">
          <div class="profile-pill" :class="{ applying: applyingId === p.id }" @contextmenu.prevent="menuFor = p.id">
            <span class="profile-hk" v-if="i < 3" :title="`快捷键 Ctrl/Cmd+Alt+${i + 1}`">{{ i + 1 }}</span>
            <template v-if="renamingId === p.id">
              <input
                v-model="renameValue"
                v-focus
                class="profile-rename"
                placeholder="新名称"
                @keyup.enter="renameConfirm"
                @keyup.esc="renamingId = ''"
              />
              <button class="btn sm primary" :disabled="store.busy || !renameValue.trim()" @click="renameConfirm">确定</button>
              <button class="btn sm" @click="renamingId = ''">取消</button>
            </template>
            <template v-else>
              <span class="profile-name" :title="`${p.name}（右键更多操作）`">{{ p.name }}</span>
              <span class="profile-meta">{{ p.targets.displays.length }} 屏</span>
              <button class="btn sm" :disabled="store.busy" @click="applyProfile(p)">
                {{ applyingId === p.id ? '应用中…' : '应用' }}
              </button>
            </template>
          </div>
          <p
            v-if="profileResults[p.id]"
            class="profile-result"
            :class="{ bad: profileResults[p.id].report.rolled_back || !profileResults[p.id].report.ok }"
            :title="profileResultTip(p.id)"
          >
            {{ profileResultText(p.id) }}
          </p>

          <div v-if="menuFor === p.id" class="profile-menu">
            <button class="export-item" @click="renameStart"><Icon name="edit" :size="13" />重命名</button>
            <button class="export-item" @click="updateToCurrent"><Icon name="update" :size="13" />更新为当前状态</button>
            <button class="export-item" @click="exportProfile"><Icon name="download" :size="13" />导出 JSON</button>
            <button class="export-item danger" @click="removeProfile"><Icon name="trash" :size="13" />删除</button>
          </div>
        </div>

        <div class="profile-wrap">
          <div v-if="!creating" class="profile-pill add" @click="creating = true" title="把当前显示状态存成一个新档">
            <span class="profile-plus">＋</span>保存当前状态
          </div>
          <div v-else class="profile-pill creating">
            <input
              v-model="creatingName"
              v-focus
              class="profile-rename"
              placeholder="档位名称，如「工作模式」"
              @keyup.enter="createProfile"
              @keyup.esc="creating = false"
            />
            <button class="btn sm primary" :disabled="store.busy || !creatingName.trim()" @click="createProfile">保存</button>
            <button class="btn sm" @click="creating = false">取消</button>
          </div>
        </div>
      </div>
    </Card>

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
