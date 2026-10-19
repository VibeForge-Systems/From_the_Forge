// Archive row ids in the viewer's routes (M5 §8.3). A chat id in the URL
// (`…/viewer/c/:chatId`) is parsed back with the same rule Rust applies to
// `SafeId` (an integer in 1..=2^53 − 1, written in plain decimal), so a
// value that could round in JavaScript never becomes an id. Rust checks
// every id it receives again. Message ids never enter a URL.
import type { SafeId } from '@/types';

const DECIMAL = /^[1-9][0-9]{0,15}$/;

export function isSafeId(value: unknown): value is SafeId {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 1;
}

/** The id written as `text`, or null unless it is a canonical safe integer. */
export function parseSafeId(text: unknown): SafeId | null {
  if (typeof text !== 'string' || !DECIMAL.test(text)) return null;
  const n = Number(text);
  return isSafeId(n) ? n : null;
}
