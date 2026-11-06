<script setup lang="ts">
/**
 * Agent 排障对话窗（可复用）：消息流 + 原因卡片 + 修复按钮 + 快捷入口。
 * 产品性格是"先动手"：排障输入不反问，直接跑体检再给结论。
 * 修复必须用户点按钮（不自动执行）；修复后自动回读复检并把结果说清楚。
 * 联网分析默认关闭：开启且本地无法定位时先展示待发送 JSON，用户点「同意发送」才发。
 */
import { nextTick, onMounted, ref, watch } from 'vue';
import {
  sendAgentMessage,
  troubleshootDiag,
  TROUBLE_QUICK_ENTRIES,
  CHAT_SUGGESTIONS,
  type ChatMsg,
} from '../api/agent';
import { CMD, INPUT_SOURCES, REPAIR_LABELS, type DiagCause, type RepairOutcome } from '../types';
import { invoke } from '../api/tauri';
import Icon from './Icon.vue';

const props = defineProps<{ compact?: boolean }>();

const messages = ref<ChatMsg[]>([]);
const input = ref('');
const thinking = ref(false);
const listEl = ref<HTMLElement | null>(null);
/** 待二次确认的修复（cause 在 messages 里的坐标 + 动作 id），同一时刻只开一个 */
const confirmFix = ref<{ cause: DiagCause; fix: string } | null>(null);
/** 待选输入源的修复 */
const pickInput = ref<DiagCause | null>(null);
/** 触发当前排障的原始输入（修复后复检用同一个问题重新跑体检） */
const lastQuestion = ref('');
/** 联网分析设置（localStorage，默认关闭） */
const llmEnabled = ref(localStorage.getItem('sc_llm_enabled') === '1');
const llmKey = ref(localStorage.getItem('sc_llm_key') ?? '');

const WELCOME: ChatMsg = {
  role: 'agent',
  text: '我是排障助手，只管屏幕这件事。描述问题（比如「副屏又黑了」「为什么只有 60Hz」），我会先跑体检再给结论和修复按钮；也可以直接下指令，比如「把亮度调到 60」「熄屏」。',
  ts: Date.now(),
};

function scrollBottom(): void {
  void nextTick(() => {
    listEl.value?.scrollTo({ top: listEl.value.scrollHeight, behavior: 'smooth' });
  });
}

function push(msg: ChatMsg): void {
  messages.value.push(msg);
  scrollBottom();
}

/** 联网分析征询：本地无法定位时（scene=other 且无原因），按开关状态决定提示内容 */
async function maybeAskLlm(diagScene: string, hasCauses: boolean, question: string): Promise<void> {
  if (diagScene !== 'other' || hasCauses) return;
  if (!llmEnabled.value) {
    push({
      role: 'agent',
      text: '提示：在「工具 → 联网分析」开启后，可以把你确认过内容的体检数据发给大模型辅助分析。默认关闭，关闭时完全离线可用。',
      ts: Date.now(),
    });
    return;
  }
  try {
    const payload = await invoke<string>(CMD.troubleshootLlmPayload, { text: question });
    push({ role: 'agent', text: '本地体检没能定位这个问题。需要联网分析吗？将发送以下内容（可展开核对，不含序列号）：', ts: Date.now(), llmAsk: { payload, question } });
  } catch {
    /* 预览环境拿不到 payload 就静默跳过 */
  }
}

async function agreeLlm(msg: ChatMsg): Promise<void> {
  if (!msg.llmAsk) return;
  messages.value = messages.value.filter((m) => m !== msg);
  push({ role: 'agent', text: '正在联网分析…', ts: Date.now() });
  thinking.value = true;
  try {
    const reply = await invoke<string>(CMD.agentLlmChat, { text: msg.llmAsk.question, apiKey: llmKey.value });
    messages.value.pop(); // 移除"正在联网分析…"占位
    push({ role: 'agent', text: reply, ts: Date.now() });
  } catch (e) {
    messages.value.pop();
    const err = typeof e === 'string' ? e : '联网分析失败';
    push({ role: 'agent', text: `联网分析失败：${err}`, ts: Date.now() });
  } finally {
    thinking.value = false;
    scrollBottom();
  }
}

function declineLlm(msg: ChatMsg): void {
  messages.value = messages.value.filter((m) => m !== msg);
  push({ role: 'agent', text: '已取消，未发送任何数据。', ts: Date.now() });
}

/** 执行修复：结果 + 回读复检一条消息说清楚 */
async function runFix(cause: DiagCause, fix: string, value?: string): Promise<void> {
  confirmFix.value = null;
  pickInput.value = null;
  const label = REPAIR_LABELS[fix]?.label ?? fix;
  thinking.value = true;
  try {
    const o = await invoke<RepairOutcome>(CMD.repairAction, {
      action: fix,
      displayId: cause.display_id ?? null,
      value: value ?? null,
    });
    const changed = o.changed.length ? `\n变化：${o.changed.join('；')}` : '';
    push({ role: 'agent', text: `【${label}】${o.message}${changed}`, ts: Date.now() });

    // 回读复检：同一个问题重新跑一次体检，给出修复后的结论
    if (lastQuestion.value) {
      const diag = await troubleshootDiag(lastQuestion.value);
      push({ role: 'agent', text: '', ts: Date.now(), diag });
      await maybeAskLlm(diag.scene, diag.causes.length > 0, lastQuestion.value);
    }
  } catch (e) {
    push({ role: 'agent', text: `修复执行出错：${typeof e === 'string' ? e : '请再试一次'}`, ts: Date.now() });
  } finally {
    thinking.value = false;
    scrollBottom();
  }
}

