<script setup lang="ts">
import { computed } from 'vue';
import { capability, type CapabilityId } from '@/services/roadmap';

const props = defineProps<{ capability: CapabilityId }>();
const info = computed(() => capability(props.capability));
</script>

<template>
  <span
    class="badge rounded-pill text-bg-secondary oma-planned"
    :title="`${info.title} is planned for milestone ${info.milestone}: ${info.summary}`"
  >
    Planned · M{{ info.milestone }}
  </span>
</template>
