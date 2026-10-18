import { describe, expect, it } from 'vitest';
import { mount } from '@vue/test-utils';
import BackupPasswordDialog from '@/components/BackupPasswordDialog.vue';

const PASSWORD = 'synthetic-test-password-sentinel';

function mountDialog(props: { open?: boolean; error?: string | null } = {}) {
  return mount(BackupPasswordDialog, {
    props: { open: true, deviceName: 'Synthetic Test Phone', error: null, ...props },
    attachTo: document.body,
  });
}

describe('BackupPasswordDialog', () => {
  it('is a password field that browsers and spellcheckers leave alone', () => {
    const w = mountDialog();
    const input = w.find('#backup-password');
    expect(input.attributes('type')).toBe('password');
    expect(input.attributes('autocomplete')).toBe('off');
    expect(input.attributes('spellcheck')).toBe('false');
    expect(w.text()).toContain('Synthetic Test Phone');
    expect(w.text()).toContain('never saved');
    w.unmount();
  });

  it('disables Submit while the field is empty', async () => {
    const w = mountDialog();
    const submit = () => w.find('button[type="submit"]');
    expect(submit().attributes('disabled')).toBeDefined();
    await w.find('#backup-password').setValue(PASSWORD);
    expect(submit().attributes('disabled')).toBeUndefined();
    w.unmount();
  });

  it('emits the value once and empties the field and the reveal toggle on submit', async () => {
    const w = mountDialog();
    await w.find('#reveal').setValue(true);
    expect(w.find('#backup-password').attributes('type')).toBe('text');
    await w.find('#backup-password').setValue(PASSWORD);
    await w.find('form').trigger('submit');
    expect(w.emitted('submit')).toEqual([[PASSWORD]]);
    expect((w.find('#backup-password').element as HTMLInputElement).value).toBe('');
    expect(w.find('#backup-password').attributes('type')).toBe('password');
    expect((w.find('#reveal').element as HTMLInputElement).checked).toBe(false);
    w.unmount();
  });

  it('empties the field on cancel and on close', async () => {
    const w = mountDialog();
    await w.find('#backup-password').setValue(PASSWORD);
    const cancel = w.findAll('button').find((b) => b.text() === 'Cancel');
    await cancel?.trigger('click');
    expect(w.emitted('close')).toHaveLength(1);
    expect((w.find('#backup-password').element as HTMLInputElement).value).toBe('');

    await w.find('#backup-password').setValue(PASSWORD);
    await w.find('button.btn-close').trigger('click');
    expect(w.emitted('close')).toHaveLength(2);
    expect((w.find('#backup-password').element as HTMLInputElement).value).toBe('');
    w.unmount();
  });

  it('renders the error as an alert and clears the field when it reopens with one', async () => {
    const w = mountDialog();
    await w.find('#backup-password').setValue(PASSWORD);
    await w.setProps({ error: 'That password did not unlock the backup.' });
    await w.vm.$nextTick();
    const alert = w.find('[role="alert"]');
    expect(alert.text()).toBe('That password did not unlock the backup.');
    expect(alert.attributes('id')).toBe('backup-password-error');
    expect(w.find('#backup-password').attributes('aria-invalid')).toBe('true');
    expect((w.find('#backup-password').element as HTMLInputElement).value).toBe('');
    expect(w.html()).not.toContain(PASSWORD);
    w.unmount();
  });

  it('names what the click starts: validation, the extraction check, or a caller-supplied action', async () => {
    const w = mount(BackupPasswordDialog, {
      props: { open: true, deviceName: null, error: null },
      attachTo: document.body,
    });
    const submit = () => w.find('button[type="submit"]').text();
    expect(submit()).toBe('Unlock and validate');
    await w.setProps({ resume: 'extractionCheck' });
    expect(submit()).toBe('Unlock and run extraction check');
    await w.setProps({ resume: 'import', submitLabel: 'Unlock and prepare import' });
    expect(submit()).toBe('Unlock and prepare import');
    w.unmount();
  });
});
