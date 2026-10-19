// @vitest-environment node
// The frontend source rule (M5 §8.1 rule 6, I7). The project has no ESLint,
// so this test is the enforcement: it reads every file under `src/` and
// fails on HTML sinks, string evaluation, attribute bindings of anything
// but an allowlisted non-content expression, dynamic bindings and
// components, render functions, DOM writes outside SafeText, navigation
// and object URLs, casts to the content brands, and `console.` outside
// `services/devlog.ts`. Each banned form has a fixture that must fail and
// one that must pass.
//
// Attribute bindings are read from the template's AST (Vue's own SFC
// parser, the version the app is built with), not matched as text, so
// every form Vue compiles into a binding is seen: `:x`, `v-bind:x` and the
// `.x` prop shorthand, with any modifiers, any quoting, any case, and the
// same-name shorthand `:alt` (which binds the variable `alt`).
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'vue/compiler-sfc';
import { describe, expect, it } from 'vitest';

const SRC = fileURLToPath(new URL('../../src/', import.meta.url));

/** Attributes whose bindings must be a listed non-content expression (lower case, kebab case). */
const BOUND_ATTR =
  /^(?:href|src|srcset|poster|xlink:href|formaction|style|class|id|name|title|alt|aria-[\w-]+|data-[\w-]+)$/;

/** A `:src` must be exactly one `mediaUrl(…)` call (optionally `?? undefined`). */
const MEDIA_SRC = /^mediaUrl\([^()]*\)(?: \?\? undefined)?$/;

/** `ariaLabel` → `aria-label`, `HREF` → `href`: the attribute a binding reaches. */
function attributeName(arg: string): string {
  return arg.replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase();
}

/** Vue's camelize, for the same-name shorthand (`:aria-label` binds `ariaLabel`). */
function camelize(name: string): string {
  return name.replace(/-(\w)/g, (_, c: string) => c.toUpperCase());
}

interface Binding {
  /** The argument as written (`aria-label`, `title`). */
  arg: string;
  /** The expression, whitespace collapsed. */
  expr: string;
}

interface AstNode {
  type: number;
  props?: AstProp[];
  children?: AstNode[];
}

interface AstProp {
  type: number;
  name: string;
  arg?: { content: string; isStatic: boolean };
  exp?: { loc: { source: string } };
}

/**
 * Every `v-bind` of a static attribute in a `.vue` file's template, and the
 * parse errors (a template the parser cannot read is a violation).
 */
function bindings(file: string, text: string): { found: Binding[]; errors: string[] } {
  if (!/<template[\s>]/.test(text)) return { found: [], errors: [] };
  const { descriptor, errors } = parse(text, { filename: file });
  const found: Binding[] = [];
  const walk = (node: AstNode): void => {
    if (node.type === 1) {
      for (const p of node.props ?? []) {
        if (p.type !== 7 || p.name !== 'bind' || !p.arg || !p.arg.isStatic) continue;
        const source = p.exp ? p.exp.loc.source : camelize(p.arg.content);
        found.push({ arg: p.arg.content, expr: source.split(/\s+/).join(' ').trim() });
      }
    }
    for (const c of node.children ?? []) walk(c);
  };
  const ast = descriptor.template?.ast as AstNode | undefined;
  if (ast) walk(ast);
  return { found, errors: errors.map((e) => (e instanceof Error ? e.message : String(e))) };
}

/**
 * The non-content expressions each file may bind (whitespace collapsed).
 * Every one is a closed code, a constant, a computed class, a numeric or
 * boolean state, or an id built from an app-made key; none is archive
 * content. A new binding must be added here deliberately.
 */
