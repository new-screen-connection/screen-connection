<script setup lang="ts">
import { computed } from 'vue';
import { ICONS } from '../icons';

const props = defineProps<{ name: string; size?: number }>();

const def = computed(() => ICONS[props.name] ?? ICONS.info);
const sizePx = computed(() => `${props.size ?? 16}px`);
</script>

<template>
  <svg
    class="ic"
    :width="sizePx"
    :height="sizePx"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.7"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <path v-for="(d, i) in def.paths" :key="i" :d="d" />
    <circle
      v-for="(c, i) in def.circles ?? []"
      :key="'c' + i"
      :cx="c.cx"
      :cy="c.cy"
      :r="c.r"
      :fill="c.r < 1 ? 'currentColor' : 'none'"
      :stroke="c.r < 1 ? 'none' : 'currentColor'"
    />
  </svg>
</template>
