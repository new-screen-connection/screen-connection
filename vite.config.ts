import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';

// 多页面入口：index.html（主窗口，Vue 应用）+ panel.html / notify.html（legacy 自包含页面，保持 Tauri 窗口路径不变）
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    target: 'chrome110',
    minify: 'esbuild',
    rollupOptions: {
      input: {
        index: 'index.html',
        panel: 'panel.html',
        notify: 'notify.html',
      },
    },
  },
});