const ALLOWED_BINDINGS: Record<string, readonly string[]> = {
  'components/AppNavbar.vue': [
    'aria-label="`Theme: ${current.label}. Switch to ${upcoming.label}.`"',
    'class="current.icon"',
    'title="`Theme: ${current.label} — switch to ${upcoming.label}`"',
  ],
  'components/ArchiveList.vue': [
    "aria-current=\"archives.selectedId === e.archiveId ? 'true' : undefined\"",
    'aria-label="STATUS_LABEL[e.status].text"',
    'class="{ active: archives.selectedId === e.archiveId }"',
  ],
  'components/ArchivePage.vue': ['aria-describedby="describedBy"'],
  'components/ArchiveStatusBadge.vue': ['class="presentation.cls"'],
  'components/BackupList.vue': [
    "aria-current=\"backups.selectedId === b.id ? 'true' : undefined\"",
    'aria-label="locationKindLabel(g.location)"',
    'class="{ active: backups.selectedId === b.id }"',
  ],
  'components/BackupPasswordDialog.vue': ['aria-invalid="!!error"'],
  'components/BackupStateBadge.vue': ['class="presentation.cls"'],
  'components/CreateArchiveDialog.vue': ['aria-invalid="!!(localError ?? archives.createError)"'],
  'components/EmptyState.vue': ['class="`bi-${icon}`"', "class=\"{ 'oma-empty-compact': compact }\""],
  'components/ExtractionReport.vue': ['class="alertClass"', "class=\"{ 'ps-3': row.indent }\""],
  'components/ImportDialog.vue': [
    "class=\"includeRecentlyDeleted ? 'alert alert-warning' : 'text-body-secondary'\"",
    'id="`import-backup-${b.id}`"',
    "title=\"continuing ? 'Continue importing' : 'Import from a backup'\"",
  ],
  'components/ImportPreflightDialog.vue': [
    'aria-labelledby="`pf-${s.id}`"',
    'id="`pf-${s.id}`"',
    'id="`pf-lineage-${l.lineageId}`"',
  ],
  'components/ImportSummary.vue': ['class="alertClass"', "class=\"{ 'ps-4': row.indent }\""],
  'components/JobProgress.vue': [
    'aria-label="ariaLabel"',
    'aria-valuenow="percent ?? undefined"',
    'aria-valuetext="label"',
    "class=\"{ 'progress-bar-striped progress-bar-animated w-100': percent === null }\"",
    'style="percent === null ? undefined : { width: `${percent}%` }"',
  ],
  'components/LocationStateBadge.vue': ['class="presentation.cls"'],
  'components/LocationStatusList.vue': ['class="ICON[check.level]"'],
  'components/ModalDialog.vue': ['aria-labelledby="titleId"', 'id="titleId"'],
  'components/PanelLayout.vue': [
    'aria-label="leftLabel"',
    'aria-label="rightLabel"',
    "class=\"{ 'has-left': layout.leftDocked, 'has-right': layout.rightDocked }\"",
    "class=\"{ 'oma-overlay': !layout.leftDocked }\"",
    "class=\"{ 'oma-overlay': !layout.rightDocked }\"",
  ],
  'components/PlannedBadge.vue': [
    'title="`${info.title} is planned for milestone ${info.milestone}: ${info.summary}`"',
  ],
  'components/ToastHost.vue': [
    "aria-live=\"toast.level === 'danger' ? 'assertive' : 'polite'\"",
    'class="ICON[toast.level]"',
    'class="`text-bg-${toast.level}`"',
    "class=\"{ 'btn-close-white': toast.level !== 'warning' }\"",
  ],
  'components/ValidationReport.vue': ["class=\"report.importable ? 'alert-success' : 'alert-danger'\""],
  'components/VerifySummary.vue': ['class="alertClass"'],
  'components/ViewerSettings.vue': ['id="`viewer-playback-${p}`"'],
  'components/viewer/SafeText.vue': ["class=\"mode === 'identifier' ? 'oma-safe-id' : 'oma-safe-body'\""],
  // Captions are named `<idBase>-a<n>`: an app-made article prefix with a
  // checked integer, and the attachment's position (5h3).
  // The Play and image buttons are `<caption id>-play` and `-open`, return
  // targets for the play flow's dialogs; `aria-disabled` is a boolean state.
  'components/viewer/AttachmentView.vue': [
    "aria-disabled=\"isChecking ? 'true' : undefined\"",
    'aria-describedby="captionId"',
    'aria-labelledby="captionId"',
    'class="boxClass"',
    'id="captionId"',
    'id="openId"',
    'id="playId"',
  ],
  // The calendar (5i): cell ids from the grid position, classes and
  // `aria-selected` from closed day states, weekday names from Intl, and
  // fixed words; none is archive content.
  'components/viewer/DateJumpDialog.vue': [
    "aria-disabled=\"nearestSeeking ? 'true' : undefined\"",
    'aria-invalid="typedBad"',
    'aria-selected="sameDay(c, focusDay)"',
    'class="cellClass(c, w * 7 + d)"',
    'id="cellId(w * 7 + d)"',
    'title="DATE_JUMP_TEXT.title"',
    'title="w.long"',
  ],
  // A result's ids are `oma-hit-<position>` (5i).
  'components/viewer/SearchHit.vue': [
    'aria-describedby="`${base}-h ${base}-t`"',
    'id="`${base}-h`"',
    'id="`${base}-t`"',
  ],
  'components/viewer/SearchPanel.vue': [
    "aria-disabled=\"state.loadingMore ? 'true' : undefined\"",
    'aria-invalid="fromInvalid"',
    'aria-invalid="toInvalid"',
    'aria-label="SEARCH_TEXT.close"',
  ],
  // Dialog titles and close labels are fixed words (5h3).
  'components/viewer/MediaViewer.vue': ['title="title"'],
  'components/viewer/MessageDetails.vue': ['aria-label="DETAILS_TEXT.close"'],
  'components/viewer/PlayConfirmDialog.vue': ['title="PLAY_CONFIRM_TEXT.title"'],
  // The menu's place in CSS px (set through the CSSOM) and a fixed title (5j).
  'components/viewer/MessageMenu.vue': ['style="position"', 'title="COPY_ACTION_TEXT.linkTitle"'],
  'components/viewer/ReplyThread.vue': ['aria-label="DETAILS_TEXT.close"'],
  // Option ids come from the position in the id snapshot; aria values are
  // counts and booleans.
  'components/viewer/ConversationList.vue': [
    'aria-activedescendant="activeOptionId"',
    'aria-posinset="index + 1"',
    'aria-selected="item === selected"',
    'aria-setsize="ids.length"',
    "class=\"{ 'oma-active': index === active, 'oma-selected': item === selected }\"",
    'id="optionId(index)"',
  ],
  // Classes from closed flags and states (5h2).
  'components/viewer/MessageBody.vue': [
    "aria-disabled=\"full?.loading === true ? 'true' : undefined\"",
    "class=\"collapsed ? 'oma-collapsed' : ''\"",
    'class="segmentClass(seg)"',
  ],
  'components/viewer/ReactionChips.vue': ['class="stateClass(chip)"'],
  'components/viewer/MessageBubble.vue': [
    "class=\"earlier.kind === 'revealed' ? 'oma-earlier oma-earlier-shown' : 'oma-earlier'\"",
  ],
  // `base` is `oma-m-<SafeId>`: an app-made prefix and a checked integer;
  // the classes come from the item's shape and direction codes.
  'components/viewer/ThreadItem.vue': [
    'aria-describedby="`${base}-b`"',
    'aria-describedby="`${base}-h`"',
    'aria-labelledby="`${base}-h`"',
    'class="itemClass"',
    'class="senderClass"',
    'id="base"',
    'id="`${base}-b`"',
    'id="`${base}-h`"',
  ],
  'components/viewer/ThreadView.vue': ['aria-busy="loading !== null"'],
  // A gap's height in pixels, set through the CSSOM (allowed by `style-src 'self'`).
  'components/viewer/VirtualFeed.vue': ['style="{ height: `${p.height}px` }"'],
  'views/ArchiveView.vue': [
    'aria-pressed="layout.leftVisible"',
    'aria-pressed="layout.rightVisible"',
    "title=\"`Show or hide archives (${keysFor('toggle-left-panel')})`\"",
    "title=\"`Show or hide details (${keysFor('toggle-right-panel')})`\"",
  ],
  'views/BackupsView.vue': [
    'aria-describedby="`${describedBy} backup-extraction-note`"',
    'aria-describedby="`${describedBy} backup-import-note`"',
    'aria-describedby="describedBy"',
    'aria-pressed="layout.leftVisible"',
    'aria-pressed="layout.rightVisible"',
    "title=\"`Show or hide locations (${keysFor('toggle-right-panel')})`\"",
    "title=\"`Show or hide the backup list (${keysFor('toggle-left-panel')})`\"",
  ],
  'views/SettingsView.vue': ['class="t.icon"', 'id="`theme-${t.value}`"'],
  'views/ViewerView.vue': [
    'aria-pressed="layout.leftVisible"',
    'aria-pressed="layout.rightVisible"',
    'title="THREAD_TEXT.chooseConversation"',
  ],
};

