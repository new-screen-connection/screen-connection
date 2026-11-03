<script setup lang="ts">
/**
 * Agent 对话窗（可复用）：消息流 + 输入框 + 快捷提问。
 * compact 模式用于托盘小窗（更紧凑的行高与字号）。
 * 回复接入见 src/api/agent.ts：将来后端实现 agent_chat 命令即自动切换。
 */
import { nextTick, onMounted, ref, watch } from 'vue';
import { sendAgentMessage, CHAT_SUGGESTIONS, type ChatMsg } from '../api/agent';
import Icon from './Icon.vue';

const props = defineProps<{ compact?: boolean }>();

const messages = ref<ChatMsg[]>([]);
const input = ref('');
const thinking = ref(false);
const listEl = ref<HTMLElement | null>(null);

const WELCOME: ChatMsg = {
  role: 'agent',
  text: '你好，我是屏幕连接助手。直接用自然语言告诉我你想做什么，比如调整亮度、铺满窗口、熄屏，我来执行。',
  ts: Date.now(),
};

function scrollBottom(): void {
  void nextTick(() => {
    listEl.value?.scrollTo({ top: listEl.value.scrollHeight, behavior: 'smooth' });
  });
}

async function send(text: string): Promise<void> {
  const content = text.trim();
  if (!content || thinking.value) return;
  input.value = '';
  messages.value.push({ role: 'user', text: content, ts: Date.now() });
  thinking.value = true;
  scrollBottom();
  try {
    const reply = await sendAgentMessage(messages.value.slice(0, -1), content);
    messages.value.push({ role: 'agent', text: reply, ts: Date.now() });
  } catch {
    messages.value.push({ role: 'agent', text: '执行出错了，请再试一次。', ts: Date.now() });
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
        <div class="chat-bubble">{{ m.text }}</div>
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

    <div class="chat-input-row">
      <input
        v-model="input"
        class="chat-input"
        type="text"
        placeholder="输入指令，回车发送"
        :disabled="thinking"
        @keydown.enter="submit"
      />
      <button class="chat-send" :disabled="thinking || !input.trim()" title="发送" @click="submit">
        <Icon name="send" :size="15" />
      </button>
    </div>
  </div>
</template>
