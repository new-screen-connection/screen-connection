<script setup lang="ts">
import { onMounted, ref } from 'vue';
import type { BatteryInfo } from '../types';
import { invoke } from '../api/tauri';
import Card from '../components/Card.vue';
import Icon from '../components/Icon.vue';

const batteries = ref<BatteryInfo[]>([]);
const loading = ref(true);

function barClass(pct: number): string {
  if (pct < 20) return 'low';
  if (pct < 45) return 'mid';
  return '';
}

const load = async (): Promise<void> => {
  loading.value = true;
  try {
    batteries.value = (await invoke<BatteryInfo[]>('get_batteries')) ?? [];
  } finally {
    loading.value = false;
  }
};

onMounted(load);
</script>

<template>
  <div class="view">
    <div class="view-head">
      <h1 class="view-title">设备</h1>
      <span class="view-sub">带电池的连接设备</span>
    </div>

    <Card title="电池设备" icon="battery">
      <template #actions>
        <button class="btn sm" @click="load" :disabled="loading">
          <Icon name="refresh" :size="13" />刷新
        </button>
      </template>
      <div v-if="loading" class="empty">正在读取带电池的设备…</div>
      <div v-else-if="batteries.length === 0" class="empty">未检测到带电池的设备</div>
      <div v-else style="display: flex; flex-direction: column; gap: 8px">
        <div v-for="b in batteries" :key="b.name" class="battery-row">
          <span class="battery-pct">{{ b.percent >= 0 ? b.percent + '%' : '--' }}</span>
          <div class="battery-bar">
            <i :class="barClass(b.percent)" :style="{ width: (b.percent >= 0 ? Math.min(100, b.percent) : 0) + '%' }"></i>
          </div>
          <div style="min-width: 0; flex: 1.4">
            <div style="font-weight: 600; font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap">
              {{ b.name }}
            </div>
            <div style="font-size: 11.5px; color: var(--muted)">
              {{ b.conn }}
              <template v-if="b.charging"> · 充电中</template>
              <template v-else-if="b.on_ac"> · 已接电源</template>
              <template v-if="b.health >= 0"> · 健康 {{ b.health }}%</template>
            </div>
          </div>
          <span class="tag" v-if="b.charging || b.on_ac">{{ b.charging ? '充电' : '外接电源' }}</span>
        </div>
      </div>
    </Card>
  </div>
</template>
