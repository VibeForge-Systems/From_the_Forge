<script setup lang="ts">
// The message menu (M5 §6.7, §9.3, D107), slice 5j: the app's own context
// menu on a message, opened by a right click, the context-menu key or
// Shift+F10 on a focused article. It replaces the engine's menu, which
// would offer "Save image as" and "Copy link". Mounted once, in the viewer
// page.
//
// - It is a small modal `<dialog>` at the pointer (or at the article for
//   the keyboard), holding an APG menu: Up, Down, Home and End move, Enter
//   or Space choose, Escape and Tab close, and focus returns to the
//   message.
// - Copy happens only on a choice, through `copyText` (the async clipboard
//   API, or `execCommand('copy')` where it is missing); the toast says
//   only "Copied" or "could not be copied", never what.
// - "Copy link" first shows the exact string in identifier mode (hidden
//   characters as visible codes), with Copy and Cancel; a link with hidden
//   characters, or one not complete here, is refused with a sentence.
import { computed, nextTick, ref, watch } from 'vue';
import ModalDialog from '@/components/ModalDialog.vue';
import SafeText from './SafeText.vue';
import {
  bodyText,
  linkRuns,
  menuItems,
  withSenderAndTime,
  withoutHiddenCharacters,
  type MenuItem,
} from '@/services/copyText';
import { copyText } from '@/services/features';
import { COPY_ACTION_TEXT } from '@/services/viewerText';
import { useNotifications } from '@/stores/notifications';
import { useSettings } from '@/stores/settings';
import { useViewer } from '@/stores/viewer';

const viewer = useViewer();
const settings = useSettings();

/** Room kept between the menu and the window's edges, in CSS px. */
const EDGE = 8;

const dialog = ref<HTMLDialogElement | null>(null);
const menuEl = ref<HTMLElement | null>(null);
const left = ref(0);
const top = ref(0);

const state = computed(() => viewer.menu);
const items = computed<MenuItem[]>(() => (state.value ? menuItems(state.value.message) : []));
const position = computed(() => ({ left: `${left.value}px`, top: `${top.value}px` }));

const link = computed(() => viewer.linkCopy);
const linkUsable = computed(() => !!link.value && !link.value.link.hidden && link.value.link.complete);

function returnFocus(id: string | undefined): void {
  if (id) document.getElementById(id)?.focus({ preventScroll: true });
}

function buttons(): HTMLButtonElement[] {
  return Array.from(menuEl.value?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? []);
}

watch(
  state,
  async (now) => {
    const el = dialog.value;
    if (!el) return;
    if (!now) {
      if (el.open) el.close();
      return;
    }
    left.value = now.x;
    top.value = now.y;
    if (!el.open && typeof el.showModal === 'function') el.showModal();
    await nextTick();
    // Keep the whole menu inside the window (it may be zoomed to 400 %).
    const rect = el.getBoundingClientRect();
    left.value = Math.max(EDGE, Math.min(now.x, window.innerWidth - rect.width - EDGE));
    top.value = Math.max(EDGE, Math.min(now.y, window.innerHeight - rect.height - EDGE));
    buttons()[0]?.focus();
  },
  { flush: 'post' },
);

function close(): void {
  const back = state.value?.returnTo;
  viewer.closeMenu();
  returnFocus(back);
}

function onNativeClose(): void {
  if (viewer.menu) close();
}

function onBackdrop(event: MouseEvent): void {
  if (event.target === dialog.value) close();
}

function onKeydown(event: KeyboardEvent): void {
  const all = buttons();
  const at = all.indexOf(document.activeElement as HTMLButtonElement);
  let next: number | null = null;
  if (event.key === 'ArrowDown') next = at < 0 || at === all.length - 1 ? 0 : at + 1;
  else if (event.key === 'ArrowUp') next = at <= 0 ? all.length - 1 : at - 1;
  else if (event.key === 'Home') next = 0;
  else if (event.key === 'End') next = all.length - 1;
  else if (event.key === 'Tab') {
    event.preventDefault();
    close();
    return;
  }
  if (next === null) return;
  event.preventDefault();
  all[next]?.focus();
}

/** Copy, say whether it worked (never what), and give focus back to the message. */
async function copy(text: string, back: string): Promise<void> {
  const ok = await copyText(text);
  const notifications = useNotifications();
  if (ok) notifications.push('success', COPY_ACTION_TEXT.copied);
  else notifications.push('danger', COPY_ACTION_TEXT.copyFailed);
  // The degraded path selects a hidden field, which takes focus.
  returnFocus(back);
}

async function choose(item: MenuItem): Promise<void> {
  const s = state.value;
  if (!s) return;
  const m = s.message;
  const body = m.body;
  const full = body.kind === 'text' ? viewer.fullTextFor(body.body, m.id) : null;
  const text = bodyText(m, full);
  switch (item.action.kind) {
    case 'details':
      close();
      viewer.openDetails(m.id, false, s.returnTo);
      return;
    case 'copyLink': {
      const run = linkRuns(m)[item.action.index];
      if (run) viewer.openLinkCopy({ link: run, returnTo: s.returnTo });
      else close();
      return;
    }
    case 'copyText':
      close();
      if (text !== null) await copy(text, s.returnTo);
      return;
    case 'copyClean':
      close();
      if (text !== null) await copy(withoutHiddenCharacters(text), s.returnTo);
      return;
    case 'copyWithSender':
      close();
      if (text !== null) {
        await copy(withSenderAndTime(m, text, { handles: s.handles, prefs: settings.viewer }), s.returnTo);
      }
      return;
  }
}

function closeLink(): void {
  const back = link.value?.returnTo;
  viewer.closeLinkCopy();
  returnFocus(back);
}

async function copyLink(): Promise<void> {
  const l = link.value;
  if (!l || !linkUsable.value) return;
  closeLink();
  await copy(l.link.text, l.returnTo);
}
</script>

<template>
  <dialog
    ref="dialog"
    class="oma-menu"
    :style="position"
    aria-label="Message actions"
    @close="onNativeClose"
    @click="onBackdrop"
  >
    <div ref="menuEl" role="menu" aria-label="Message actions" class="oma-menu-list" @keydown="onKeydown">
      <button
        v-for="(item, i) in items"
        :key="i"
        type="button"
        role="menuitem"
        tabindex="-1"
        class="oma-menu-item"
        @click="choose(item)"
      >
        {{ item.label }}
      </button>
    </div>
  </dialog>

  <ModalDialog :open="link !== null" :title="COPY_ACTION_TEXT.linkTitle" @close="closeLink">
    <template v-if="link">
      <p v-if="link.link.hidden">{{ COPY_ACTION_TEXT.linkHidden }}</p>
      <p v-else-if="!link.link.complete">{{ COPY_ACTION_TEXT.linkIncomplete }}</p>
      <template v-else>
        <p class="small">{{ COPY_ACTION_TEXT.linkExact }}</p>
        <p class="oma-link-exact">
          <SafeText :pieces="link.link.pieces" mode="identifier" ltr />
        </p>
      </template>
    </template>
    <template #footer>
      <template v-if="linkUsable">
        <button type="button" class="btn btn-outline-secondary" @click="closeLink">{{ COPY_ACTION_TEXT.cancel }}</button>
        <button type="button" class="btn btn-primary" @click="copyLink">{{ COPY_ACTION_TEXT.copy }}</button>
      </template>
      <button v-else type="button" class="btn btn-primary" @click="closeLink">{{ COPY_ACTION_TEXT.close }}</button>
    </template>
  </ModalDialog>
</template>
