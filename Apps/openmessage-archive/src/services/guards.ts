// The viewer's page guards (M5 §6.7, D106), slice 5j. They close the exits
// a web engine offers in the page itself; Rust already refuses downloads,
// new windows and permission requests (the window built in `window.rs`).
//
// - **Accelerators:** Ctrl+P (print, which can "Save as PDF"), Ctrl+S,
//   F5 and Ctrl+R (reload) get `preventDefault`, in text fields too. This
//   works only where the engine lets the page see the key first, which is
//   UNVERIFIED on WebKitGTK and WebView2 (docs/compatibility.md); if an
//   engine prints anyway, the print style sheet shows a sentence instead
//   of the viewer (`html.oma-viewer-open` in `viewer.css`).
// - **Context menu:** the engine's menu ("Save image as", "Copy link") is
//   prevented except in text fields; messages open the app's own menu
//   (copy actions, details).
// - **Drag-out:** dragging anything out of the viewer is prevented except
//   from text fields; images and bubbles are also `draggable="false"` and
//   `-webkit-user-drag: none`.
// - **Reloads** drop the frontend state while the Rust session lives on;
//   opening the viewer always calls `open_viewer` afresh, which closes the
//   old session and its tokens.
import { isMac, isTextInput } from './shortcuts';

export type GuardedKey = 'print' | 'save' | 'reload';

/** The browser accelerator a key event would trigger, or null. */
export function guardedKey(event: KeyboardEvent, mac = isMac()): GuardedKey | null {
  const key = event.key.toLowerCase();
  if (key === 'f5') return 'reload';
  const mod = mac ? event.metaKey : event.ctrlKey;
  if (!mod || event.altKey) return null;
  if (key === 'p') return 'print';
  if (key === 's') return 'save';
  if (key === 'r') return 'reload';
  return null;
}

/** The class on `<html>` while the viewer page is mounted (print style sheet). */
export const VIEWER_OPEN_CLASS = 'oma-viewer-open';

/**
 * Install the guards for the viewer page; returns their removal. Key
 * events are taken in the capture phase, before any other handler.
 */
export function installViewerGuards(win: Window = window): () => void {
  const doc = win.document;
  const onKey = (event: KeyboardEvent) => {
    if (guardedKey(event) !== null) event.preventDefault();
  };
  const onMenu = (event: MouseEvent) => {
    if (!isTextInput(event.target)) event.preventDefault();
  };
  const onDrag = (event: DragEvent) => {
    if (!isTextInput(event.target)) event.preventDefault();
  };
  win.addEventListener('keydown', onKey, true);
  doc.addEventListener('contextmenu', onMenu);
  doc.addEventListener('dragstart', onDrag, true);
  doc.documentElement.classList.add(VIEWER_OPEN_CLASS);
  return () => {
    win.removeEventListener('keydown', onKey, true);
    doc.removeEventListener('contextmenu', onMenu);
    doc.removeEventListener('dragstart', onDrag, true);
    doc.documentElement.classList.remove(VIEWER_OPEN_CLASS);
  };
}
