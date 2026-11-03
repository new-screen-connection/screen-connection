<script setup lang="ts">
import Icon from './Icon.vue';

defineProps<{ title: string; icon?: string }>();
</script>

<template>
  <section class="card">
    <div v-if="title" class="card-head">
      <h2 class="card-title">
        <Icon v-if="icon" :name="icon" />
        {{ title }}
      </h2>
      <div class="head-actions">
        <slot name="actions" />
      </div>
    </div>
    <slot />
  </section>
</template>
