import { createRouter, createWebHashHistory, type RouteRecordRaw } from 'vue-router';
import ArchiveView from '@/views/ArchiveView.vue';

// Hash history: the app is served from Tauri's custom protocol, where a
// reload on a path-style URL would not fall back to index.html.
export const routes: RouteRecordRaw[] = [
  { path: '/', name: 'archive', component: ArchiveView, meta: { title: 'Archive' } },
  {
    path: '/backups',
    name: 'backups',
    component: () => import('@/views/BackupsView.vue'),
    meta: { title: 'Backups' },
  },
  {
    path: '/settings',
    name: 'settings',
    component: () => import('@/views/SettingsView.vue'),
    meta: { title: 'Settings' },
  },
  {
    path: '/about',
    name: 'about',
    component: () => import('@/views/AboutView.vue'),
    meta: { title: 'About' },
  },
  { path: '/:pathMatch(.*)*', redirect: '/' },
];

export const router = createRouter({
  history: createWebHashHistory(),
  routes,
});

router.afterEach((to) => {
  const title = typeof to.meta.title === 'string' ? to.meta.title : null;
  document.title = title ? `${title} — OpenMessage Archive` : 'OpenMessage Archive';
});
