import {
  createRouter,
  createWebHashHistory,
  type RouteLocationNormalized,
  type RouteRecordRaw,
} from 'vue-router';
import ArchiveView from '@/views/ArchiveView.vue';
import { useViewer } from '@/stores/viewer';

const ViewerView = () => import('@/views/ViewerView.vue');

/** The viewer's routes. Message ids are never part of them (M5 §8.3). */
export const VIEWER_ROUTES: readonly string[] = ['viewer', 'viewer-chat'];

export function isViewerRoute(route: Pick<RouteLocationNormalized, 'name'>): boolean {
  return typeof route.name === 'string' && VIEWER_ROUTES.includes(route.name);
}

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
    path: '/archive/:archiveId/viewer',
    name: 'viewer',
    component: ViewerView,
    props: true,
    // A fixed title: the window title never shows a chat name (D112).
    meta: { title: 'Viewer' },
  },
  {
    path: '/archive/:archiveId/viewer/c/:chatId',
    name: 'viewer-chat',
    component: ViewerView,
    props: true,
    meta: { title: 'Viewer' },
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

/**
 * Leaving the viewer routes closes the viewer session (D114): the archive
 * no longer counts as open, and the store forgets everything it held.
 */
export function closeViewerOnLeave(to: RouteLocationNormalized, from: RouteLocationNormalized): void {
  if (isViewerRoute(from) && !isViewerRoute(to)) void useViewer().close();
}

router.beforeEach((to, from) => {
  closeViewerOnLeave(to, from);
});

router.afterEach((to) => {
  const title = typeof to.meta.title === 'string' ? to.meta.title : null;
  document.title = title ? `${title} — OpenMessage Archive` : 'OpenMessage Archive';
});
