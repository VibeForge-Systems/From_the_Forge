<script setup lang="ts">
import { computed } from 'vue';
import { useLayout } from '@/stores/layout';

defineProps<{ leftLabel: string; rightLabel: string }>();
const layout = useLayout();

const overlayOpen = computed(
  () =>
    (layout.leftVisible && !layout.leftDocked) || (layout.rightVisible && !layout.rightDocked),
);
</script>

<template>
  <div
    class="oma-panels"
    :class="{ 'has-left': layout.leftDocked, 'has-right': layout.rightDocked }"
    @keydown.esc="layout.closeOverlays()"
  >
    <aside
      v-if="layout.leftVisible"
      id="oma-left-panel"
      class="oma-panel oma-panel-left"
      :class="{ 'oma-overlay': !layout.leftDocked }"
      :aria-label="leftLabel"
    >
      <slot name="left" />
    </aside>
    <section class="oma-panel-center" aria-label="Thread">
      <slot />
    </section>
    <aside
      v-if="layout.rightVisible"
      id="oma-right-panel"
      class="oma-panel oma-panel-right"
      :class="{ 'oma-overlay': !layout.rightDocked }"
      :aria-label="rightLabel"
    >
      <slot name="right" />
    </aside>
    <div v-if="overlayOpen" class="oma-backdrop" aria-hidden="true" @click="layout.closeOverlays()"></div>
  </div>
</template>
