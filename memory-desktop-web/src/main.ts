import { createApp } from 'vue'
import './styles.css'
import App from './App.vue'

// 禁用 WebView 默认右键菜单，桌面应用不展示浏览器上下文菜单。
window.addEventListener('contextmenu', (event) => {
  event.preventDefault()
})

createApp(App).mount('#app')
