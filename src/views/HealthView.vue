<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue';
import { invoke } from '../api/tauri';
import { act, store } from '../store';
import type { DisplayHealth, HealthReport, HopKind, RepairOutcome, RepairResult, Severity } from '../types';
import { CMD, INPUT_SOURCES, REPAIR_LABELS } from '../types';
import Card from '../components/Card.vue';
import Icon from '../components/Icon.vue';

const report = ref<HealthReport | null>(null);
const loaded = ref(false);
const expanded = ref<Set<string>>(new Set());
const menuOpen = ref(false);

// ---------- 便携屏建议勾选（P1-4）：标记状态只存本地，不参与诊断逻辑 ----------

const PORTABLE_DONE_KEY = 'sc_portable_done_v1';

function loadAdviceDone(): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(PORTABLE_DONE_KEY);
    return raw ? (JSON.parse(raw) as Record<string, boolean>) : {};
  } catch {
    return {};
  }
}

const adviceDone = ref<Record<string, boolean>>(loadAdviceDone());

function adviceKey(displayId: string, advice: string): string {
  return `${displayId}::${advice}`;
}

function isAdviceDone(displayId: string, advice: string): boolean {
  return adviceDone.value[adviceKey(displayId, advice)] === true;
}

function toggleAdvice(displayId: string, advice: string): void {
  const key = adviceKey(displayId, advice);
  const next = { ...adviceDone.value };
  if (next[key]) delete next[key];
  else next[key] = true;
  adviceDone.value = next;
  try {
    localStorage.setItem(PORTABLE_DONE_KEY, JSON.stringify(next));
  } catch {
    // 存不进 localStorage 就只在本会话生效，不影响功能
  }
}

/** 拓扑跳的固定中文名（label 是各跳的具体值，kind 才是位置名） */
const HOP_NAMES: Record<HopKind, string> = {
  gpu: '显卡',
  port: '接口',
  cable: '线缆',
  hub: '扩展坞',
  display: '显示器',
};

/** 严重度 → 排序权重：问题在前、提示在后 */
const SEV_ORDER: Record<Severity, number> = { error: 0, warn: 1, info: 2, ok: 3 };

function sortChecks(list: DisplayHealth['checks']): DisplayHealth['checks'] {
  return [...list].sort((a, b) => SEV_ORDER[a.severity] - SEV_ORDER[b.severity] || a.id.localeCompare(b.id));
}

/** 拉取体检报告（不动展开状态；供刷新与修复后重跑共用） */
async function loadReport(): Promise<void> {
  const r = await invoke<HealthReport>(CMD.healthReport);
  report.value = r;
  loaded.value = true;
}

const refresh = (): void => {
  void act(async () => {
    await loadReport();
    expanded.value = new Set();
  }, '体检完成');
};

function toggle(key: string): void {
  const next = new Set(expanded.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  expanded.value = next;
}

function sevLabel(s: Severity): string {
  return s === 'error' ? '问题' : s === 'warn' ? '建议' : s === 'info' ? '提示' : '正常';
}

function fmtMode(d: DisplayHealth): string {
  if (!d.connected) return '未在活动路径上';
  const hz = d.current_hz != null ? ` @${d.current_hz}Hz` : '';
  return `${d.current_resolution || '?'}${hz}`;
}

onMounted(() => {
  refresh();
  document.addEventListener('click', closeMenuOnOutside);
});
onBeforeUnmount(() => {
  document.removeEventListener('click', closeMenuOnOutside);
});

// ---------- 导出报告（P0-3）：默认不含设备序列号 ----------

/** 点下拉外面时收起菜单 */
function closeMenuOnOutside(e: MouseEvent): void {
  if (!(e.target as HTMLElement).closest('.export-wrap')) menuOpen.value = false;
}

function fetchMarkdown(): Promise<string> {
  return invoke<string>(CMD.healthReportMarkdown, { includeSerial: false });
}

/** 剪贴板写入：优先 async Clipboard API，被拒/不可用时回退 textarea + execCommand；
 *  两条路都不通时抛出带操作指引的错误（不许静默失败）。 */
async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    // 继续走回退
  }
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try {
    ok = document.execCommand('copy');
  } catch {
    ok = false;
  }
  ta.remove();
  if (!ok) {
    throw new Error('剪贴板不可用：请改用「另存为 .md」保存报告文件，再手动复制内容');
  }
}

