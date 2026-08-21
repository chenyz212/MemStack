import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

/** 构建桌面窗口使用的 Vue 静态资源。 */
export default defineConfig({
  plugins: [vue()],
  base: '/',
})
