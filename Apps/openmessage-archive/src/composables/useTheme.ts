import { computed, watchEffect, type Ref } from 'vue';
import type { Theme } from '@/types';
import { useMediaQuery } from './useMediaQuery';

export type ResolvedTheme = 'light' | 'dark';

export function resolveTheme(theme: Theme, systemPrefersDark: boolean): ResolvedTheme {
  if (theme === 'system') return systemPrefersDark ? 'dark' : 'light';
  return theme;
}

export const THEME_CYCLE: readonly Theme[] = ['system', 'light', 'dark'];

export function nextTheme(theme: Theme): Theme {
  const index = THEME_CYCLE.indexOf(theme);
  return THEME_CYCLE[(index + 1) % THEME_CYCLE.length] ?? 'system';
}

/**
 * Apply the chosen theme to <html data-bs-theme>, which drives Bootstrap
 * 5.3's color modes. "system" follows the OS setting live.
 */
export function useTheme(theme: Ref<Theme>) {
  const prefersDark = useMediaQuery('(prefers-color-scheme: dark)');
  const resolved = computed(() => resolveTheme(theme.value, prefersDark.value));
  watchEffect(() => {
    document.documentElement.setAttribute('data-bs-theme', resolved.value);
    document.documentElement.style.colorScheme = resolved.value;
  });
  return { resolved };
}
