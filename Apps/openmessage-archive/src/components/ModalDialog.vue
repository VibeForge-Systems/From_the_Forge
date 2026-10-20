<script setup lang="ts">
import { nextTick, onMounted, ref, useId, watch } from 'vue';

// Built on the native <dialog> element: showModal() provides the focus
// trap, Escape handling, and inert background that a hand-rolled modal
// would have to reimplement. showModal() moves focus to the first control;
// on close, focus returns to what had it before (M5 §10, slice 5j), also on
// engines that do not restore it themselves. `returnTo` (an app-made
// element id) names where focus goes back when the opener cannot take it:
// a control that was disabled or removed when it was pressed leaves focus
// on `<body>`, and a control inside another, now closed, dialog cannot be
// focused.
// `guardEscape`: Escape does not close the dialog by itself; it reports
// `close`, and the parent decides (the Export dialog asks before
// cancelling a running export).
const props = defineProps<{ open: boolean; title: string; returnTo?: string | null; guardEscape?: boolean }>();
const emit = defineEmits<{ close: [] }>();

const dialog = ref<HTMLDialogElement | null>(null);
const titleId = useId();
let opener: HTMLElement | null = null;
let returnId: string | null = null;

/** An element that can take focus back: connected, not `<body>`, not in a closed dialog. */
function usable(el: HTMLElement | null): el is HTMLElement {
  return !!el && el.isConnected && el !== document.body && !el.closest('dialog:not([open])');
}

/** Give focus back to the opener, unless the user already moved it elsewhere. */
function restoreFocus(): void {
  const back = usable(opener) ? opener : returnId ? document.getElementById(returnId) : null;
  opener = null;
  returnId = null;
  if (!usable(back)) return;
  const now = document.activeElement;
  if (now === null || now === document.body || (dialog.value?.contains(now) ?? false) || !!now.closest('dialog:not([open])')) {
    back.focus({ preventScroll: true });
  }
}

function sync(open: boolean): void {
  const el = dialog.value;
  if (!el || typeof el.showModal !== 'function') return;
  if (open && !el.open) {
    const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    opener = active === document.body ? null : active;
    returnId = props.returnTo ?? null;
    el.showModal();
  }
  if (!open && el.open) {
    el.close();
    restoreFocus();
  }
}

onMounted(() => sync(props.open));
watch(() => props.open, sync);

/**
 * The native `close` event is dispatched asynchronously. Report it only when
 * the user closed the dialog (Escape) while it was meant to be open; a stale
 * event from a programmatic close that arrives after the dialog was reopened
 * must not close it again.
 *
 * The parent may decline the close (a running export asks first). The
 * element is closed by then, whatever `cancel`'s `preventDefault()` did: a
 * Chromium engine (WebView2) makes a second Escape's `cancel`
 * non-cancelable without a user activation in between. So when `open` is
 * still true after the parent had its say, the dialog is shown again, or
 * nothing could ever show it (review fix).
 */
async function onNativeClose(): Promise<void> {
  if (!props.open || dialog.value?.open) return;
  emit('close');
  await nextTick();
  const el = dialog.value;
  if (props.open && el && !el.open && typeof el.showModal === 'function') {
    el.showModal();
    return;
  }
  if (!props.open) restoreFocus();
}

function onCancel(event: Event): void {
  if (!props.guardEscape) return;
  event.preventDefault();
  emit('close');
}

function onBackdropClick(event: MouseEvent): void {
  if (event.target === dialog.value) emit('close');
}
</script>

<template>
  <dialog
    ref="dialog"
    class="oma-dialog p-0"
    :aria-labelledby="titleId"
    @close="onNativeClose"
    @cancel="onCancel"
    @click="onBackdropClick"
  >
    <div class="modal-content">
      <div class="modal-header">
        <h2 :id="titleId" class="modal-title fs-5">{{ title }}</h2>
        <button type="button" class="btn-close" aria-label="Close" @click="emit('close')"></button>
      </div>
      <div class="modal-body"><slot /></div>
      <div v-if="$slots.footer" class="modal-footer"><slot name="footer" /></div>
    </div>
  </dialog>
</template>
