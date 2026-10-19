// The three-panel layout's overlays (review fixes): Escape inside an open
// dialog belongs to the dialog and never also closes an overlay, and focus
// returns to what had it when an overlay that held it closes.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { flushPromises, mount } from '@vue/test-utils';
import { h } from 'vue';
import PanelLayout from '@/components/PanelLayout.vue';
import { useLayout } from '@/stores/layout';
import { useSettings } from '@/stores/settings';
import { snapshot, stubMatchMedia } from './helpers';

function mountLayout() {
  return mount(PanelLayout, {
    attachTo: document.body,
    props: { leftLabel: 'Conversations', rightLabel: 'Details' },
    slots: {
      default: () => [
        h('button', { id: 'opener' }, 'Open'),
        h('dialog', { open: true }, [h('button', { id: 'in-dialog' }, 'Cancel')]),
      ],
      right: () => h('button', { id: 'in-panel' }, 'Close'),
    },
  });
}

beforeEach(() => {
  setActivePinia(createPinia());
  // A narrow window: both side panels are overlays.
  stubMatchMedia(() => false);
  useSettings().snapshot = snapshot();
});

afterEach(() => {
  document.body.innerHTML = '';
});

describe('PanelLayout overlays', () => {
  it('leave an overlay open when Escape is pressed inside an open dialog', async () => {
    const w = mountLayout();
    const layout = useLayout();
    layout.rightOverlay = true;
    await flushPromises();
    await w.get('#in-dialog').trigger('keydown', { key: 'Escape' });
    expect(layout.rightOverlay).toBe(true);
    await w.get('#in-panel').trigger('keydown', { key: 'Escape' });
    expect(layout.rightOverlay).toBe(false);
  });

  it('give focus back to the opener when an overlay that held it closes', async () => {
    const w = mountLayout();
    const layout = useLayout();
    (w.get('#opener').element as HTMLButtonElement).focus();
    layout.rightOverlay = true;
    await flushPromises();
    (w.get('#in-panel').element as HTMLButtonElement).focus();
    layout.closeOverlays();
    await flushPromises();
    expect(document.activeElement?.id).toBe('opener');
  });
});
