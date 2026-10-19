<script setup lang="ts" generic="T, K extends string | number">
// A virtualised, bidirectional list (M5 §8.4; the in-house feed of slice
// 5h0, chosen because virtua failed spike 5s(1) at scrollTop 0 on
// WebKitGTK). The geometry lives in `services/feedModel.ts`.
//
// - Only the rows near the viewport are rendered (at most `maxRows`), plus
//   every key in `keepMounted` (the focused item, the active option, a
//   playing medium); gaps of the right height stand for the rest.
// - Row heights are measured with a ResizeObserver; each row is a
//   `display: flow-root` box, so its children's margins are inside it.
// - The feed keeps an anchor (a row and its offset in the viewport). When
//   rows are prepended, appended or evicted, or a height changes, the
//   scroll position is recomputed from it after the DOM is patched and
//   before the browser paints, so the anchored row does not move. This is
//   the compensation WebKitGTK's missing scroll anchoring needs; the
//   scroller sets `overflow-anchor: none` (viewer.css) so Chromium's
//   native anchoring in WebView2 does not compensate a second time.
// - Attributes and native listeners (role, tabindex, aria-*, keydown) fall
//   through to the scroller.
//
// The slot gets `{ item, index }`. It renders no content itself.
import { computed, nextTick, onBeforeUnmount, onMounted, onUpdated, ref, shallowRef, watch } from 'vue';
import { FeedModel, type FeedAlign, type FeedAnchor, type FeedRange } from '@/services/feedModel';

const props = withDefaults(
  defineProps<{
    items: readonly T[];
    itemKey: (item: T) => K;
    /** The height assumed for a row until it is measured, in CSS pixels. */
    estimate?: number;
    keepMounted?: readonly K[];
    overscan?: number;
    /** How close to an end (in pixels) `nearStart`/`nearEnd` fire. */
    edgeDistance?: number;
    maxRows?: number;
  }>(),
  { estimate: 80, keepMounted: () => [], overscan: 600, edgeDistance: 800, maxRows: 200 },
);

const emit = defineEmits<{
  nearStart: [];
  nearEnd: [];
  visible: [range: FeedRange<K>];
}>();

defineSlots<{ default(props: { item: T; index: number }): unknown }>();

const scroller = ref<HTMLElement | null>(null);
const model = new FeedModel<K>(props.estimate);
/** Bumped whenever the model's geometry changes, so `pieces` recomputes. */
const geometry = ref(0);
const viewTop = ref(0);
const viewport = ref(0);
let anchor: FeedAnchor<K> | null = null;
let lastVisible = '';

const rowKeys = new WeakMap<Element, K>();
const rowEls = new Map<K, HTMLElement>();
const observer = shallowRef<ResizeObserver | null>(null);

function blockSize(entry: ResizeObserverEntry): number {
  const box = entry.borderBoxSize?.[0];
  if (box) return box.blockSize;
  return (entry.target as HTMLElement).offsetHeight;
}

/** Recompute the scroll position from the anchor, then apply it after the patch. */
function relayout(): void {
  if (anchor) {
    const top = model.scrollTopFor(anchor, viewport.value);
    if (top === null) anchor = null;
    else viewTop.value = top;
  }
  viewTop.value = Math.min(Math.max(viewTop.value, 0), model.maxScroll(viewport.value));
  if (!anchor) anchor = model.anchorAt(viewTop.value);
  geometry.value += 1;
  void nextTick(apply);
}

/** Write the scroll position the model wants, and report what is visible. */
function apply(): void {
  const el = scroller.value;
  if (!el) return;
  if (observer.value === null) measureWithoutObserver();
  if (Math.abs(el.scrollTop - viewTop.value) >= 1) {
    el.scrollTop = viewTop.value;
    // The engine clamps to its own scroll range (rows not yet measured can
    // make it shorter than the model's): follow it, but keep the anchor, so
    // the next measurement puts the anchored row back where it belongs.
    if (Math.abs(el.scrollTop - viewTop.value) >= 1) viewTop.value = el.scrollTop;
  }
  report();
}

function report(): void {
  const v = model.visible(viewTop.value, viewport.value);
  if (v) {
    const range: FeedRange<K> = {
      first: model.keyAt(v.first)!,
      last: model.keyAt(v.last)!,
      firstIndex: v.first,
      lastIndex: v.last,
    };
    const sig = `${v.first}:${v.last}:${String(range.first)}:${String(range.last)}`;
    if (sig !== lastVisible) {
      lastVisible = sig;
      emit('visible', range);
    }
  }
  // Fired on every report while in range: the parent decides whether a
  // load is possible (more rows, nothing loading, no failed page).
  if (model.size === 0) return;
  if (viewTop.value <= props.edgeDistance) emit('nearStart');
  if (model.total() - (viewTop.value + viewport.value) <= props.edgeDistance) emit('nearEnd');
}

