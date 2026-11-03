<script setup lang="ts">
/** 主窗口的 Agent 对话视图：全高聊天窗 + 能力说明 */
import ChatPanel from '../components/ChatPanel.vue';
</script>

<template>
  <div class="view agent-view">
    <div class="agent-card">
      <ChatPanel />
    </div>
  </div>
</template>

<style scoped>
.agent-view {
  height: 100%;
}

.agent-card {
  height: 100%;
  display: flex;
  flex-direction: column;
  background: var(--panel);
  border: 1px solid var(--border-soft);
  border-radius: var(--radius);
  padding: 14px 16px;
}
</style>
