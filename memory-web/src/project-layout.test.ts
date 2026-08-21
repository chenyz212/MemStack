import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

/** 验证项目标题操作样式不会误伤项目列表按钮。 */
describe('项目空间布局', () => {
  /** 统一源码空白和引号，避免纯格式化造成假失败。 */
  function normalizeSource(value: string): string {
    return value.replace(/\s+/g, ' ')
  }
  /** 读取前端源码并校验项目标题使用独立类名。 */
  it('只为项目空间标题中的新建按钮设置固定尺寸', () => {
    const stylesPath = fileURLToPath(new URL('./styles.css', import.meta.url))
    const appPath = fileURLToPath(new URL('./App.vue', import.meta.url))
    const styles = readFileSync(stylesPath, 'utf8')
    const app = normalizeSource(readFileSync(appPath, 'utf8'))

    expect(app).toContain('class="project-space-header"')
    expect(styles).toContain('.project-space-header > button')
    expect(styles).not.toContain('.project-space-group > div > button')
  })

  /** 验证项目内新建记忆会继承当前项目，并使用独立状态徽章。 */
  it('继承当前项目并展示清晰的收藏置顶徽章', () => {
    const stylesPath = fileURLToPath(new URL('./styles.css', import.meta.url))
    const appPath = fileURLToPath(new URL('./App.vue', import.meta.url))
    const styles = readFileSync(stylesPath, 'utf8')
    const app = normalizeSource(readFileSync(appPath, 'utf8'))

    expect(app).toContain('memoryView.value === "PROJECT" && selectedProjectId.value')
    expect(app).toContain('memoryForm.value.scope = "Project"')
    expect(app).toContain('memoryForm.value.projectId = selectedProjectId.value')
    expect(app).toContain('class="favorite-marker"')
    expect(app).toContain('class="pinned-marker"')
    expect(styles).toContain('.memory-card-markers .favorite-marker')
    expect(styles).toContain('.memory-card-markers .pinned-marker')
  })

  /** 验证详情抽屉不再加载或展示修改恢复功能。 */
  it('删除最近修改恢复功能', () => {
    const appPath = fileURLToPath(new URL('./App.vue', import.meta.url))
    const app = normalizeSource(readFileSync(appPath, 'utf8'))

    expect(app).not.toContain('MemoryRevisionItem')
    expect(app).not.toContain('memoryRevisions')
    expect(app).not.toContain('restoreRevision')
    expect(app).not.toContain('最近修改')
  })

  /** 验证修改后重新读取详情和页面聚合数据。 */
  it('修改后同步刷新详情和列表状态', () => {
    const appPath = fileURLToPath(new URL('./App.vue', import.meta.url))
    const app = normalizeSource(readFileSync(appPath, 'utf8'))

    expect(app).toContain('async function refreshMemoryState(memoryId: string)')
    expect(app).toContain("apiFetch<MemoryItem>( `/api/memories/${memoryId}`")
    expect(app).toContain('await refreshMemoryState(memory.id)')
    expect(app).toContain('showToast(form.id ? "记忆已更新，页面已同步" : "记忆已创建")')
  })

  /** 验证详情抽屉支持菜单切换、空白点击和键盘关闭。 */
  it('在抽屉外点击或切换菜单时自动关闭详情', () => {
    const appPath = fileURLToPath(new URL('./App.vue', import.meta.url))
    const app = normalizeSource(readFileSync(appPath, 'utf8'))

    expect(app).toContain('async function selectNavigation(key: NavigationKey)')
    expect(app).toContain('function closeMemoryDetailsOutside(event: MouseEvent)')
    expect(app).toContain('@click="closeMemoryDetailsOutside"')
    expect(app).toContain('class="detail-drawer" :class="getMemoryTypeClass(selectedMemory.memoryType)" aria-label="记忆详情" @click.stop')
    expect(app).toContain('target?.closest( ".memory-list article, .recent-list button, .modal-backdrop"')
  })

  /** 验证归档和永久删除使用应用内确认弹窗。 */
  it('使用美观的应用内弹窗确认归档和永久删除', () => {
    const stylesPath = fileURLToPath(new URL('./styles.css', import.meta.url))
    const appPath = fileURLToPath(new URL('./App.vue', import.meta.url))
    const styles = readFileSync(stylesPath, 'utf8')
    const app = normalizeSource(readFileSync(appPath, 'utf8'))

    expect(app).toContain("selectedMemory.status === 'Archived'")
    expect(app).toContain('恢复记忆')
    expect(app).toContain('彻底删除')
    expect(app).toContain('DELETE_MEMORY')
    expect(app).toContain('if (memory.status === "Archived") return')
    expect(app).toContain('class="confirmation-card"')
    expect(app).toContain('class="confirmation-card danger-confirmation"')
    expect(app).toContain('确定移至归档吗？')
    expect(app).toContain('确定彻底删除吗？')
    expect(app).not.toContain('window.prompt')
    expect(styles).toContain('.confirmation-card')
    expect(styles).toContain('.danger-solid-button')
  })

  /** 验证内部类型代码被转换为中文并应用浅色类型卡片。 */
  it('使用中文类型名称和浅色渐变卡片', () => {
    const stylesPath = fileURLToPath(new URL('./styles.css', import.meta.url))
    const appPath = fileURLToPath(new URL('./App.vue', import.meta.url))
    const styles = readFileSync(stylesPath, 'utf8')
    const app = normalizeSource(readFileSync(appPath, 'utf8'))

    expect(app).toContain('NOTE: "笔记"')
    expect(app).toContain('DECISION: "决策"')
    expect(app).toContain('SOLUTION: "方案"')
    expect(app).toContain('getMemoryTypeLabel(memory.memoryType)')
    expect(styles).toContain('.memory-list article.memory-type-note')
    expect(styles).toContain('.memory-list article.memory-type-decision')
    expect(styles).toContain('.memory-list article.memory-type-solution')
  })

  /** 验证收藏和置顶在视觉上只显示中文文字。 */
  it('收藏和置顶不显示装饰符号', () => {
    const appPath = fileURLToPath(new URL('./App.vue', import.meta.url))
    const stylesPath = fileURLToPath(new URL('./styles.css', import.meta.url))
    const app = normalizeSource(readFileSync(appPath, 'utf8'))
    const styles = readFileSync(stylesPath, 'utf8')

    expect(app).toContain('class="favorite-marker"')
    expect(app).toContain('title="已收藏"')
    expect(app).toContain('class="pinned-marker"')
    expect(app).toContain('title="已置顶"')
    expect(app).not.toContain('★ 收藏')
    expect(app).not.toContain('⌃ 置顶')
    expect(styles).not.toContain('.favorite-marker::after')
    expect(styles).not.toContain('.pinned-marker::after')
  })

  /** 验证详情抽屉具有清晰的信息层级和固定操作区。 */
  it('使用结构化记忆详情抽屉', () => {
    const stylesPath = fileURLToPath(new URL('./styles.css', import.meta.url))
    const appPath = fileURLToPath(new URL('./App.vue', import.meta.url))
    const styles = readFileSync(stylesPath, 'utf8')
    const app = normalizeSource(readFileSync(appPath, 'utf8'))

    expect(app).toContain('class="drawer-header"')
    expect(app).toContain('class="drawer-metadata"')
    expect(app).toContain('class="drawer-meta-row"')
    expect(app).toContain('class="drawer-header-actions"')
    expect(app).toContain('class="memory-content-card"')
    expect(app).toContain('class="drawer-body"')
    expect(app).toContain('<footer class="drawer-actions">')
    expect(styles).toContain('grid-template-rows: auto minmax(0, 1fr) auto')
    expect(styles).toContain('.drawer-header-actions button')
    expect(styles).toContain('min-height: 26px')
    expect(styles).not.toContain('.revision-section')
    expect(styles).toContain('.detail-drawer.memory-type-note')
    expect(styles).toContain('.detail-drawer.memory-type-decision')
    expect(styles).toContain('--drawer-type-tint')
  })

  /** 验证总览不再渲染会遮挡页面的搜索框和结果浮层。 */
  it('删除总览搜索框并保留记忆页检索', () => {
    const stylesPath = fileURLToPath(new URL('./styles.css', import.meta.url))
    const appPath = fileURLToPath(new URL('./App.vue', import.meta.url))
    const styles = readFileSync(stylesPath, 'utf8')
    const app = normalizeSource(readFileSync(appPath, 'utf8'))

    expect(app).not.toContain('class="hero-search"')
    expect(app).not.toContain('class="search-popover')
    expect(app).not.toContain('searchPopoverStyle')
    expect(styles).not.toContain('.hero-search')
    expect(styles).not.toContain('.search-popover')
    expect(app).toContain('class="list-search"')
  })
})
