<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue';
import { invoke } from '../api/tauri';
import { act, store } from '../store';
import { CMD } from '../types';
import type { DisplayHealth, HealthReport, HopKind, Severity } from '../types';
import Card from '../components/Card.vue';
import Icon from '../components/Icon.vue';

const report = ref<HealthReport | null>(null);
const loaded = ref(false);
const expanded = ref<Set<string>>(new Set());
const menuOpen = ref(false);

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

const refresh = (): void => {
  void act(async () => {
    const r = await invoke<HealthReport>(CMD.healthReport);
    report.value = r;
    expanded.value = new Set();
    loaded.value = true;
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
                disabled
                :title="`修复动作 ${f}：待接入`"
              >
                <Icon name="wrench" :size="12" />修复（待接入）
              </button>
            </div>
          </div>
        </div>
      </div>
    </Card>

    <div v-if="loaded && !report?.displays.length" class="health-empty">没有枚举到任何显示器，请检查连接后重新体检。</div>
  </div>
</template>
