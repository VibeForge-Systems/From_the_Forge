<script setup lang="ts">
// Shown instead of the app when the web engine lacks the modal <dialog>
// (M5 §8.10). It names the requirement; it never guesses a version.
</script>

<template>
  <main class="oma-page">
    <h1 class="h4">This system's web engine is too old</h1>
    <p>
      OpenMessage Archive needs a web engine that supports modal dialogs
      (<code>HTMLDialogElement.showModal</code>). On Windows, update the Microsoft Edge
      WebView2 Runtime. On Linux, update the WebKitGTK package (webkit2gtk-4.1) through your
      distribution.
    </p>
  </main>
</template>
