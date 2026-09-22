import { readFileSync } from 'node:fs'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// 版本号从 package.json 读，打进界面里。
// 为什么必须显示：2026-09-17 发生过「更新了但用户看到的还是旧界面」的扯皮——
// 界面上只有构建时间，用户没法确认自己到底跑的是哪一版，我也没法自证换上了没有。
const pkg = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8'))

export default defineConfig({
  plugins: [react()],
  base: './',
  define: {
    __APP_BUILD__: JSON.stringify(new Date().toLocaleString('zh-CN')),
    __APP_VERSION__: JSON.stringify(pkg.version),
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    port: 5173,
    strictPort: true,
  },
})
