import { createApp } from 'vue';
import PanelApp from './PanelApp.vue';
import { initTheme } from '../theme';
import '../styles/tokens.css';
import '../styles/chat.css';
import './panel.css';

initTheme();
createApp(PanelApp).mount('#panel');