function askFix(cause: DiagCause, fix: string): void {
  // switch_input：先选输入源（选择即确认）；其余动作两步确认
  if (fix === 'switch_input') {
    pickInput.value = pickInput.value === cause ? null : cause;
    confirmFix.value = null;
    return;
  }
  if (confirmFix.value && confirmFix.value.cause === cause && confirmFix.value.fix === fix) {
    void runFix(cause, fix);
    return;
  }
  confirmFix.value = { cause, fix };
  pickInput.value = null;
}

function cancelAsk(): void {
  confirmFix.value = null;
  pickInput.value = null;
}

function openFullReport(): void {
  window.dispatchEvent(new CustomEvent('sc:navigate', { detail: 'health' }));
}

async function send(text: string): Promise<void> {
  const content = text.trim();
  if (!content || thinking.value) return;
  input.value = '';
  cancelAsk();
  push({ role: 'user', text: content, ts: Date.now() });
  thinking.value = true;
  try {
    const reply = await sendAgentMessage(messages.value.slice(0, -1), content);
    if (reply.diag) {
      lastQuestion.value = content;
      push({ role: 'agent', text: '', ts: Date.now(), diag: reply.diag });
      await maybeAskLlm(reply.diag.scene, reply.diag.causes.length > 0, content);
    } else {
      push({ role: 'agent', text: reply.text, ts: Date.now() });
    }
  } catch {
    push({ role: 'agent', text: '执行出错了，请再试一次。', ts: Date.now() });
  } finally {
    thinking.value = false;
    scrollBottom();
  }
}

function submit(): void {
  void send(input.value);
}

onMounted(() => {
  messages.value = [WELCOME];
});

// 托盘面板与主窗口切换显示时保持滚动到底
watch(
  () => messages.value.length,
  () => scrollBottom(),
);
</script>

<template>
  <div class="chat" :class="{ compact: props.compact }">
    <div ref="listEl" class="chat-list">
      <div v-for="(m, i) in messages" :key="i" class="chat-row" :class="m.role">
        <div class="chat-bubble">
          <template v-if="m.text">{{ m.text }}</template>

          <!-- 结构化排障回答：结论 + 原因卡片 + 建议 -->
          <template v-if="m.diag">
            <p class="diag-conclusion" :class="{ ok: !m.diag.causes.length }">{{ m.diag.conclusion }}</p>
            <p v-if="m.diag.healthy_note" class="diag-note">{{ m.diag.healthy_note }}</p>

            <div v-for="(c, ci) in m.diag.causes" :key="ci" class="diag-cause">
              <p class="diag-cause-title">{{ c.title }}</p>
              <ul class="diag-evidence">
                <li v-for="(ev, ei) in c.evidence" :key="ei">{{ ev }}</li>
              </ul>

              <div v-if="c.manual?.length" class="diag-manual">
                <p v-for="(mn, mi) in c.manual" :key="mi">{{ mn }}</p>
              </div>

              <div v-if="c.fix_ids?.length" class="diag-fixes">
                <template v-for="fix in c.fix_ids" :key="fix">
                  <button v-if="confirmFix?.cause === c && confirmFix?.fix === fix" class="btn sm diag-fix-confirm" @click="askFix(c, fix)">
                    确认执行{{ REPAIR_LABELS[fix]?.label }}？
                  </button>
                  <button v-else class="btn sm diag-fix" @click="askFix(c, fix)">尝试修复{{ REPAIR_LABELS[fix]?.label }}</button>
                </template>
                <!-- 输入源选择（选择即确认） -->
                <div v-if="pickInput === c" class="diag-inputs">
                  <button v-for="src in INPUT_SOURCES" :key="src.value" class="btn sm" @click="runFix(c, 'switch_input', src.value)">
                    {{ src.label }}
                  </button>
                </div>
              </div>
            </div>

            <ul v-if="m.diag.advice?.length" class="diag-advice">
              <li v-for="(a, ai) in m.diag.advice" :key="ai">{{ a }}</li>
            </ul>

            <button
              v-if="m.diag.show_report && !props.compact"
              class="diag-report-link"
              @click="openFullReport"
            >
              以上都不对？查看完整体检报告 →
            </button>
          </template>

          <!-- 联网分析同意卡 -->
          <template v-if="m.llmAsk">
            <details class="llm-payload">
              <summary>展开查看将要发送的完整内容</summary>
              <pre>{{ m.llmAsk.payload }}</pre>
            </details>
            <div class="llm-actions">
              <button class="btn sm" @click="agreeLlm(m)">同意发送</button>
              <button class="btn sm danger-ghost" @click="declineLlm(m)">取消</button>
            </div>
          </template>
        </div>
      </div>
      <div v-if="thinking" class="chat-row agent">
        <div class="chat-bubble typing">
          <span /><span /><span />
        </div>
      </div>
    </div>

    <div v-if="messages.length <= 1" class="chat-suggest">
      <button v-for="s in CHAT_SUGGESTIONS" :key="s" class="chat-chip" @click="send(s)">{{ s }}</button>
    </div>

    <div class="chat-quick">
      <span class="chat-quick-label">排障：</span>
      <button v-for="q in TROUBLE_QUICK_ENTRIES" :key="q" class="chat-chip quick" @click="send(q)">{{ q }}</button>
    </div>

    <div class="chat-input-row">
      <input
        v-model="input"
        class="chat-input"
        type="text"
        placeholder="描述问题或输入指令，回车发送"
        :disabled="thinking"
        @keydown.enter="submit"
      />
      <button class="chat-send" :disabled="thinking || !input.trim()" title="发送" @click="submit">
        <Icon name="send" :size="15" />
      </button>
    </div>
  </div>
</template>
