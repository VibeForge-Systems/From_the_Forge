// The privacy screen (M5 §9.2, D108), slice 5j. It hides the screen; it is
// not a lock, and every sentence that describes it says so.
//
// - **Hide** (Ctrl+Shift+H, or "Hide screen" in the viewer's toolbar):
//   every `<audio>`/`<video>` is paused and loses its source, fullscreen is
//   left, every viewer dialog closes, the viewer's content is unmounted
//   (`ViewerView` renders nothing of it while `hidden`), Rust drops its
//   cached renditions (`hide_viewer`, D120), and an opaque curtain opens
//   last, as a modal `<dialog>` (`components/PrivacyCurtain.vue`), with
//   focus on "Show". There is no hover reveal.
// - **Blur on focus loss** (opt-in): the window's `blur` or the document
//   becoming hidden hides it. Window previews may capture a frame first.
// - **Hide on idle** (opt-in, 1–30 minutes without a key, pointer or wheel
//   event): hides it and closes the viewer session (readers, tokens, the
//   renditions, search, helpers); the store forgets everything. "Show"
//   opens the archive again at the same conversation and message.
// - **Show** remounts at the same anchor: the open conversation opens
//   around the message that was focused (focused again) or, without one,
//   the first one in view (shown, not focused). The thread names that
//   place through `setPlaceProvider` while it is mounted. A focus loss
//   while "Show" is still reopening the session is remembered: the screen
//   then stays hidden (reason `focusLoss`) instead of showing the
//   conversations in a window that no longer has focus.
//
// It works only while the viewer page is mounted (`attach`/`detach`) and a
// session is open. It keeps ids only (the archive id, a chat and a message
// id), in memory, and never persists anything.
import { defineStore } from 'pinia';
import { ref } from 'vue';
import type { SafeId } from '@/types';
import { useSettings } from './settings';
import { useViewer } from './viewer';

export type HideReason = 'manual' | 'focusLoss' | 'idle';

/** Where "Show" returns to. */
export interface Place {
  archiveId: string;
  chatId: SafeId | null;
  message: SafeId | null;
  /** Keyboard focus was on that message. */
  focus: boolean;
}

/** What the open thread reports when the screen is hidden. */
export interface ThreadPlace {
  chatId: SafeId;
  message: SafeId | null;
  focus: boolean;
}

/** How often the idle timer looks at the last activity. */
export const IDLE_CHECK_MS = 5_000;

/** What counts as activity for idle hide. */
export const ACTIVITY_EVENTS = ['keydown', 'pointerdown', 'pointermove', 'wheel', 'touchstart'] as const;

/**
 * Stop every medium in the document: pause it and release its source, so
 * nothing keeps playing or buffering behind the curtain; leave fullscreen.
 */
export function stopAllMedia(root: ParentNode = document): void {
  for (const el of Array.from(root.querySelectorAll('audio, video'))) {
    const m = el as HTMLMediaElement;
    try {
      m.pause();
      m.removeAttribute('src');
      m.load();
    } catch {
      // An engine without media support has nothing playing.
    }
  }
  if (typeof document !== 'undefined' && document.fullscreenElement && typeof document.exitFullscreen === 'function') {
    void document.exitFullscreen().catch(() => undefined);
  }
  const session = typeof navigator !== 'undefined' ? navigator.mediaSession : undefined;
  if (session) session.metadata = null;
}

