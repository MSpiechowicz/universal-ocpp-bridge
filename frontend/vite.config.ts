import { defineConfig } from 'vite';

export default defineConfig({
  base: '/ui/',
  build: {
    target: 'es2022',
    outDir: '../adapters/management/ui',
    emptyOutDir: true,
    sourcemap: false,
    rolldownOptions: {
      output: {
        entryFileNames: 'assets/console.js',
        assetFileNames: 'assets/console.[ext]',
      },
    },
  },
});
