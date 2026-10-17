<script setup lang="ts">
import type { CheckLevel, LocationCheck } from '@/types';

defineProps<{ checks: LocationCheck[] }>();

const ICON: Record<CheckLevel, string> = {
  ok: 'bi-check-circle-fill text-success',
  warning: 'bi-exclamation-triangle-fill text-warning',
  error: 'bi-x-circle-fill text-danger',
};
const LABEL: Record<CheckLevel, string> = { ok: 'Passed', warning: 'Warning', error: 'Failed' };
</script>

<template>
  <ul class="list-unstyled oma-checks mb-0">
    <li v-for="check in checks" :key="check.id" class="d-flex gap-2 align-items-start">
      <i class="bi" :class="ICON[check.level]" aria-hidden="true"></i>
      <span>
        <span class="visually-hidden">{{ LABEL[check.level] }}: </span>{{ check.message }}
      </span>
    </li>
  </ul>
</template>
