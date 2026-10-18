import { describe, expect, it } from 'vitest';
import { mount } from '@vue/test-utils';
import ModalDialog from '@/components/ModalDialog.vue';

describe('ModalDialog', () => {
  it('reports a native close only when the dialog was meant to be open', async () => {
    const w = mount(ModalDialog, { props: { open: true, title: 'T' }, attachTo: document.body });
    const el = w.find('dialog').element as HTMLDialogElement;

    // Escape closes the native dialog while the parent still wants it open.
    el.removeAttribute('open');
    el.dispatchEvent(new Event('close'));
    expect(w.emitted('close')).toHaveLength(1);

    // A stale event from a programmatic close, arriving after a reopen.
    el.setAttribute('open', '');
    el.dispatchEvent(new Event('close'));
    expect(w.emitted('close')).toHaveLength(1);

    // A programmatic close (the parent set open to false).
    await w.setProps({ open: false });
    el.removeAttribute('open');
    el.dispatchEvent(new Event('close'));
    expect(w.emitted('close')).toHaveLength(1);
    w.unmount();
  });
});
