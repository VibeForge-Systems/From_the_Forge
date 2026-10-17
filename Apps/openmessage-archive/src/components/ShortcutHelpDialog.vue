<script setup lang="ts">
import ModalDialog from './ModalDialog.vue';
import { SHORTCUTS, formatShortcut } from '@/services/shortcuts';

defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();
</script>

<template>
  <ModalDialog :open="open" title="Keyboard shortcuts" @close="emit('close')">
    <table class="table table-sm align-middle mb-0">
      <thead>
        <tr>
          <th scope="col">Action</th>
          <th scope="col" class="text-end">Keys</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="s in SHORTCUTS" :key="s.id">
          <td>{{ s.description }}</td>
          <td class="text-end text-nowrap">
            <template v-for="(k, i) in formatShortcut(s)" :key="k">
              <span v-if="i > 0" class="text-body-secondary"> + </span><kbd>{{ k }}</kbd>
            </template>
          </td>
        </tr>
        <tr>
          <td>Close a dialog or overlay panel</td>
          <td class="text-end"><kbd>Esc</kbd></td>
        </tr>
      </tbody>
    </table>
  </ModalDialog>
</template>
