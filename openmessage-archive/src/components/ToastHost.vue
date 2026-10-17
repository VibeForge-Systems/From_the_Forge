<script setup lang="ts">
import { storeToRefs } from 'pinia';
import { useNotifications, type ToastLevel } from '@/stores/notifications';

const notifications = useNotifications();
const { toasts } = storeToRefs(notifications);

const ICON: Record<ToastLevel, string> = {
  info: 'bi-info-circle-fill',
  success: 'bi-check-circle-fill',
  warning: 'bi-exclamation-triangle-fill',
  danger: 'bi-x-octagon-fill',
};
</script>

<template>
  <div class="toast-container position-fixed bottom-0 end-0 p-3 oma-toasts">
    <div
      v-for="toast in toasts"
      :key="toast.id"
      class="toast show align-items-center border-0"
      :class="`text-bg-${toast.level}`"
      :role="toast.level === 'danger' ? 'alert' : 'status'"
      :aria-live="toast.level === 'danger' ? 'assertive' : 'polite'"
      aria-atomic="true"
    >
      <div class="d-flex">
        <div class="toast-body d-flex gap-2">
          <i class="bi" :class="ICON[toast.level]" aria-hidden="true"></i>
          <span>{{ toast.message }}</span>
        </div>
        <button
          type="button"
          class="btn-close me-2 m-auto"
          :class="{ 'btn-close-white': toast.level !== 'warning' }"
          aria-label="Dismiss notification"
          @click="notifications.dismiss(toast.id)"
        ></button>
      </div>
    </div>
  </div>
</template>
