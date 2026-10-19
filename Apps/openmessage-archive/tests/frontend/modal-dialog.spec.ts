import { afterEach, describe, expect, it } from 'vitest';
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

  it('gives focus back to its opener when it closes (M5 §10)', async () => {
    const proto = HTMLDialogElement.prototype as unknown as Record<string, unknown>;
    proto.showModal = function showModal(this: HTMLDialogElement) {
      this.setAttribute('open', '');
      this.querySelector('button')?.focus();
    };
    proto.close = function close(this: HTMLDialogElement) {
      this.removeAttribute('open');
    };
    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();
    const w = mount(ModalDialog, { props: { open: false, title: 'T' }, attachTo: document.body });
    await w.setProps({ open: true });
    expect(document.activeElement?.getAttribute('aria-label')).toBe('Close');
    await w.setProps({ open: false });
    expect(document.activeElement).toBe(opener);
    // Escape (a native close while meant to be open) restores it too.
    await w.setProps({ open: true });
    const el = w.find('dialog').element as HTMLDialogElement;
    el.removeAttribute('open');
    el.dispatchEvent(new Event('close'));
    expect(document.activeElement).toBe(opener);
    w.unmount();
  });
});

afterEach(() => {
  const proto = HTMLDialogElement.prototype as unknown as Record<string, unknown>;
  delete proto.showModal;
  delete proto.close;
  document.body.innerHTML = '';
});
