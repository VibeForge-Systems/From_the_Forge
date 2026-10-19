<script setup lang="ts">
// The three-panel layout. On narrow windows the side panels are overlays
// over the centre with a backdrop; Escape or the backdrop closes them.
//
// - Escape pressed inside an open `<dialog>` (the date dialog lives in the
//   thread, inside this layout) belongs to that dialog: it never also
//   closes an overlay.
// - When an overlay closes and focus was inside it (now gone), focus goes
//   back to what had it when the overlay opened, so it never sits under a
//   backdrop or on `<body>` (WCAG 2.4.3, 2.4.11).
import { computed, nextTick, watch } from 'vue';
import { useLayout } from '@/stores/layout';

defineProps<{ leftLabel: string; rightLabel: string }>();
const layout = useLayout();

const overlayOpen = computed(
  () =>
    (layout.leftVisible && !layout.leftDocked) || (layout.rightVisible && !layout.rightDocked),
);

let opener: HTMLElement | null = null;

watch(overlayOpen, (open) => {
  if (open) {
    const active = document.activeElement;
    opener = active instanceof HTMLElement && active !== document.body ? active : null;
    return;
  }
  const back = opener;
  opener = null;
  void nextTick(() => {
    const now = document.activeElement;
    if (now !== null && now !== document.body) return;
    if (back && back.isConnected && !back.closest('dialog:not([open])')) back.focus({ preventScroll: true });
  });
});

function onEscape(event: KeyboardEvent): void {
  const target = event.target;
  if (target instanceof Element && target.closest('dialog[open]')) return;
  layout.closeOverlays();
}
</script>

<template>
  <div
    class="oma-panels"
    :class="{ 'has-left': layout.leftDocked, 'has-right': layout.rightDocked }"
    @keydown.esc="onEscape"
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