function copyReport(): void {
  menuOpen.value = false;
  void act(async () => {
    const md = await fetchMarkdown();
    await copyText(md);
  }, '报告已复制，可直接粘贴到论坛或发给客服（不包含设备序列号）');
}

function saveReport(): void {
  menuOpen.value = false;
  void act(async () => {
    const md = await fetchMarkdown();
    const blob = new Blob([md], { type: 'text/markdown;charset=utf-8' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = `display-health-report-${new Date().toISOString().slice(0, 10)}.md`;
    a.click();
    URL.revokeObjectURL(a.href);
  }, '报告已保存（不包含设备序列号）');
}

// ---------- 修复动作（P0-4）：逐个确认执行，成功后自动重跑体检 ----------

/** key = `${display_id}:${check_id}` → 最近一次修复的就地结果 */
const repairResults = ref<Record<string, RepairOutcome>>({});
/** 当前打开确认/选择面板的 check 行 key（同一时刻只开一个） */
const repairAsk = ref<string | null>(null);
/** 待执行动作 id（确认面板用） */
const repairPending = ref<string | null>(null);

function fixLabel(id: string): string {
  return REPAIR_LABELS[id]?.label ?? id;
}

function repairBadge(r: RepairResult): string {
  return r === 'applied' ? '已执行' : r === 'not_applicable' ? '不适用' : '未成功';
}

function repairDesc(fix: string | null): string {
  return fix ? (REPAIR_LABELS[fix]?.desc ?? '') : '';
}

/** 点修复按钮：switch_input 弹输入源选择（选择即确认），其余需确认的弹就地确认 */
function askRepair(displayId: string, checkId: string, fix: string): void {
  const key = `${displayId}:${checkId}`;
  if (repairAsk.value === key) {
    repairAsk.value = null;
    repairPending.value = null;
    return;
  }
  repairAsk.value = key;
  repairPending.value = fix;
}

function cancelAsk(): void {
  repairAsk.value = null;
  repairPending.value = null;
}

function confirmRepair(displayId: string, checkId: string, fix: string): void {
  cancelAsk();
  void runRepair(displayId, checkId, fix);
}

/** 选输入源即视为确认（明确选择了目标源） */
function pickInput(displayId: string, checkId: string, value: string): void {
  cancelAsk();
  void runRepair(displayId, checkId, 'switch_input', value);
}

async function runRepair(displayId: string, checkId: string, fix: string, value?: string): Promise<void> {
  const key = `${displayId}:${checkId}`;
  await act(async () => {
    const o = await invoke<RepairOutcome>(CMD.repairAction, {
      action: fix,
      displayId,
      value: value ?? null,
    });
    repairResults.value = { ...repairResults.value, [key]: o };
    if (o.result === 'applied') {
      // 成功后自动重跑体检；保持这条 check 展开，让用户看到回读核验到的新状态
      await loadReport();
      expanded.value = new Set([...expanded.value, key]);
    }
    return o;
  }, (o: RepairOutcome) => {
    if (o.result === 'applied') return `${fixLabel(fix)}已执行`;
    if (o.result === 'not_applicable') return `${fixLabel(fix)}：当前环境不适用，原因见详情`;
    return `${fixLabel(fix)}未成功，原因见详情`;
  });
}
</script>

<template>
  <div class="view">
    <div class="view-head">
      <h1 class="view-title">体检</h1>
      <span class="view-sub">逐屏检查连接质量与设置</span>
    </div>

    <Card title="体检结论" icon="gauge">
      <template #actions>
        <span class="tag" v-if="report">{{ report.generated_at }}</span>
        <button class="btn sm" :disabled="store.busy" @click="refresh">
          <Icon name="refresh" :size="13" />重新体检
        </button>
        <div class="export-wrap">
          <button class="btn sm" :disabled="store.busy || !loaded" @click="menuOpen = !menuOpen">
            <Icon name="download" :size="13" />导出报告
          </button>
          <div v-if="menuOpen" class="export-menu">
            <button class="export-item" @click="copyReport">
              <Icon name="clipboard" :size="14" />复制到剪贴板
            </button>
            <button class="export-item" @click="saveReport">
              <Icon name="backup" :size="14" />另存为 .md
            </button>
            <p class="export-tip">导出内容不包含设备序列号</p>
          </div>
        </div>
      </template>

      <div v-if="!loaded" class="health-empty">正在体检，请稍候…</div>
      <template v-else-if="report">
        <div class="health-headline">
          <span class="health-counts">
            <span class="health-chip err" v-if="report.summary.errors">问题 {{ report.summary.errors }}</span>
            <span class="health-chip warn" v-if="report.summary.warnings">建议 {{ report.summary.warnings }}</span>
            <span class="health-chip info" v-if="report.summary.infos">提示 {{ report.summary.infos }}</span>
            <span class="health-chip ok" v-if="!report.summary.errors && !report.summary.warnings && !report.summary.infos">全部正常</span>
          </span>
          <span class="health-line">{{ report.summary.headline }}</span>
        </div>
        <p class="health-meta">
          {{ report.os_version }} · 应用 v{{ report.app_version }} · {{ report.displays.length }} 台显示器（含未活动的 {{ report.displays.filter((d) => !d.connected).length }} 台）
        </p>
      </template>
    </Card>

    <!-- 便携屏一线通区域（P1-4）：仅检测到便携屏时出现 -->
    <Card v-if="report?.portable?.length" title="便携屏" icon="battery">
      <div v-for="p in report.portable" :key="p.display_id" class="portable-item">
        <p class="portable-name">{{ p.name }}</p>
        <p class="portable-conclusion">{{ p.conclusion }}</p>
        <ul class="portable-evidence">
          <li v-for="(ev, i) in p.evidence" :key="i">{{ ev }}</li>
        </ul>
        <div class="portable-advice">
          <button
            v-for="a in p.advice"
            :key="a"
            class="portable-advice-item"
            :class="{ done: isAdviceDone(p.display_id, a) }"
            :title="isAdviceDone(p.display_id, a) ? '点击取消「已处理」标记' : '点击标记为已处理'"
            @click="toggleAdvice(p.display_id, a)"
          >
            <span class="portable-dot"></span>
            <span class="portable-advice-text">{{ a }}</span>
            <span v-if="isAdviceDone(p.display_id, a)" class="portable-done-tag">已处理</span>
          </button>
        </div>
      </div>
      <p class="portable-boundary">本应用无法改变显示器供电，以上为接线与设置建议。</p>
    </Card>

    <Card
      v-for="d in report?.displays ?? []"
      :key="d.display_id"
      :title="d.name"
      :icon="d.connected ? 'monitor' : 'plug'"
    >
      <div class="health-display-meta">
        <span class="tag" :class="{ off: !d.connected }">{{ fmtMode(d) }}</span>
        <span class="tag" v-if="d.native_resolution" :class="{ off: d.native_resolution !== d.current_resolution }">
          原生 {{ d.native_resolution }}
        </span>
        <span class="tag" :class="{ off: !d.ddc_readable }">
          {{ d.ddc_readable ? 'DDC/CI 可用' : d.ddc_capable ? 'DDC/CI 读不到数据' : 'DDC/CI 不可达' }}
        </span>
        <span class="tag" v-if="d.hdr != null">HDR {{ d.hdr ? '开' : '关' }}</span>
        <span class="tag" v-if="d.color_profile">色彩 {{ d.color_profile }}</span>
      </div>

      <div v-if="d.topology" class="topo">
        <div class="topo-chain">
          <template v-for="(h, i) in d.topology.hops" :key="d.display_id + ':' + h.kind">
            <span v-if="i" class="topo-arrow">→</span>
            <button
              class="topo-hop"
              :class="[h.status, { open: expanded.has('t:' + d.display_id + ':' + h.kind) }]"
              @click="toggle('t:' + d.display_id + ':' + h.kind)"
            >
              <span class="topo-kind">{{ HOP_NAMES[h.kind] }}</span>
              <span class="topo-label">{{ h.label }}</span>
            </button>
          </template>
        </div>
        <template v-for="h in d.topology.hops" :key="'d' + h.kind">
          <div v-if="expanded.has('t:' + d.display_id + ':' + h.kind) && h.detail" class="topo-detail">
            {{ h.detail }}
          </div>
        </template>
        <p v-if="d.topology.note" class="topo-note">{{ d.topology.note }}</p>
      </div>

      <p v-if="!d.checks.length" class="health-allok">这块屏没有发现需要留意的地方。</p>

      <div v-else class="health-checks">
        <div
          v-for="c in sortChecks(d.checks)"
          :key="d.display_id + ':' + c.id"
          class="health-check"
          :class="{ open: expanded.has(d.display_id + ':' + c.id) }"
        >
          <button class="health-check-head" @click="toggle(d.display_id + ':' + c.id)">
            <span class="sev-dot" :class="c.severity"></span>
            <span class="health-check-title">{{ c.title }}</span>
            <span class="health-check-sev">{{ sevLabel(c.severity) }}</span>
            <Icon name="restore" :size="13" class="health-caret" />
          </button>
          <div v-if="expanded.has(d.display_id + ':' + c.id)" class="health-check-body">
            <p class="health-detail">{{ c.detail }}</p>
            <ul class="health-evidence">
              <li v-for="(ev, i) in c.evidence" :key="i">{{ ev }}</li>
            </ul>
            <div v-if="c.fix_ids.length" class="health-fixes">
              <button
                v-for="f in c.fix_ids"
                :key="f"
                class="btn sm"
                :disabled="store.busy"
                :title="REPAIR_LABELS[f]?.desc ?? f"
                @click="askRepair(d.display_id, c.id, f)"
              >
                <Icon name="wrench" :size="12" />{{ fixLabel(f) }}
              </button>
            </div>

            <!-- 切换输入源：选源即确认（明确选择目标源） -->
            <div
              v-if="repairAsk === d.display_id + ':' + c.id && repairPending === 'switch_input'"
              class="repair-ask"
            >
              <p class="repair-ask-title">切换到哪个输入源？显示器会短暂重新握手。</p>
              <div class="repair-actions">
                <button
                  v-for="s in INPUT_SOURCES"
                  :key="s.value"
                  class="btn sm"
                  :disabled="store.busy"
                  @click="pickInput(d.display_id, c.id, s.value)"
                >
                  {{ s.label }}
                </button>
                <button class="btn sm repair-cancel" :disabled="store.busy" @click="cancelAsk">取消</button>
              </div>
            </div>

            <!-- 就地二次确认（会临时改变显示状态的动作） -->
            <div
              v-else-if="repairAsk === d.display_id + ':' + c.id && repairPending"
              class="repair-ask"
            >
              <p class="repair-ask-title">
                执行「{{ fixLabel(repairPending ?? '') }}」？{{ repairDesc(repairPending) }}
              </p>
              <div class="repair-actions">
                <button class="btn sm" :disabled="store.busy" @click="confirmRepair(d.display_id, c.id, repairPending)">
                  确认执行
                </button>
                <button class="btn sm repair-cancel" :disabled="store.busy" @click="cancelAsk">取消</button>
              </div>
            </div>

            <!-- 结果三态：成功 / 不适用 / 未成功 -->
            <div
              v-if="repairResults[d.display_id + ':' + c.id]"
              class="repair-outcome"
              :class="repairResults[d.display_id + ':' + c.id].result"
            >
              <p class="repair-msg">
                <span class="repair-badge">{{ repairBadge(repairResults[d.display_id + ':' + c.id].result) }}</span>
                {{ repairResults[d.display_id + ':' + c.id].message }}
              </p>
              <p v-if="repairResults[d.display_id + ':' + c.id].changed.length" class="repair-changed">
                回读核验：{{ repairResults[d.display_id + ':' + c.id].changed.join('；') }}
              </p>
            </div>
          </div>
        </div>
      </div>
    </Card>

    <div v-if="loaded && !report?.displays.length" class="health-empty">没有枚举到任何显示器，请检查连接后重新体检。</div>
  </div>
</template>
