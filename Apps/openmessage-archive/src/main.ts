import { createApp } from 'vue';
import { createPinia } from 'pinia';
import 'bootstrap/dist/css/bootstrap.min.css';
import 'bootstrap-icons/font/bootstrap-icons.min.css';
import './assets/styles/app.css';
import './assets/styles/viewer.css';
import App from './App.vue';
import EngineTooOld from './views/EngineTooOld.vue';
import { router } from './router';
import { applyFeatures } from './services/features';

// Feature detection runs once, before anything renders (M5 §8.10). Every
// feature has a degraded path except the modal <dialog>, which every
// dialog in the app needs: without it, a page names the problem instead
// of an app whose dialogs would not open.
const features = applyFeatures();

// Bootstrap's JavaScript is deliberately not loaded: it manipulates the DOM
// behind Vue's back. Dialogs, overlays, and toasts are Vue components that
// use Bootstrap's CSS classes.
if (features.dialogModal) {
  createApp(App).use(createPinia()).use(router).mount('#app');
} else {
  createApp(EngineTooOld).mount('#app');
}
