<script setup lang="ts">
// "Jump to a date" (M5 §8.7, slice 5i): the APG date-picker dialog grid
// for one conversation, plus a typed `YYYY-MM-DD` field, because popup
// support for `<input type="date">` on WebKitGTK is UNVERIFIED.
//
// - One month at a time: the 6-week grid's 43 local-day boundaries come
//   from the injected `DayBoundaryBuilder` (the system zone and its
//   history, or UTC; a fake in tests) and go to `get_day_counts`, which
//   counts visible items (folded reactions and undated rows never count).
//   Days with items are marked by a dot and by text, not by colour alone.
// - Keyboard (APG): arrows move by day and week, Home and End to the
//   week's ends, Page Up and Page Down by month (with Shift, by year);
//   Enter or Space picks the day.
// - Picking a day counts that one day again; with items, `seek_date`
//   finds its first visible item and the thread opens around it. "No
//   messages on this day" offers the nearest earlier and later items.
// - Rust stays zone-free (D104). Undated items are never on the calendar.
// - A day the zone skipped entirely is an empty interval: it shows no
//   items, and Rust is sent only strictly increasing bounds.
// - Closing the dialog cancels a pick still being answered: the thread
//   never jumps after the user closed it. "Earlier" and "Later" stay in
//   place (disabled) while they seek, so focus is not lost.
import { computed, nextTick, ref, watch } from 'vue';
import type { SafeId } from '@/types';
import ModalDialog from '@/components/ModalDialog.vue';
import {
  addDays,
  addMonths,
  civilOf,
  dayRange,
  endOfWeek,
  fullDayLabel,
  monthGrid,
  monthLabel,
  parseIsoDate,
  sameDay,
  startOfWeek,
  weekdayLabels,
  type CivilDate,
} from '@/services/calendar';
import { countsPerDay, dayBoundaryFor, strictBounds, type DayBoundaryBuilder } from '@/services/time';
import { DATE_JUMP_TEXT, fill } from '@/services/viewerText';
import { useSettings } from '@/stores/settings';
import { useViewer } from '@/stores/viewer';

const props = defineProps<{
  open: boolean;
  chatId: SafeId;
  /** An instant to start on (the conversation's last dated item); now when null. */
  initial: number | null;
  build?: DayBoundaryBuilder;
  /** The clock, for tests. */
  now?: () => number;
}>();
const emit = defineEmits<{ close: []; jump: [messageId: SafeId] }>();
const viewer = useViewer();
const settings = useSettings();

const build = computed(() => props.build ?? dayBoundaryFor(settings.viewer.timeZone));
const zone = computed(() => settings.viewer.timeZone);
const WEEKDAYS = weekdayLabels();

const focusDay = ref<CivilDate>({ year: 2026, month: 0, day: 1 });
const shown = ref<{ year: number; month: number }>({ year: 2026, month: 0 });
const days = ref<CivilDate[]>([]);
const counts = ref<number[] | null>(null);
const countsState = ref<'idle' | 'loading' | 'failed'>('idle');
const typed = ref('');
const typedBad = ref(false);
const picked = ref<CivilDate | null>(null);
const outcome = ref<'seeking' | 'none' | 'noEarlier' | 'noLater' | 'failed' | null>(null);
const error = ref<string | null>(null);
/** "Earlier" or "Later" is seeking: keep both buttons, disabled. */
const nearestSeeking = ref(false);
let countsSeq = 0;
let pickSeq = 0;

const title = computed(() => monthLabel(shown.value.year, shown.value.month));
const weeks = computed(() => Array.from({ length: 6 }, (_, w) => days.value.slice(w * 7, w * 7 + 7)));

function cellIndex(c: CivilDate): number {
  return days.value.findIndex((d) => sameDay(d, c));
}

function cellId(index: number): string {
  return `oma-cal-${index}`;
}

function countOf(index: number): number | null {
  return counts.value?.[index] ?? null;
}

function countText(index: number): string {
  const n = countOf(index);
  if (n === null) return '';
  if (n === 0) return DATE_JUMP_TEXT.noItems;
  return n === 1 ? DATE_JUMP_TEXT.oneItem : fill(DATE_JUMP_TEXT.items, { n });
}

function cellClass(c: CivilDate, index: number): string {
  const classes = ['oma-cal-day'];
  if (c.month !== shown.value.month) classes.push('oma-cal-outside');
  if ((countOf(index) ?? 0) > 0) classes.push('oma-cal-has');
  if (sameDay(c, focusDay.value)) classes.push('oma-cal-focus');
  return classes.join(' ');
}

async function loadMonth(): Promise<void> {
  const mine = (countsSeq += 1);
  const { year, month } = shown.value;
  let bounds: number[];
  try {
    const grid = monthGrid(build.value, year, month);
    days.value = grid.days;
    bounds = grid.bounds;
  } catch {
    counts.value = null;
    countsState.value = 'failed';
    return;
  }
  counts.value = null;
  countsState.value = 'loading';
  // A day the zone skipped is empty: Rust gets strictly increasing bounds.
  const strict = strictBounds(bounds);
  const got = await viewer.dayCounts(props.chatId, strict.bounds);
  if (mine !== countsSeq) return;
  if (got === null || 'error' in got || got.counts.length !== strict.bounds.length - 1) {
    countsState.value = 'failed';
    return;
  }
  counts.value = countsPerDay(strict.slots, got.counts);
  countsState.value = 'idle';
}

