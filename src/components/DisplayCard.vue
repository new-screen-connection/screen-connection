<script setup lang="ts">
import { ref, watch } from 'vue';
import type { DisplayInfo } from '../types';
import { invoke, errText } from '../api/tauri';
import { sendVCP } from '../api/vcp';
import { setStatus } from '../store';
import Icon from './Icon.vue';
import SliderRow from './SliderRow.vue';

const props = defineProps<{ display: DisplayInfo }>();

const brightness = ref(props.display.brightness ?? 50);
const volume = ref(props.display.volume ?? 0);

watch(
  () => props.display,
  (d) => {
    brightness.value = d.brightness ?? 50;
    volume.value = d.volume ?? 0;
  },
);

function pushBrightness(): void {
  sendVCP('brightness', props.display.id, brightness.value, invoke);
}

function pushVolume(): void {
  if (props.display.volume === null) return;
  sendVCP('volume', props.display.id, volume.value, invoke);
}

async function refreshInfo(): Promise<void> {
  try {
    await invoke('display_action', { action: 'refresh', displayId: props.display.id });
    setStatus(`已刷新「${props.display.name}」`);
  } catch (e) {
    setStatus(errText(e), false);
  }
}
</script>

<template>
  <div class="display-card">
    <div class="display-top">
      <span class="display-name">
        <Icon name="monitor" :size="15" />
        {{ display.name }}
      </span>
      <span class="tag" :class="{ main: display.main }">{{ display.main ? '主屏' : '副屏' }}</span>
    </div>

    <div class="display-meta">
      {{ display.resolution }}<template v-if="display.hz"> · {{ display.hz }}Hz</template>
      <template v-if="display.color_profile"> · {{ display.color_profile }}</template>
    </div>

    <div class="display-tags">
      <span class="tag" v-if="display.ddc">DDC/CI</span>
      <span class="tag off" v-else>无 DDC</span>
    </div>

    <SliderRow
      v-if="display.ddc && display.brightness !== null"
      v-model="brightness"
      label="亮度"
      unit="%"
      @commit="pushBrightness"
    />
    <SliderRow
      v-if="display.ddc"
      v-model="volume"
      label="音量"
      unit="%"
      :disabled="display.volume === null"
      @commit="pushVolume"
    />

    <div class="btn-grid display-foot">
      <button class="btn sm" @click="refreshInfo">
        <Icon name="refresh" :size="13" />刷新信息
      </button>
    </div>
  </div>
</template>
