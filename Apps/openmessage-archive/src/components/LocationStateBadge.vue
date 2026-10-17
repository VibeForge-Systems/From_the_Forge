<script setup lang="ts">
import { computed } from 'vue';
import type { LocationState } from '@/types';

const props = defineProps<{ state: LocationState }>();

const presentation = computed(
  () =>
    ({
      unset: { text: 'Not set', cls: 'text-bg-secondary' },
      ready: { text: 'Ready', cls: 'text-bg-success' },
      warning: { text: 'Needs attention', cls: 'text-bg-warning' },
      error: { text: 'Unavailable', cls: 'text-bg-danger' },
    })[props.state],
);
</script>

<template>
  <span class="badge" :class="presentation.cls">{{ presentation.text }}</span>
</template>
