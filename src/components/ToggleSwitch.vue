<script setup lang="ts">
withDefaults(defineProps<{ modelValue: boolean; onText?: string; offText?: string }>(), {
  onText: '开启',
  offText: '关闭',
});

const emit = defineEmits<{ (e: 'update:modelValue', v: boolean): void }>();
</script>

<template>
  <button
    class="switch"
    type="button"
    :aria-pressed="modelValue"
    @click="emit('update:modelValue', !modelValue)"
  >
    <span class="switch-track"></span>
    <span v-if="modelValue ? onText : offText">{{ modelValue ? onText : offText }}</span>
  </button>
</template>
