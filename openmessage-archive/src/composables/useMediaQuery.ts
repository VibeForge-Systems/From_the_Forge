import { onBeforeUnmount, ref, type Ref } from 'vue';

/** Reactive `matchMedia`. Returns false where matchMedia is unavailable. */
export function useMediaQuery(query: string): Ref<boolean> {
  const mql = typeof window !== 'undefined' && window.matchMedia ? window.matchMedia(query) : null;
  const matches = ref(mql?.matches ?? false);
  const onChange = (event: MediaQueryListEvent) => {
    matches.value = event.matches;
  };
  mql?.addEventListener('change', onChange);
  onBeforeUnmount(() => mql?.removeEventListener('change', onChange));
  return matches;
}
