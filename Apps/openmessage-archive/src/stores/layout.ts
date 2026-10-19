import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { useMediaQuery } from '@/composables/useMediaQuery';
import { useSettings } from './settings';

// Bootstrap's md and lg breakpoints.
const MEDIUM = '(min-width: 768px)';
const LARGE = '(min-width: 992px)';

/**
 * Panel visibility. On wide windows the side panels dock beside the thread
 * and their open/closed state is a saved preference. On narrow windows they
 * become overlays whose state is temporary and starts closed, so a small
 * window never opens with the content covered.
 */
export const useLayout = defineStore('layout', () => {
  const settings = useSettings();
  const canDockLeft = useMediaQuery(MEDIUM);
  const canDockRight = useMediaQuery(LARGE);
  const leftOverlay = ref(false);
  const rightOverlay = ref(false);

  const leftDocked = computed(() => canDockLeft.value && settings.layout.leftPanelOpen);
  const rightDocked = computed(() => canDockRight.value && settings.layout.rightPanelOpen);
  const leftVisible = computed(() => leftDocked.value || (!canDockLeft.value && leftOverlay.value));
  const rightVisible = computed(
    () => rightDocked.value || (!canDockRight.value && rightOverlay.value),
  );

  async function toggleLeft(): Promise<void> {
    if (canDockLeft.value) {
      await settings.setLayout({ ...settings.layout, leftPanelOpen: !settings.layout.leftPanelOpen });
    } else {
      leftOverlay.value = !leftOverlay.value;
      if (leftOverlay.value) rightOverlay.value = false;
    }
  }

  async function toggleRight(): Promise<void> {
    if (canDockRight.value) {
      await settings.setLayout({
        ...settings.layout,
        rightPanelOpen: !settings.layout.rightPanelOpen,
      });
    } else {
      rightOverlay.value = !rightOverlay.value;
      if (rightOverlay.value) leftOverlay.value = false;
    }
  }

  /** Show the right panel (the viewer's details), docked or as an overlay. */
  async function openRight(): Promise<void> {
    if (rightVisible.value) return;
    if (canDockRight.value) {
      await settings.setLayout({ ...settings.layout, rightPanelOpen: true });
    } else {
      rightOverlay.value = true;
      leftOverlay.value = false;
    }
  }

  function closeOverlays(): void {
    leftOverlay.value = false;
    rightOverlay.value = false;
  }

  return {
    canDockLeft,
    canDockRight,
    leftDocked,
    rightDocked,
    leftVisible,
    rightVisible,
    leftOverlay,
    rightOverlay,
    toggleLeft,
    toggleRight,
    openRight,
    closeOverlays,
  };
});
