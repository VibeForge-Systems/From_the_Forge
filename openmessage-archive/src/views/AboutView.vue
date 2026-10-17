<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { ipc } from '@/services/ipc';
import { CAPABILITIES } from '@/services/roadmap';
import type { AppInfo } from '@/types';

const info = ref<AppInfo | null>(null);
const error = ref<string | null>(null);

onMounted(async () => {
  try {
    info.value = await ipc.getAppInfo();
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  }
});
</script>

<template>
  <div class="oma-page">
    <h1 class="h4 mb-4">About</h1>

    <section class="card mb-4" aria-labelledby="about-app">
      <div class="card-header"><h2 id="about-app" class="h6 mb-0">Application</h2></div>
      <div class="card-body">
        <div v-if="error" class="alert alert-danger small mb-0" role="alert">{{ error }}</div>
        <dl v-else-if="info" class="row mb-0 small">
          <dt class="col-sm-3">Name</dt>
          <dd class="col-sm-9">{{ info.name }}</dd>
          <dt class="col-sm-3">Version</dt>
          <dd class="col-sm-9">
            {{ info.version }}
            <span v-if="info.debugBuild" class="badge text-bg-warning ms-1">debug build</span>
          </dd>
          <dt class="col-sm-3">Platform</dt>
          <dd class="col-sm-9">{{ info.os }} ({{ info.arch }})</dd>
          <dt class="col-sm-3">Identifier</dt>
          <dd class="col-sm-9 font-monospace">{{ info.identifier }}</dd>
          <dt class="col-sm-3">License</dt>
          <dd class="col-sm-9">{{ info.license }}</dd>
          <dt class="col-sm-3">Source</dt>
          <dd class="col-sm-9 font-monospace user-select-all text-break">{{ info.sourceUrl }}</dd>
        </dl>
        <div v-else class="spinner-border spinner-border-sm" role="status">
          <span class="visually-hidden">Loading…</span>
        </div>
      </div>
    </section>

    <section class="card mb-4" aria-labelledby="about-capabilities">
      <div class="card-header"><h2 id="about-capabilities" class="h6 mb-0">Capabilities</h2></div>
      <div class="card-body p-0">
        <table class="table table-sm mb-0 align-middle small">
          <caption class="px-3">
            What this build can do. Planned capabilities are shown disabled wherever they appear.
          </caption>
          <thead>
            <tr>
              <th scope="col" class="ps-3">Capability</th>
              <th scope="col">Milestone</th>
              <th scope="col" class="pe-3">Status</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="c in CAPABILITIES" :key="c.id">
              <td class="ps-3">
                <div class="fw-semibold">{{ c.title }}</div>
                <div class="text-body-secondary">{{ c.summary }}</div>
              </td>
              <td>M{{ c.milestone }}</td>
              <td class="pe-3">
                <span v-if="c.status === 'available'" class="badge text-bg-success">Available</span>
                <span v-else class="badge text-bg-secondary">Planned</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <section class="card" aria-labelledby="about-thirdparty">
      <div class="card-header"><h2 id="about-thirdparty" class="h6 mb-0">Third-party software</h2></div>
      <div class="card-body small">
        OpenMessage Archive is built on Tauri, Vue, Bootstrap, Bootstrap Icons, and Rust crates
        under permissive licenses (MIT, Apache-2.0, BSD, ISC, and similar). The complete audited
        list is in <span class="font-monospace">docs/dependencies.md</span> in the source
        repository.
      </div>
    </section>
  </div>
</template>
