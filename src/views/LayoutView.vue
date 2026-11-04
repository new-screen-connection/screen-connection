<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { invoke, listenEvent } from '../api/tauri';
import { act, setStatus, store } from '../store';
import { CMD } from '../types';
import type { ApplyReport, AutoConfig, AutoEvent, Profile, ProfileMatch } from '../types';
import Card from '../components/Card.vue';
import Icon from '../components/Icon.vue';
import ToggleSwitch from '../components/ToggleSwitch.vue';

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

// ---------- 场景自动触发（P1-1） ----------

const AUTO_EVENT = 'screens://auto-applied';
const DEFAULT_MATCH: ProfileMatch = {
  require_all_displays: true,
  docking: null,
  on_ac: null,
  time_range: null,
};

const autoCfg = ref<AutoConfig | null>(null);
const autoOpen = ref(false);

async function loadAuto(): Promise<void> {
  try {
    autoCfg.value = await invoke<AutoConfig>(CMD.autoRulesGet);
  } catch {
    /* 配置读不到不阻塞页面 */
  }
}

async function saveAuto(next: AutoConfig, okMsg: string): Promise<void> {
  await act(async () => {
    autoCfg.value = await invoke<AutoConfig>(CMD.autoRulesSet, { config: next });
  }, okMsg);
}

/** 总开关。开启时弹一次说明（自动改屏幕是敏感操作，必须先讲清楚） */
async function setAutoEnabled(v: boolean): Promise<void> {
  const cfg = autoCfg.value;
  if (!cfg || cfg.enabled === v) return;
  if (v) {
    const ok = window.confirm(
      '开启后，插拔显示器、插拔电源或到达时间段时，应用可能自动调整分辨率、亮度和布局。\n' +
        `你手动改动配置后的 ${cfg.quiet_minutes} 分钟内不会自动切换；托盘面板可「暂停 1 小时」。\n` +
        '确定开启自动切换？',
    );
    if (!ok) return;
  }
  await saveAuto({ ...cfg, enabled: v }, v ? '自动切换已开启' : '自动切换已关闭');
}

function toggleSource(key: keyof AutoConfig['sources']): void {
  const cfg = autoCfg.value;
  if (!cfg) return;
  void saveAuto(
    { ...cfg, sources: { ...cfg.sources, [key]: !cfg.sources[key] } },
    '触发源设置已保存',
  );
}

const pausedLeftMin = computed(() => {
  const until = autoCfg.value?.paused_until_epoch;
  if (!until) return 0;
  return Math.max(1, Math.ceil((until - Date.now() / 1000) / 60));
});

function pauseAuto(): void {
  void act(async () => {
    autoCfg.value = await invoke<AutoConfig>(CMD.autoPause, { minutes: 60 });
  }, '自动切换已暂停 1 小时（到期自动恢复）');
}

function resumeAuto(): void {
  void act(async () => {
    autoCfg.value = await invoke<AutoConfig>(CMD.autoPause, { minutes: 0 });
  }, '自动切换已恢复');
}

const lastAutoText = computed(() => {
  const la = autoCfg.value?.last_applied;
  if (!la) return '';
  return `${la.at} ${la.ok ? '已应用' : '失败'}「${la.profile_name}」（${la.reason}）`;
});

/** 开关某档的自动触发；开启时若还没配过匹配条件，给默认条件（要求集合完全一致） */
function toggleProfileAuto(p: Profile): void {
  void act(async () => {
    const enabling = !p.auto;
    const matchArg: ProfileMatch | null = enabling && !p.match ? { ...DEFAULT_MATCH } : null;
    const updated = await invoke<Profile>(CMD.profileUpdate, {
      id: p.id,
      name: null,
      recapture: false,
      auto: enabling,
      match: matchArg,
    });
    profiles.value = profiles.value.map((x) => (x.id === p.id ? updated : x));
    return updated;
  }, (up: Profile) => (up.auto ? `「${up.name}」已开启自动触发` : `「${up.name}」已关闭自动触发`));
}

/** 改某档的匹配条件（整体提交 match 对象） */
function patchMatch(p: Profile, patch: Partial<ProfileMatch>): void {
  const base = p.match ?? { ...DEFAULT_MATCH };
  void act(async () => {
    const updated = await invoke<Profile>(CMD.profileUpdate, {
      id: p.id,
      name: null,
      recapture: false,
      auto: null,
      match: { ...base, ...patch },
    });
    profiles.value = profiles.value.map((x) => (x.id === p.id ? updated : x));
    return updated;
  }, `「${p.name}」匹配条件已保存`);
}

function onAcChange(p: Profile, e: Event): void {
  const v = (e.target as HTMLSelectElement).value;
  patchMatch(p, { on_ac: v === '' ? null : v === 'true' });
}

