import { createApp } from 'vue';
import { createPinia } from 'pinia';
import 'bootstrap/dist/css/bootstrap.min.css';
import 'bootstrap-icons/font/bootstrap-icons.min.css';
import './assets/styles/app.css';
import App from './App.vue';
import { router } from './router';

// Bootstrap's JavaScript is deliberately not loaded: it manipulates the DOM
// behind Vue's back. Dialogs, overlays, and toasts are Vue components that
// use Bootstrap's CSS classes.
createApp(App).use(createPinia()).use(router).mount('#app');
