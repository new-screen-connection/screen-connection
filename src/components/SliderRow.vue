<script setup lang="ts">
const props = withDefaults(defineProps<{ label: string; modelValue: number; min?: number; max?: number; unit?: string; disabled?: boolean }>(), {
  min: 0,
  max: 100,
  unit: '',
  disabled: false,
});

const emit = defineEmits<{ (e: 'update:modelValue', v: number): void; (e: 'commit', v: number): void }>();

function onInput(e: Event): void {
  emit('update:modelValue', Number((e.target as HTMLInputElement).value));
}
function onCommit(): void {
  emit('commit', props.modelValue);
}
</script>

<template>
  <div class="slider-row">
    <label>{{ label }}</label>
    <input
      type="range"
      :min="min"
      :max="max"
      :value="modelValue"
      :disabled="disabled"
      @input="onInput"
      @change="onCommit"
    />
    <span class="val">{{ disabled ? '—' : `${modelValue}${unit}` }}</span>
  </div>
</template>
