<script setup lang="ts">
withDefaults(
  defineProps<{
    icon: string;
    title: string;
    compact?: boolean;
    headingLevel?: 2 | 3;
  }>(),
  { compact: false, headingLevel: 2 },
);
</script>

<template>
  <div class="oma-empty" :class="{ 'oma-empty-compact': compact }">
    <i class="bi oma-empty-icon" :class="`bi-${icon}`" aria-hidden="true"></i>
    <!-- Two fixed elements, not a dynamic component (the source rule bans
         `:is`). -->
    <h3 v-if="headingLevel === 3" class="oma-empty-title">{{ title }}</h3>
    <h2 v-else class="oma-empty-title">{{ title }}</h2>
    <div class="oma-empty-body"><slot /></div>
    <div v-if="$slots.actions" class="oma-empty-actions"><slot name="actions" /></div>
  </div>
</template>
