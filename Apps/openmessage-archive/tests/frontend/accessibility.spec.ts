// @vitest-environment node
// The accessibility pass of slice 5j (M5 §10), as far as a test without an
// engine can check it: the style sheets keep the reduced-motion reset for
// both the system setting and the viewer's own, never scroll smoothly,
// keep borders and focus visible in forced colours, keep targets at least
// 24 × 24 CSS px, and hide the viewer from print. Screen readers, forced
// colours and 400 % zoom on WebKitGTK and WebView2 are manual checks
// (docs/compatibility.md).
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const SRC = fileURLToPath(new URL('../../src/', import.meta.url));
const read = (rel: string) => readFileSync(join(SRC, rel), 'utf8');
const app = read('assets/styles/app.css');
const viewer = read('assets/styles/viewer.css');

/** The bodies of every `@media <query>` block (one level of nesting). */
function mediaBlocks(css: string, query: string): string {
  const out: string[] = [];
  let at = css.indexOf(`@media ${query}`);
  while (at >= 0) {
    let i = css.indexOf('{', at) + 1;
    let depth = 1;
    const start = i;
    while (depth > 0 && i < css.length) {
      if (css[i] === '{') depth += 1;
      else if (css[i] === '}') depth -= 1;
      i += 1;
    }
    out.push(css.slice(start, i - 1));
    at = css.indexOf(`@media ${query}`, i);
  }
  return out.join('\n');
}

function walk(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const full = join(dir, name);
    return statSync(full).isDirectory() ? walk(full) : [full];
  });
}

describe('motion', () => {
  it('honours the system setting and the viewer setting with the same reset', () => {
    const system = mediaBlocks(app, '(prefers-reduced-motion: reduce)');
    expect(system).toContain('transition-duration: 0.01ms !important');
    expect(system).toContain('animation-duration: 0.01ms !important');
    expect(app).toMatch(/\.oma-reduce-motion \*,[\s\S]*?transition-duration: 0\.01ms !important/);
  });

  it('never scrolls smoothly', () => {
    for (const f of walk(SRC).filter((x) => /\.(ts|vue|css)$/.test(x))) {
      expect(readFileSync(f, 'utf8'), f).not.toMatch(/scroll-behavior:\s*smooth|behavior:\s*['"]smooth/);
    }
    expect(viewer).toMatch(/\.oma-feed,\s*\.oma-conv-list\s*\{\s*scroll-behavior: auto;/);
  });
});

describe('forced colours', () => {
  const forced = mediaBlocks(viewer, '(forced-colors: active)');

  it('keep bubble, chip, card and badge borders in a system colour', () => {
    for (const cls of ['.oma-bubble', '.oma-chip', '.oma-file-card', '.oma-badge-inline', '.oma-masked', '.oma-cp', '.oma-menu']) {
      expect(forced, cls).toContain(cls);
    }
    expect(forced).toContain('CanvasText');
  });

  it('keep focus rings and search marks visible', () => {
    expect(forced).toMatch(/\.oma-thread-item:focus-visible[\s\S]*?outline: 2px solid Highlight/);
    expect(forced).toMatch(/mark[\s\S]*?background: Highlight/);
  });
});

describe('targets, drag and print', () => {
  it('keep inline buttons and menu items at least 24 px', () => {
    expect(viewer).toMatch(/\.oma-inline-btn \{[^}]*min-height: 24px;[^}]*min-width: 24px;/);
    expect(viewer).toMatch(/\.oma-menu-item \{[^}]*min-height: 2rem;/);
  });

  it('keep bubbles and media from being dragged out', () => {
    expect(viewer).toMatch(/\.oma-thread-item,\s*\.oma-bubble,\s*\.oma-media-box,\s*\.oma-media-full \{\s*-webkit-user-drag: none;/);
  });

  it('print only a sentence while the viewer is open', () => {
    const print = mediaBlocks(viewer, 'print');
    expect(print).toContain('html.oma-viewer-open .oma-app');
    expect(print).toContain('html.oma-viewer-open dialog');
    expect(print).toContain('html.oma-viewer-open .oma-print-note');
  });

  it('give the curtain an opaque backdrop, never a blur filter', () => {
    expect(viewer).toMatch(/\.oma-curtain::backdrop \{[^}]*background: Canvas;/);
    expect(viewer).not.toMatch(/filter:\s*blur/);
  });
});
