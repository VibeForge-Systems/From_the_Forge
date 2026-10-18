import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import { createMemoryHistory, createRouter } from 'vue-router';

vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  return { ...actual, ipc: { chooseArchiveDirectory: vi.fn(), updateSettings: vi.fn() } };
});
import ArchiveView from '@/views/ArchiveView.vue';
import { useSettings } from '@/stores/settings';
import { snapshot, status, stubMatchMedia } from './helpers';

function mountView() {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', component: ArchiveView },
      { path: '/settings', component: { template: '<div />' } },
      { path: '/backups', component: { template: '<div />' } },
    ],
  });
  return mount(ArchiveView, { global: { plugins: [router] } });
}

beforeEach(() => {
  setActivePinia(createPinia());
  stubMatchMedia(() => true); // wide window: both panels dock
});

describe('ArchiveView', () => {
  it('welcomes a new user with one working action and the rest marked planned', () => {
    const settings = useSettings();
    settings.loaded = true;
    settings.snapshot = snapshot();
    const wrapper = mountView();

    expect(wrapper.text()).toContain('Welcome to OpenMessage Archive');
    const choose = wrapper.findAll('button').find((b) => b.text().includes('Choose archive folder'));
    expect(choose?.attributes('disabled')).toBeUndefined();
    expect(wrapper.findAll('.oma-planned').length).toBeGreaterThanOrEqual(2);
    const steps = wrapper.findAll('.oma-steps li').map((li) => li.text());
    expect(steps).toHaveLength(4);
    expect(steps[1]).toContain('Find your iPhone backup.');
    expect(steps[1]).not.toContain('Planned');
    expect(wrapper.find('.oma-steps a[href="/backups"]').exists()).toBe(true);
    expect(steps[2]).toContain('Import messages.');
    expect(steps[2]).toContain('Planned · M3');
    expect(steps[3]).toContain('Planned · M5');
  });

  it('offers Find backups and never an enabled Import button before milestone 3', () => {
    const settings = useSettings();
    settings.loaded = true;
    settings.snapshot = snapshot({ archiveDirectory: '/data/archives' });
    settings.location = status('ready');
    const wrapper = mountView();

    expect(wrapper.text()).toContain('Your archive library is ready');
    const find = wrapper.findAll('a').find((a) => a.text().includes('Find backups'));
    expect(find?.attributes('href')).toBe('/backups');
    const importButton = wrapper.findAll('button').find((b) => b.text().includes('Import messages'));
    expect(importButton?.attributes('disabled')).toBeDefined();
    expect(wrapper.text()).toContain('Planned · M3');
    expect(wrapper.text()).not.toContain('Planned · M2');
    expect(wrapper.text()).toContain('Backup passwords are used once and never saved.');
  });

  it('shows why an unavailable location failed', () => {
    const settings = useSettings();
    settings.loaded = true;
    settings.snapshot = snapshot({ archiveDirectory: '/mnt/usb/archives' });
    settings.location = status('error', '/mnt/usb/archives', [
      { id: 'exists', level: 'error', message: 'The folder could not be found.' },
    ]);
    const wrapper = mountView();
    expect(wrapper.text()).toContain('The archive location is unavailable');
    expect(wrapper.text()).toContain('The folder could not be found.');
  });

  it('docks both panels on a wide window and hides them per the saved layout', async () => {
    const settings = useSettings();
    settings.loaded = true;
    settings.snapshot = snapshot();
    const wrapper = mountView();
    expect(wrapper.find('#oma-left-panel').exists()).toBe(true);
    expect(wrapper.find('#oma-right-panel').exists()).toBe(true);

    settings.snapshot = snapshot({ layout: { leftPanelOpen: false, rightPanelOpen: true } });
    await wrapper.vm.$nextTick();
    expect(wrapper.find('#oma-left-panel').exists()).toBe(false);
  });

  it('starts with overlays closed on a narrow window', () => {
    stubMatchMedia(() => false);
    const settings = useSettings();
    settings.loaded = true;
    settings.snapshot = snapshot();
    const wrapper = mountView();
    expect(wrapper.find('#oma-left-panel').exists()).toBe(false);
    expect(wrapper.find('#oma-right-panel').exists()).toBe(false);
  });
});