export const usePrivacy = defineStore('privacy', () => {
  const hidden = ref(false);
  const reason = ref<HideReason>('manual');
  /** Idle hide closed the session; "Show" opens it again. */
  const closed = ref(false);
  /** "Show" is opening the session again. */
  const showing = ref(false);
  /** The viewer page is mounted. */
  const attached = ref(false);

  let place: Place | null = null;
  /** The window lost focus while "Show" was reopening the session. */
  let lostWhileShowing = false;
  let provider: (() => ThreadPlace | null) | null = null;
  let lastActivity = Date.now();
  let timer: ReturnType<typeof setInterval> | null = null;
  let removeListeners: (() => void) | null = null;

  function idleMinutes(): number {
    return useSettings().viewer.idleHideMinutes;
  }

  /** Hide the screen (or, already hidden, record a stronger reason). */
  async function hide(why: HideReason): Promise<void> {
    const viewer = useViewer();
    if (!attached.value) return;
    if (!hidden.value) {
      if (viewer.session === null || viewer.archiveId === null) return;
      const at = provider?.() ?? null;
      place = {
        archiveId: viewer.archiveId,
        chatId: at?.chatId ?? viewer.openChatId,
        message: at?.message ?? null,
        focus: at?.focus === true && at.message !== null,
      };
      stopAllMedia();
      viewer.closeDialogs();
      reason.value = why;
      hidden.value = true;
      void viewer.hideContent();
    } else if (why === 'idle') {
      reason.value = 'idle';
    } else if (why === 'focusLoss' && showing.value) {
      // "Show" is still opening the session: it must not end by showing.
      lostWhileShowing = true;
    }
    if (why === 'idle' && !closed.value) {
      closed.value = true;
      await viewer.close();
    }
  }

  /**
   * The open thread names the message to return to (the focused one, or
   * the first in view); `null` when it unmounts. Only the provider that is
   * set may clear itself.
   */
  function setPlaceProvider(next: (() => ThreadPlace | null) | null, previous?: () => ThreadPlace | null): void {
    if (next === null && previous !== undefined && provider !== previous) return;
    provider = next;
  }

  /** Show the screen again, at the same place. */
  async function show(): Promise<void> {
    if (!hidden.value || showing.value) return;
    const viewer = useViewer();
    const p = place;
    showing.value = true;
    lostWhileShowing = false;
    try {
      if (closed.value && p) await viewer.open(p.archiveId);
      if (p && p.chatId !== null && p.message !== null && viewer.session) {
        viewer.focusOnOpen(p.chatId, p.message, p.focus);
      }
    } finally {
      showing.value = false;
    }
    if (lostWhileShowing) {
      // The window lost focus during the reopen: keep the curtain up. The
      // session is open now, so the next "Show" only lowers it.
      lostWhileShowing = false;
      if (viewer.session) closed.value = false;
      reason.value = 'focusLoss';
      return;
    }
    hidden.value = false;
    closed.value = false;
    reason.value = 'manual';
    place = null;
    lastActivity = Date.now();
    // Without a message to focus, focus goes to the main content.
    if (!p?.focus && typeof document !== 'undefined') {
      setTimeout(() => document.getElementById('main-content')?.focus({ preventScroll: true }), 0);
    }
  }

  function onActivity(): void {
    lastActivity = Date.now();
  }

  function onBlur(): void {
    if (useSettings().viewer.blurOnFocusLoss) void hide('focusLoss');
  }

  function onVisibility(): void {
    if (document.visibilityState === 'hidden') onBlur();
  }

  function checkIdle(): void {
    const minutes = idleMinutes();
    if (minutes <= 0 || closed.value) return;
    if (Date.now() - lastActivity < minutes * 60_000) return;
    if (!hidden.value && useViewer().session === null) return;
    void hide('idle');
  }

  /** The viewer page is mounted: listen for focus loss and activity. */
  function attach(): void {
    if (attached.value) return;
    attached.value = true;
    lastActivity = Date.now();
    for (const e of ACTIVITY_EVENTS) window.addEventListener(e, onActivity, { capture: true, passive: true });
    window.addEventListener('blur', onBlur);
    document.addEventListener('visibilitychange', onVisibility);
    timer = setInterval(checkIdle, IDLE_CHECK_MS);
    removeListeners = () => {
      for (const e of ACTIVITY_EVENTS) window.removeEventListener(e, onActivity, { capture: true });
      window.removeEventListener('blur', onBlur);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }

  /** The viewer page is gone: the curtain goes with it. */
  function detach(): void {
    attached.value = false;
    removeListeners?.();
    removeListeners = null;
    if (timer !== null) clearInterval(timer);
    timer = null;
    hidden.value = false;
    closed.value = false;
    showing.value = false;
    lostWhileShowing = false;
    reason.value = 'manual';
    place = null;
  }

  return { hidden, reason, closed, showing, attached, hide, show, setPlaceProvider, attach, detach };
});
