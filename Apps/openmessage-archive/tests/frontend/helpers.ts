import type {
  ArchiveLocationStatus,
  LocationCheck,
  Settings,
  SettingsSnapshot,
} from '@/types';

export function snapshot(overrides: Partial<Settings> = {}, readOnly = false): SettingsSnapshot {
  return {
    settings: {
      schemaVersion: 1,
      archiveDirectory: null,
      theme: 'system',
      layout: { leftPanelOpen: true, rightPanelOpen: true },
      ...overrides,
    },
    persistence: { file: '/config/settings.json', readOnly, notice: null },
  };
}

export function status(
  state: ArchiveLocationStatus['state'],
  path: string | null = '/data/archives',
  checks: LocationCheck[] = [],
): ArchiveLocationStatus {
  return { path: state === 'unset' ? null : path, state, checks };
}

/** jsdom has no matchMedia; tests choose what the "window" matches. */
export function stubMatchMedia(matching: (query: string) => boolean): void {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    writable: true,
    value: (query: string) => ({
      matches: matching(query),
      media: query,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
    }),
  });
}
