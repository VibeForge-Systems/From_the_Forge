import { afterEach, describe, expect, it } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
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
    // Escape (a native close while meant to be open) restores it too, once
    // the parent agrees and closes it.
    await w.setProps({ open: true, onClose: () => void w.setProps({ open: false }) });
    const el = w.find('dialog').element as HTMLDialogElement;
    el.removeAttribute('open');
    el.dispatchEvent(new Event('close'));
    await flushPromises();
    expect(document.activeElement).toBe(opener);
    w.unmount();
  });

  it('shows itself again when the parent declines a native close (review fix)', async () => {
    const proto = HTMLDialogElement.prototype as unknown as Record<string, unknown>;
    let shown = 0;
    proto.showModal = function showModal(this: HTMLDialogElement) {
      shown += 1;
      this.setAttribute('open', '');
      this.querySelector('button')?.focus();
    };
    proto.close = function close(this: HTMLDialogElement) {
      this.removeAttribute('open');
    };
    // The parent keeps it open (a running export asks before cancelling).
    const w = mount(ModalDialog, { props: { open: true, title: 'T', guardEscape: true }, attachTo: document.body });
    const el = w.find('dialog').element as HTMLDialogElement;
    expect(shown).toBe(1);
    // A second Escape whose `cancel` could not be prevented: the element
    // closes natively although `open` is still true.
    el.removeAttribute('open');
    el.dispatchEvent(new Event('close'));
    await flushPromises();
    expect(w.emitted('close')).toHaveLength(1);
    expect(shown).toBe(2);
    expect(el.open).toBe(true);
    expect(el.contains(document.activeElement)).toBe(true);
    w.unmount();
  });

  it('returns focus to `returnTo` when the opener is <body> or inside a closed dialog (review fix)', async () => {
    const proto = HTMLDialogElement.prototype as unknown as Record<string, unknown>;
    proto.showModal = function showModal(this: HTMLDialogElement) {
      this.setAttribute('open', '');
      this.querySelector('button')?.focus();
    };
    proto.close = function close(this: HTMLDialogElement) {
      this.removeAttribute('open');
    };
    // The Play button was disabled when pressed: focus fell to <body>.
    const play = document.createElement('button');
    play.id = 'oma-m-1-a1-play';
    document.body.append(play);
    (document.activeElement as HTMLElement | null)?.blur();
    const w = mount(ModalDialog, { props: { open: false, title: 'T', returnTo: 'oma-m-1-a1-play' }, attachTo: document.body });
    await w.setProps({ open: true });
    await w.setProps({ open: false });
    expect(document.activeElement).toBe(play);

    // Opened from a button inside another dialog that has closed since.
    const other = document.createElement('dialog');
    const inner = document.createElement('button');
    other.append(inner);
    other.setAttribute('open', '');
    document.body.append(other);
    inner.focus();
    await w.setProps({ open: true });
    other.removeAttribute('open');
    await w.setProps({ open: false });
    expect(document.activeElement).toBe(play);
    w.unmount();
  });
});

afterEach(() => {
  const proto = HTMLDialogElement.prototype as unknown as Record<string, unknown>;
  delete proto.showModal;
  delete proto.close;
  document.body.innerHTML = '';
});
