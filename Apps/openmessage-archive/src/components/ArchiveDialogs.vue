<script setup lang="ts">
// The archive dialogs, mounted once in App.vue so they open from the
// Archive page and the Backups page alike, and so the gate shows wherever
// the user is while an import waits for an answer.
import { computed } from 'vue';
import BackupPasswordDialog from '@/components/BackupPasswordDialog.vue';
import CreateArchiveDialog from '@/components/CreateArchiveDialog.vue';
import ImportDialog from '@/components/ImportDialog.vue';
import ImportPreflightDialog from '@/components/ImportPreflightDialog.vue';
import { useArchives } from '@/stores/archives';
import { useBackups } from '@/stores/backups';

const archives = useArchives();
const backups = useBackups();

const promptOpen = computed(() => archives.passwordPrompt !== null);
const deviceName = computed(() => {
  const id = archives.passwordPrompt?.backupId;
  if (!id) return null;
  const s = backups.details[id]?.summary ?? backups.list?.backups.find((b) => b.id === id);
  return s?.deviceName?.value ?? null;
});

function onPassword(password: string): void {
  void archives.submitPassword(password);
}
</script>

<template>
  <CreateArchiveDialog />
  <ImportDialog />
  <BackupPasswordDialog
    :open="promptOpen"
    :device-name="deviceName"
    :error="archives.passwordPrompt?.error ?? null"
    resume="import"
    submit-label="Unlock and prepare import"
    @submit="onPassword"
    @close="archives.closePrompt()"
  />
  <ImportPreflightDialog />
</template>