/** Files allowed to call render functions (none yet). */
const RENDER_FUNCTION_FILES: readonly string[] = [];
/** The only file that may write DOM text properties directly. */
const SAFE_TEXT = 'components/viewer/SafeText.vue';
/** The only file that may use the console. */
const DEVLOG = 'services/devlog.ts';

interface Rule {
  id: string;
  pattern: RegExp;
  /** Files (relative to `src/`, `/` separators) the rule does not apply to. */
  except?: readonly string[];
}

const RULES: readonly Rule[] = [
  {
    id: 'html-sink',
    pattern:
      /v-html|\binnerHTML\b|\bouterHTML\b|\binsertAdjacentHTML\b|\bdocument\.write(?:ln)?\b|\bsrcdoc\b|\bDOMParser\b|\bcreateContextualFragment\b/,
  },
  {
    id: 'string-eval',
    pattern: /\bnew\s+Function\b|\beval\s*\(|\bset(?:Timeout|Interval)\s*\(\s*['"`]/,
  },
  {
    id: 'dynamic-binding',
    pattern: /\sv-bind\s*=|\s(?::|v-bind:|@|v-on:)\[|\s(?::|v-bind:)is\s*=|\sis\s*=\s*"vue:|<component\b/,
  },
  {
    id: 'render-function',
    pattern: /\bh\s*\(|\brender\s*\(|\b(?:createVNode|createElementVNode|createBlock|createElementBlock|resolveDynamicComponent)\s*\(/,
    except: RENDER_FUNCTION_FILES,
  },
  {
    id: 'dom-write',
    pattern: /\bsetAttribute(?:NS)?\s*\(|\.(?:src|href|srcset|innerText|textContent)\s*=(?!=)/,
    except: [SAFE_TEXT],
  },
  {
    id: 'navigation',
    pattern:
      /\bURL\.createObjectURL\b|\bwindow\.open\b|\blocation\.assign\b|\blocation\.replace\b|(?<!(?:const|let|var)\s+)(?<![\w$.])location(?:\.href)?\s*=(?!=)|\.location(?:\.href)?\s*=(?!=)/,
  },
  {
    id: 'brand-cast',
    // `x as Brand`, and the angle-bracket cast `<Brand>x` (valid in `.ts`
    // and `lang="ts"`); a generic argument (`Set<SafeId>(…)`, `Array<SafeId>`)
    // follows a name or a bracket, a cast never does.
    pattern:
      /\bas\s+(?:UntrustedText|MediaToken|MediaBase|SafeId|DecimalString)\b|(?:(?<![\w$)\]]\s*)|\b(?:return|yield|await|throw|case|typeof|void|in|of|else)\s*)<(?:UntrustedText|MediaToken|MediaBase|SafeId|DecimalString)>\s*[\w($]/,
  },
  { id: 'console', pattern: /\bconsole\./, except: [DEVLOG] },
];

/**
 * Remove comments, so documentation may name what the rule bans: HTML
 * comments, block comments, and whole-line `//` comments. A trailing `//`
 * after code is kept, because a string such as a URL scheme could contain
 * one; such a comment must not name a banned form.
 */
function stripComments(text: string): string {
  return text
    .replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/^[ \t]*\/\/.*$/gm, '');
}

/** Every violation in one file's text. `file` is relative to `src/`. */
function violations(file: string, text: string): string[] {
  const code = stripComments(text);
  const found: string[] = [];
  for (const rule of RULES) {
    if (rule.except?.includes(file)) continue;
    const m = rule.pattern.exec(code);
    if (m) found.push(`${rule.id}: ${m[0].trim()}`);
  }
  if (file.endsWith('.vue')) {
    const allowed = new Set(ALLOWED_BINDINGS[file] ?? []);
    const { found: bound, errors } = bindings(file, text);
    for (const e of errors) found.push(`template-parse: ${e}`);
    for (const { arg, expr } of bound) {
      const attr = attributeName(arg);
      if (!BOUND_ATTR.test(attr)) continue;
      if (attr === 'src') {
        if (!MEDIA_SRC.test(expr)) found.push(`src-binding: :src="${expr}"`);
        continue;
      }
      if (!allowed.has(`${arg}="${expr}"`)) found.push(`attribute-binding: :${arg}="${expr}"`);
    }
  }
  return found;
}

function walk(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const full = join(dir, name);
    return statSync(full).isDirectory() ? walk(full) : [full];
  });
}

const FILES = walk(SRC)
  .filter((f) => /\.(ts|vue|js|mjs)$/.test(f))
  .map((f) => ({ file: relative(SRC, f).split(sep).join('/'), text: readFileSync(f, 'utf8') }));

describe('the source rule over src/', () => {
  it('reads the source tree', () => {
    expect(FILES.length).toBeGreaterThan(50);
    expect(FILES.some((f) => f.file === SAFE_TEXT)).toBe(true);
    expect(FILES.some((f) => f.file === DEVLOG)).toBe(true);
  });

  it('finds no violation in any file', () => {
    const all = FILES.flatMap(({ file, text }) => violations(file, text).map((v) => `${file}: ${v}`));
    expect(all).toEqual([]);
  });

  it('has no stale allowlist entry', () => {
    const used = new Set<string>();
    for (const { file, text } of FILES) {
      if (!file.endsWith('.vue')) continue;
      for (const { arg, expr } of bindings(file, text).found) used.add(`${file} ${arg}="${expr}"`);
    }
    const stale = Object.entries(ALLOWED_BINDINGS).flatMap(([file, list]) =>
      list.filter((b) => !used.has(`${file} ${b}`)).map((b) => `${file} ${b}`),
    );
    expect(stale).toEqual([]);
  });
});

describe('each banned form', () => {
  // [banned fixture, a near miss that must pass]
  const TS_CASES: [string, string, string][] = [
    ['html-sink', 'el.innerHTML = x;', 'const innerHtmlNote = 1;'],
    ['html-sink', 'el.outerHTML', 'const outer = 1;'],
    ['html-sink', "el.insertAdjacentHTML('beforeend', x)", 'el.insertAdjacentElement(a, b)'],
    ['html-sink', 'document.write(x)', 'document.title'],
    ['html-sink', 'frame.srcdoc = x', 'const src = 1;'],
    ['html-sink', 'new DOMParser()', 'new Map()'],
    ['html-sink', 'range.createContextualFragment(x)', 'range.cloneContents()'],
    ['string-eval', 'new Function("return 1")', 'function f() {}'],
    ['string-eval', 'eval(code)', 'evaluate(code)'],
    ['string-eval', "setTimeout('go()', 5)", 'setTimeout(go, 5)'],
    ['string-eval', 'setInterval(`go()`, 5)', 'setInterval(() => go(), 5)'],
    ['render-function', "return h('div')", 'return each(x)'],
    ['render-function', 'render(vnode, el)', 'const rendered = 1;'],
    ['render-function', "createVNode('a', { href: row.url })", 'const vnodeCount = 1;'],
    ['render-function', "createElementVNode('a', p)", 'createElement(a)'],
    ['render-function', 'createBlock(c)', 'const blocks = 1;'],
    ['render-function', 'resolveDynamicComponent(name)', 'resolveComponent(name)'],
    ['dom-write', "el.setAttribute('title', x)", "el.getAttribute('title')"],
    ['dom-write', 'img.src = url', 'const src = url;'],
    ['dom-write', 'a.href = url', 'if (a.href === url) go();'],
    ['dom-write', 'img.srcset = x', 'img.width = 3'],
    ['dom-write', 'el.innerText = x', 'const t = el.innerText;'],
    ['dom-write', 'el.textContent = x', 'const t = el.textContent;'],
    ['navigation', 'URL.createObjectURL(blob)', 'new URL(a)'],
    ['navigation', 'window.open(u)', 'window.addEventListener(a, b)'],
    ['navigation', 'location.assign(u)', 'const location = ref(1);'],
    ['navigation', 'location = u', 'let location = 1;'],
    ['navigation', 'location.href = u', 'if (location.href === u) go();'],
    ['navigation', 'window.location = u', 'const x = window.location.hash;'],
    ['brand-cast', 'x as UntrustedText', 'x as string'],
    ['brand-cast', 'x as MediaToken', 'x as MediaTokenish'],
    ['brand-cast', 'x as SafeId', 'x satisfies number'],
    ['brand-cast', 'const id = <SafeId>raw;', 'const ids = new Set<SafeId>();'],
    ['brand-cast', 'f(<UntrustedText>(x))', 'const list: Array<UntrustedText> = [];'],
    ['brand-cast', 'return <DecimalString>s;', 'function f(): Promise<DecimalString> {}'],
    ['console', 'console.log(x)', 'devNote("jobIdMismatch")'],
  ];

  for (const [id, bad, good] of TS_CASES) {
    it(`${id}: fails on \`${bad}\` and passes \`${good}\``, () => {
      expect(violations('services/fixture.ts', bad).join()).toContain(id);
      expect(violations('services/fixture.ts', good)).toEqual([]);
    });
  }

  const VUE_CASES: [string, string, string][] = [
    ['html-sink', '<div v-html="x"></div>', '<div>{{ x }}</div>'],
    ['dynamic-binding', '<div v-bind="attrs"></div>', '<div :key="k"></div>'],
    ['dynamic-binding', '<div :[name]="x"></div>', '<div :disabled="x"></div>'],
    ['dynamic-binding', '<div @[event]="x"></div>', '<div @click="x"></div>'],
    ['dynamic-binding', '<div :is="x"></div>', '<div :dir="x"></div>'],
    ['dynamic-binding', '<component :is="x" />', '<span />'],
    ['src-binding', '<img :src="row.url" alt="" />', '<img :src="mediaUrl(base, token, \'thumb\')" alt="" />'],
    ['src-binding', '<img :src="mediaUrl(a) || row.url" alt="" />', '<img :src="mediaUrl(a) ?? undefined" alt="" />'],
    ['src-binding', '<img :src.attr="row.url" alt="" />', '<img src="" alt="" />'],
    ['src-binding', "<img :src='row.url' alt='' />", "<img src='' alt='' />"],
    ['attribute-binding', '<img alt="" :alt="row.name" />', '<img alt="" />'],
    ['attribute-binding', '<a :href="u">x</a>', '<a href="#main">x</a>'],
    ['attribute-binding', '<video :poster="u"></video>', '<video preload="none"></video>'],
    ['attribute-binding', '<img :srcset="u" />', '<img srcset="" />'],
    ['attribute-binding', '<svg><use :xlink:href="u" /></svg>', '<svg></svg>'],
    ['attribute-binding', '<button :formaction="u"></button>', '<button></button>'],
    ['attribute-binding', '<div :style="s"></div>', '<div style="color: red"></div>'],
    ['attribute-binding', '<div :class="name"></div>', '<div class="x"></div>'],
    ['attribute-binding', '<div :id="name"></div>', '<div id="x"></div>'],
    ['attribute-binding', '<input :name="n" />', '<input name="n" />'],
    ['attribute-binding', '<div :title="label"></div>', '<div title="Fixed"></div>'],
    ['attribute-binding', '<div v-bind:title="label"></div>', '<div title="Fixed"></div>'],
    ['attribute-binding', '<div :aria-label="label"></div>', '<div aria-labelledby="x"></div>'],
    ['attribute-binding', '<div :data-name="label"></div>', '<div data-x="1"></div>'],
    // Forms Vue 3.5 compiles into bindings that a text match missed.
    ['attribute-binding', '<img alt="" :alt>', '<img alt="" />'],
    ['attribute-binding', '<div :aria-label></div>', '<div aria-label="Fixed"></div>'],
    ['attribute-binding', "<div :title='label'></div>", "<div title='Fixed'></div>"],
    ['attribute-binding', '<div :title.prop="label"></div>', '<div :tabindex.prop="0"></div>'],
    ['attribute-binding', '<div .title="label"></div>', '<div .scrollTop="0"></div>'],
    ['attribute-binding', '<a :HREF="u">x</a>', '<a HREF="#main">x</a>'],
    ['attribute-binding', '<div :ariaLabel="label"></div>', '<div :tabIndex="0"></div>'],
    ['attribute-binding', '<template v-if="a"><p :title="label" /></template>', '<template v-if="a"><p /></template>'],
  ];

  for (const [id, bad, good] of VUE_CASES) {
    it(`${id}: fails on \`${bad}\` and passes \`${good}\``, () => {
      expect(violations('components/Fixture.vue', `<template>${bad}</template>`).join()).toContain(id);
      expect(violations('components/Fixture.vue', `<template>${good}</template>`)).toEqual([]);
    });
  }

  it('allows the console only in devlog.ts and DOM text writes only in SafeText', () => {
    expect(violations(DEVLOG, 'console.debug(x)')).toEqual([]);
    expect(violations(SAFE_TEXT, 'el.textContent = x')).toEqual([]);
    expect(violations('services/other.ts', 'el.textContent = x')).not.toEqual([]);
  });

  it('ignores comments that name a banned form', () => {
    expect(violations('services/fixture.ts', '// never v-html or console.log here\nconst a = 1;')).toEqual(
      [],
    );
    expect(violations('components/Fixture.vue', '<template><!-- no v-html --><p /></template>')).toEqual(
      [],
    );
    expect(violations('services/fixture.ts', '/* innerHTML */ const a = 1;')).toEqual([]);
  });
});
