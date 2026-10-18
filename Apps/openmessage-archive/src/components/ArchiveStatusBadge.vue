<script setup lang="ts">
import { computed } from 'vue';
import type { ArchiveStatusDto } from '@/types';
import { STATUS_LABEL } from '@/services/importText';

const props = defineProps<{ status: ArchiveStatusDto }>();
const presentation = computed(() => STATUS_LABEL[props.status]);
</script>

<template>
  <span class="badge" :class="presentation.cls">{{ presentation.text }}</span>
</template>
