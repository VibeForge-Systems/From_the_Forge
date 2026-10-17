import { defineStore } from 'pinia';
import { ref } from 'vue';

export type ToastLevel = 'info' | 'success' | 'warning' | 'danger';

export interface Toast {
  id: number;
  level: ToastLevel;
  message: string;
}

const AUTO_DISMISS_MS: Record<ToastLevel, number | null> = {
  info: 5000,
  success: 4000,
  warning: 8000,
  // Errors stay until dismissed: the user may need to read or copy them.
  danger: null,
};

export const useNotifications = defineStore('notifications', () => {
  const toasts = ref<Toast[]>([]);
  let nextId = 1;

  function dismiss(id: number): void {
    toasts.value = toasts.value.filter((t) => t.id !== id);
  }

  function push(level: ToastLevel, message: string): number {
    const id = nextId++;
    toasts.value = [...toasts.value, { id, level, message }];
    const timeout = AUTO_DISMISS_MS[level];
    if (timeout !== null) setTimeout(() => dismiss(id), timeout);
    return id;
  }

  return { toasts, push, dismiss };
});
