// Focus that a control's own action would drop (M5 §10; review fix). A
// button that is removed or replaced by what it did leaves focus on
// `<body>`, and the next Tab starts again at the skip link. These helpers
// put it back on the closest still-connected landmark the user was in: the
// message's article, or a fallback element.
import { nextTick } from 'vue';

/** Focus was lost: nothing, or `<body>`, has it. */
export function focusLost(): boolean {
  if (typeof document === 'undefined') return false;
  const now = document.activeElement;
  return now === null || now === document.body;
}

/**
 * After the next render, when focus was lost, focus the article that held
 * `from` (captured before the action), or `fallback`.
 */
export function keepFocusNear(from: Element | null, fallback: HTMLElement | null = null): void {
  const article = from?.closest('article') ?? null;
  void nextTick(() => {
    if (!focusLost()) return;
    const target = article instanceof HTMLElement && article.isConnected ? article : fallback;
    if (target && target.isConnected) target.focus({ preventScroll: true });
  });
}