/** select 绑定值：null → ''（任何供电） */
function acSelectValue(p: Profile): string {
  const v = p.match?.on_ac;
  return v === null || v === undefined ? '' : String(v);
}

function onTimeRangeChange(p: Profile, e: Event): void {
  const v = (e.target as HTMLInputElement).value.trim();
  patchMatch(p, { time_range: v || null });
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
  void loadAuto();
  // 自动应用结果推送：状态栏轻提示；回滚/成功后刷新档位与配置（安全阀可能停用了某档）
  await listenEvent(AUTO_EVENT, (payload) => {
    const ev = payload as AutoEvent;
    if (!ev || typeof ev !== 'object' || !ev.kind) return;
    if (ev.kind === 'skipped') {
      setStatus(ev.message || '检测到手动调整，本次未自动切换', true);
      return;
    }
    setStatus(`自动触发「${ev.profile_name}」：${ev.message}`, ev.kind === 'applied');
    void loadProfiles();
    void loadAuto();
  });
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
        <span v-if="autoCfg" class="auto-head">
          <ToggleSwitch
            :model-value="autoCfg.enabled"
            on-text=""
            off-text=""
            @update:model-value="setAutoEnabled"
          />
          <button class="btn sm" :class="{ primary: autoOpen }" @click="autoOpen = !autoOpen">自动规则</button>
        </span>
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
              <span v-if="p.auto" class="profile-auto-tag" title="已开启自动触发">自动</span>
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

      <div v-if="autoOpen && autoCfg" class="auto-panel">
        <div v-if="autoCfg.paused_until_epoch" class="auto-paused">
          自动切换已暂停（剩 {{ pausedLeftMin }} 分钟）
          <button class="btn sm" @click="resumeAuto">立即恢复</button>
        </div>
        <div v-else-if="autoCfg.enabled" class="auto-row">
          <button class="btn sm" @click="pauseAuto" title="会议、演示前临时停用；到期自动恢复">暂停自动切换 1 小时</button>
        </div>

        <div class="auto-row">
          <span class="auto-label">触发源</span>
          <label class="auto-check" title="显示器集合或连接状态变化（最主要的触发源）">
            <input type="checkbox" :checked="autoCfg.sources.displays_changed" @change="toggleSource('displays_changed')" />
            插拔显示器
          </label>
          <label class="auto-check" title="笔记本插拔电源适配器">
            <input type="checkbox" :checked="autoCfg.sources.on_ac" @change="toggleSource('on_ac')" />
            电源接入 / 断开
          </label>
          <label class="auto-check" title="到达档位配置的时间段（如 09:00-19:00）">
            <input type="checkbox" :checked="autoCfg.sources.time_range" @change="toggleSource('time_range')" />
            时间段
          </label>
        </div>

        <p class="auto-note">
          防抖 3 秒（固定）：显示器变化稳定后才应用，连插拔不会反复触发。
          静默期 {{ autoCfg.quiet_minutes }} 分钟：你手动改过配置后，自动切换让路。
          档位还要在下面开启「自动」并配好匹配条件才会被触发。
        </p>

        <div v-if="profiles.length" class="auto-profiles">
          <div v-for="p in profiles" :key="p.id" class="auto-profile-row">
            <span class="auto-profile-name" :title="p.name">{{ p.name }}</span>
            <label class="auto-check">
              <input type="checkbox" :checked="p.auto" @change="toggleProfileAuto(p)" />
              自动
            </label>
            <template v-if="p.auto">
              <label
                class="auto-check"
                :title="p.match?.require_all_displays ?? true ? '要求当前显示器集合与档内完全一致才触发' : '只要档内的屏都在线就触发'"
              >
                <input
                  type="checkbox"
                  :checked="p.match?.require_all_displays ?? true"
                  @change="patchMatch(p, { require_all_displays: !(p.match?.require_all_displays ?? true) })"
                />
                集合完全一致
              </label>
              <select class="auto-sel" :value="acSelectValue(p)" @change="onAcChange(p, $event)">
                <option value="">任何供电</option>
                <option value="true">仅接通电源</option>
                <option value="false">仅用电池</option>
              </select>
              <input
                class="auto-time"
                :value="p.match?.time_range ?? ''"
                placeholder="09:00-19:00"
                title="时间段触发（需开启「时间段」触发源），支持跨零点如 22:00-06:00，留空不限"
                @change="onTimeRangeChange(p, $event)"
              />
            </template>
          </div>
        </div>

        <p
          v-if="lastAutoText"
          class="auto-last"
          :class="{ bad: !autoCfg.last_applied?.ok }"
        >
          最近自动应用：{{ lastAutoText }}
          <template v-if="!autoCfg.last_applied?.ok">（该档自动触发已停用，可重新开启）</template>
        </p>
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