/** Move the focused day, changing month when it leaves the shown one. */
function moveTo(c: CivilDate, focusCell = true): void {
  focusDay.value = c;
  if (c.year !== shown.value.year || c.month !== shown.value.month) {
    shown.value = { year: c.year, month: c.month };
    void loadMonth();
  }
  if (focusCell) void nextTick(() => document.getElementById(cellId(cellIndex(c)))?.focus());
}

watch(
  () => props.open,
  (open) => {
    if (!open) {
      // Closed (Escape, Close, the backdrop): answers still on their way
      // are dropped, so nothing jumps after the user cancelled.
      pickSeq += 1;
      countsSeq += 1;
      nearestSeeking.value = false;
      return;
    }
    const start = civilOf(props.initial ?? (props.now ?? Date.now)(), zone.value);
    focusDay.value = start;
    shown.value = { year: start.year, month: start.month };
    typed.value = '';
    typedBad.value = false;
    picked.value = null;
    outcome.value = null;
    error.value = null;
    void loadMonth();
    void nextTick(() => document.getElementById(cellId(cellIndex(start)))?.focus());
  },
  { immediate: true },
);

const KEY_DAYS: Record<string, number> = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7 };

function onGridKeydown(event: KeyboardEvent): void {
  if (event.ctrlKey || event.altKey || event.metaKey) return;
  const c = focusDay.value;
  let next: CivilDate | null = null;
  if (event.key in KEY_DAYS) next = addDays(c, KEY_DAYS[event.key]!);
  else if (event.key === 'Home') next = startOfWeek(c);
  else if (event.key === 'End') next = endOfWeek(c);
  else if (event.key === 'PageUp') next = addMonths(c, event.shiftKey ? -12 : -1);
  else if (event.key === 'PageDown') next = addMonths(c, event.shiftKey ? 12 : 1);
  if (next === null) return;
  event.preventDefault();
  moveTo(next);
}

function step(months: number): void {
  moveTo(addMonths(focusDay.value, months), false);
}

async function pick(c: CivilDate): Promise<void> {
  const mine = (pickSeq += 1);
  focusDay.value = c;
  picked.value = c;
  nearestSeeking.value = false;
  outcome.value = 'seeking';
  error.value = null;
  let range: [number, number];
  try {
    range = dayRange(build.value, c);
  } catch {
    outcome.value = 'failed';
    return;
  }
  // A day the zone skipped has no instant, so no item.
  if (range[1] <= range[0]) {
    outcome.value = 'none';
    return;
  }
  const counted = await viewer.dayCounts(props.chatId, range);
  if (mine !== pickSeq || counted === null) return;
  if ('error' in counted) {
    outcome.value = 'failed';
    error.value = counted.error;
    return;
  }
  if ((counted.counts[0] ?? 0) === 0) {
    outcome.value = 'none';
    return;
  }
  await seek(range[0], 'onOrAfter', 'none', mine);
}

async function seek(
  utcMs: number,
  dir: 'onOrAfter' | 'before',
  missing: 'none' | 'noEarlier' | 'noLater',
  mine = (pickSeq += 1),
): Promise<void> {
  outcome.value = 'seeking';
  const got = await viewer.seekDate(props.chatId, utcMs, dir);
  if (mine !== pickSeq || got === null || !props.open) return;
  nearestSeeking.value = false;
  if ('error' in got) {
    outcome.value = 'failed';
    error.value = got.error;
    return;
  }
  if (got.id === null) {
    outcome.value = missing;
    return;
  }
  outcome.value = null;
  emit('jump', got.id);
}

function nearest(dir: 'earlier' | 'later'): void {
  const c = picked.value;
  if (!c || nearestSeeking.value) return;
  let range: [number, number];
  try {
    range = dayRange(build.value, c);
  } catch {
    outcome.value = 'failed';
    return;
  }
  nearestSeeking.value = true;
  if (dir === 'earlier') void seek(range[0], 'before', 'noEarlier');
  else void seek(range[1], 'onOrAfter', 'noLater');
}

function submitTyped(): void {
  const c = parseIsoDate(typed.value);
  typedBad.value = c === null;
  if (!c) return;
  moveTo(c, false);
  void pick(c);
}

const outcomeText = computed(() => {
  switch (outcome.value) {
    case 'seeking':
      return DATE_JUMP_TEXT.seeking;
    case 'none':
      return DATE_JUMP_TEXT.noMessages;
    case 'noEarlier':
      return DATE_JUMP_TEXT.noEarlier;
    case 'noLater':
      return DATE_JUMP_TEXT.noLater;
    case 'failed':
      return error.value ? `${DATE_JUMP_TEXT.failed} ${error.value}` : DATE_JUMP_TEXT.failed;
    default:
      return '';
  }
});
const offerNearest = computed(
  () =>
    outcome.value === 'none' ||
    outcome.value === 'noEarlier' ||
    outcome.value === 'noLater' ||
    (outcome.value === 'seeking' && nearestSeeking.value),
);
</script>