watch(
  () => props.items,
  (items) => {
    if (!anchor && model.size > 0) anchor = model.anchorAt(viewTop.value);
    model.setKeys(items.map((item) => props.itemKey(item)));
    relayout();
  },
  { immediate: true, flush: 'pre' },
);

const pieces = computed(() => {
  void geometry.value;
  return model.window(viewTop.value, viewport.value, {
    overscan: props.overscan,
    keep: props.keepMounted,
    maxRows: props.maxRows,
  });
});

function onScroll(): void {
  const el = scroller.value;
  if (!el) return;
  const top = el.scrollTop;
  // Our own write echoes back as a scroll event: keep the anchor as set
  // (it may be a bottom-edge anchor from scrollToEnd).
  if (Math.abs(top - viewTop.value) < 1) return;
  viewTop.value = top;
  anchor = model.anchorAt(top);
  report();
}

function setRow(el: unknown, key: K): void {
  if (!(el instanceof HTMLElement)) return;
  const old = rowEls.get(key);
  if (old === el) return;
  if (old) observer.value?.unobserve(old);
  rowEls.set(key, el);
  rowKeys.set(el, key);
  observer.value?.observe(el);
}

/** Forget rows that left the DOM. */
function sweep(): void {
  for (const [key, el] of rowEls) {
    if (!el.isConnected) {
      observer.value?.unobserve(el);
      rowEls.delete(key);
    }
  }
}

/** Without a ResizeObserver (an old engine), measure after each patch. */
function measureWithoutObserver(): void {
  let changed = false;
  for (const [key, el] of rowEls) {
    if (el.isConnected && el.offsetHeight > 0 && model.setHeight(key, el.offsetHeight)) changed = true;
  }
  const h = scroller.value?.clientHeight ?? 0;
  if (h !== viewport.value) {
    viewport.value = h;
    changed = true;
  }
  if (changed) relayout();
}

function onResize(entries: readonly ResizeObserverEntry[]): void {
  let changed = false;
  for (const entry of entries) {
    if (entry.target === scroller.value) {
      const h = scroller.value.clientHeight;
      if (h !== viewport.value) {
        viewport.value = h;
        changed = true;
      }
      continue;
    }
    const key = rowKeys.get(entry.target);
    if (key !== undefined && model.setHeight(key, blockSize(entry))) changed = true;
  }
  if (changed) relayout();
}

onMounted(() => {
  const el = scroller.value!;
  viewport.value = el.clientHeight;
  if (typeof ResizeObserver !== 'undefined') {
    observer.value = new ResizeObserver(onResize);
    observer.value.observe(el);
    for (const row of rowEls.values()) observer.value.observe(row);
  }
  relayout();
});

onUpdated(sweep);

onBeforeUnmount(() => {
  observer.value?.disconnect();
  observer.value = null;
  rowEls.clear();
});

/** Show a row: `start`/`end` align it with that edge, `nearest` scrolls only if needed. */
function scrollToIndex(index: number, align: FeedAlign = 'start'): void {
  const key = model.keyAt(index);
  if (key === undefined) return;
  let edge: Exclude<FeedAlign, 'nearest'>;
  if (align === 'nearest') {
    const top = model.top(index);
    const bottom = model.bottom(index);
    if (top >= viewTop.value && bottom <= viewTop.value + viewport.value) {
      report();
      return;
    }
    edge = top < viewTop.value ? 'start' : 'end';
  } else {
    edge = align;
  }
  anchor = model.alignAnchor(key, edge, viewport.value);
  relayout();
}

function scrollToKey(key: K, align: FeedAlign = 'start'): void {
  scrollToIndex(model.indexOf(key), align);
}

function scrollToStart(): void {
  scrollToIndex(0, 'start');
}

function scrollToEnd(): void {
  scrollToIndex(model.size - 1, 'end');
}

/** The key of the row at the top of the viewport, or null. */
function firstVisibleKey(): K | null {
  const v = model.visible(viewTop.value, viewport.value);
  return v ? (model.keyAt(v.first) ?? null) : null;
}

/** Whether a row is in the DOM now. */
function isMounted(key: K): boolean {
  const el = rowEls.get(key);
  return !!el && el.isConnected;
}

defineExpose({ scroller, scrollToIndex, scrollToKey, scrollToStart, scrollToEnd, firstVisibleKey, isMounted });
</script>

<template>
  <div ref="scroller" class="oma-feed" @scroll.passive="onScroll">
    <template v-for="p in pieces" :key="p.id">
      <div v-if="p.kind === 'gap'" class="oma-feed-gap" :style="{ height: `${p.height}px` }" aria-hidden="true"></div>
      <div v-else :ref="(el) => setRow(el, p.key)" class="oma-feed-row">
        <slot :item="items[p.index]!" :index="p.index" />
      </div>
    </template>
  </div>
</template>