<template>
  <ModalDialog :open="open" :title="DATE_JUMP_TEXT.title" @close="emit('close')">
    <div class="oma-cal">
      <div class="oma-cal-head">
        <button type="button" class="btn btn-sm btn-outline-secondary" @click="step(-12)">
          <i class="bi bi-chevron-double-left" aria-hidden="true"></i
          ><span class="visually-hidden">{{ DATE_JUMP_TEXT.previousYear }}</span>
        </button>
        <button type="button" class="btn btn-sm btn-outline-secondary" @click="step(-1)">
          <i class="bi bi-chevron-left" aria-hidden="true"></i
          ><span class="visually-hidden">{{ DATE_JUMP_TEXT.previousMonth }}</span>
        </button>
        <h3 id="oma-cal-title" class="oma-cal-title" aria-live="polite">{{ title }}</h3>
        <button type="button" class="btn btn-sm btn-outline-secondary" @click="step(1)">
          <i class="bi bi-chevron-right" aria-hidden="true"></i
          ><span class="visually-hidden">{{ DATE_JUMP_TEXT.nextMonth }}</span>
        </button>
        <button type="button" class="btn btn-sm btn-outline-secondary" @click="step(12)">
          <i class="bi bi-chevron-double-right" aria-hidden="true"></i
          ><span class="visually-hidden">{{ DATE_JUMP_TEXT.nextYear }}</span>
        </button>
      </div>
      <table class="oma-cal-grid" role="grid" aria-labelledby="oma-cal-title" @keydown="onGridKeydown">
        <thead>
          <tr>
            <th v-for="w in WEEKDAYS" :key="w.long" scope="col">
              <abbr :title="w.long">{{ w.short }}</abbr>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="(week, w) in weeks" :key="w">
            <td
              v-for="(c, d) in week"
              :key="d"
              role="gridcell"
              :aria-selected="sameDay(c, focusDay)"
            >
              <button
                :id="cellId(w * 7 + d)"
                type="button"
                :class="cellClass(c, w * 7 + d)"
                :tabindex="sameDay(c, focusDay) ? 0 : -1"
                @click="pick(c)"
                @focus="focusDay = c"
              >
                <span aria-hidden="true">{{ c.day }}</span>
                <span v-if="(countOf(w * 7 + d) ?? 0) > 0" class="oma-cal-dot" aria-hidden="true">●</span>
                <span class="visually-hidden">{{ fullDayLabel(c) }}<template v-if="countText(w * 7 + d)">, {{ countText(w * 7 + d) }}</template></span>
              </button>
            </td>
          </tr>
        </tbody>
      </table>
      <p class="form-text mb-1">{{ DATE_JUMP_TEXT.gridHint }} {{ DATE_JUMP_TEXT.marked }}</p>
      <p v-if="countsState === 'loading'" class="form-text mb-1" role="status">{{ DATE_JUMP_TEXT.countsLoading }}</p>
      <p v-else-if="countsState === 'failed'" class="form-text mb-1" role="status">{{ DATE_JUMP_TEXT.countsFailed }}</p>
      <p class="form-text mb-2">{{ DATE_JUMP_TEXT.zoneNote }}</p>

      <form class="oma-cal-typed" @submit.prevent="submitTyped">
        <label for="oma-cal-typed" class="form-label mb-1">{{ DATE_JUMP_TEXT.typedLabel }}</label>
        <div class="d-flex gap-2">
          <input
            id="oma-cal-typed"
            v-model="typed"
            type="text"
            inputmode="numeric"
            class="form-control form-control-sm"
            spellcheck="false"
            autocomplete="off"
            autocapitalize="off"
            autocorrect="off"
            :aria-invalid="typedBad"
          />
          <button type="submit" class="btn btn-sm btn-outline-secondary">{{ DATE_JUMP_TEXT.go }}</button>
        </div>
        <p v-if="typedBad" class="text-danger mb-0 mt-1" role="alert">{{ DATE_JUMP_TEXT.typedBad }}</p>
      </form>

      <div class="oma-cal-outcome" role="status">
        <p v-if="outcomeText" class="mb-1 mt-2">
          <template v-if="picked && outcome !== 'seeking'">{{ fullDayLabel(picked) }}: </template>{{ outcomeText }}
        </p>
      </div>
      <div v-if="offerNearest" class="d-flex gap-2 flex-wrap">
        <button
          type="button"
          class="btn btn-sm btn-outline-secondary"
          :aria-disabled="nearestSeeking ? 'true' : undefined"
          @click="nearest('earlier')"
        >
          {{ DATE_JUMP_TEXT.earlier }}
        </button>
        <button
          type="button"
          class="btn btn-sm btn-outline-secondary"
          :aria-disabled="nearestSeeking ? 'true' : undefined"
          @click="nearest('later')"
        >
          {{ DATE_JUMP_TEXT.later }}
        </button>
      </div>
    </div>
  </ModalDialog>
</template>
