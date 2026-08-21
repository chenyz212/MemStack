<script setup lang="ts">
/**
 * 记忆图谱视图 —— Obsidian 原生图谱美学重构版。
 *
 * 参考项目：
 * - Obsidian 官方 Graph View（深色星空 + 简洁节点 + 右侧设置面板）
 * - Jarvis UI（3D bloom + 流动粒子 + 星云氛围）
 * - obsidian-graph-styler（Neon / Galaxy / Aurora 主题美学）
 * - ObsiGraph（Pixi.JS + D3 force 的高性能实现）
 *
 * 设计方向：深紫星云背景 + 细贝塞尔连线 + 节点光晕 +
 * 玻璃面板 UI + 衬线标题 + 精确动效。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import {
  apiFetch,
  type GraphEdge,
  type GraphNode,
  type GraphResult,
  type ProjectItem,
} from '../api'
import { useGraphCanvas } from './useGraphCanvas'

const props = defineProps<{
  projects: ProjectItem[]
  isDark: boolean
  initialMemoryId: string | null
}>()

const emit = defineEmits<{
  (event: 'open-memory', memoryId: string): void
  (event: 'open-search', query: string): void
}>()

type GraphMode = 'global' | 'local'

const mode = ref<GraphMode>(props.initialMemoryId ? 'local' : 'global')
const localCenterId = ref<string | null>(props.initialMemoryId)
const graph = ref<GraphResult | null>(null)
const isLoading = ref(false)
const loadError = ref('')
const search = ref('')
const showLabels = ref(true)
const showScores = ref(false)
const strengthPercent = ref(45)
const timeRange = ref<'all' | '7' | '30'>('all')
const activeProjects = ref<Set<string>>(new Set())
const includePersonal = ref(true)
const filterPanelOpen = ref(true)
const selectedNode = ref<GraphNode | null>(null)
const rebuildQueued = ref(false)

const textFadeThreshold = ref(0.6)
const nodeSizeScale = ref(1)
const linkStrength = ref(1)

const canvasRoot = ref<HTMLCanvasElement | null>(null)

const memoryTypeLabels: Record<string, string> = {
  NOTE: '笔记',
  PREFERENCE: '偏好',
  DECISION: '决策',
  SOLUTION: '方案',
  FACT: '事实',
  CONVENTION: '约定',
  TASK: '任务',
  CONTEXT: '上下文',
  OTHER: '其他',
}

const PERSONAL_COLOR_LIGHT = '#7c5cb8'
const PERSONAL_COLOR_DARK = '#c8b4f0'

const graphProjects = computed(() => props.projects.filter((project) => !project.isArchived))

const projectColorMap = computed(() => {
  const map = new Map<string, string>()
  const fallback = ['#9d7fe8', '#6f5ce0', '#4a90d9', '#d9822b', '#c2579a', '#5f7cd9']
  let fallbackIndex = 0
  for (const project of graphProjects.value) {
    if (project.color && project.color.trim()) {
      map.set(project.id, project.color)
    } else {
      map.set(project.id, fallback[fallbackIndex % fallback.length])
      fallbackIndex += 1
    }
  }
  return map
})

function colorOf(node: GraphNode): string {
  if (node.scope === 'Personal' || !node.projectId) {
    return props.isDark ? PERSONAL_COLOR_DARK : PERSONAL_COLOR_LIGHT
  }
  return projectColorMap.value.get(node.projectId) ?? (props.isDark ? '#7188dd' : '#4a65c7')
}

const isReducedMotion = computed(
  () => typeof window !== 'undefined'
    && window.matchMedia('(prefers-reduced-motion: reduce)').matches,
)

const canvas = useGraphCanvas(canvasRoot, {
  isDark: () => props.isDark,
  colorOf,
  isVisible: (node) => {
    if (node.scope === 'Personal') return includePersonal.value
    return node.projectId != null && activeProjects.value.has(node.projectId)
  },
  minStrength: () => strengthPercent.value / 100,
  showLabels: () => showLabels.value,
  showScores: () => showScores.value,
  search: () => search.value.trim().toLowerCase(),
  reduceMotion: () => isReducedMotion.value,
  textFadeThreshold: () => textFadeThreshold.value,
  nodeSizeScale: () => nodeSizeScale.value,
  linkStrength: () => linkStrength.value,
  onSelect: (node) => {
    selectedNode.value = node
  },
  onOpen: (node) => {
    emit('open-memory', node.id)
  },
})

function neighborListOf(node: GraphNode): Array<{ node: GraphNode; score: number; signal: string }> {
  const edges = graph.value?.edges ?? []
  const byId = new Map((graph.value?.nodes ?? []).map((item) => [item.id, item]))
  const result: Array<{ node: GraphNode; score: number; signal: string }> = []
  for (const edge of edges) {
    let otherId: string | null = null
    if (edge.memoryIdA === node.id) otherId = edge.memoryIdB
    else if (edge.memoryIdB === node.id) otherId = edge.memoryIdA
    if (otherId == null) continue
    const other = byId.get(otherId)
    if (other) {
      result.push({ node: other, score: edge.combinedScore, signal: edge.dominantSignal })
    }
  }
  return result.sort((left, right) => right.score - left.score).slice(0, 8)
}

const selectedNeighbors = computed(() => (selectedNode.value ? neighborListOf(selectedNode.value) : []))

const signalLabels: Record<string, string> = {
  SEMANTIC: '语义',
  KEYWORD: '关键词',
  MIXED: '语义 + 关键词',
}

const legendEntries = computed(() => {
  const entries: Array<{ key: string; label: string; color: string }> = []
  if (includePersonal.value) {
    entries.push({
      key: 'personal',
      label: '个人记忆',
      color: props.isDark ? PERSONAL_COLOR_DARK : PERSONAL_COLOR_LIGHT,
    })
  }
  for (const project of graphProjects.value) {
    if (activeProjects.value.has(project.id)) {
      entries.push({
        key: project.id,
        label: project.name,
        color: colorOf({ scope: 'Project', projectId: project.id } as GraphNode),
      })
    }
  }
  return entries
})

const searchLower = computed(() => search.value.trim().toLowerCase())
const searchMatches = computed(() => {
  if (!searchLower.value) return 0
  return (graph.value?.nodes ?? []).filter((node) => node.title.toLowerCase().includes(searchLower.value)).length
})

const summaryText = computed(() => {
  if (!graph.value) return '正在加载图谱…'
  if (mode.value === 'local') {
    return `两跳邻域 · ${graph.value.nodes.length} 个节点 · ${graph.value.edges.length} 条关系`
  }
  const truncated = graph.value.truncated ? `（已截断，共 ${graph.value.totalNodes}）` : ''
  return `${graph.value.nodes.length} 个节点 · ${graph.value.edges.length} 条关系${truncated}`
})

const allProjectsActive = computed(
  () => includePersonal.value && graphProjects.value.every((project) => activeProjects.value.has(project.id)),
)

function buildGlobalQuery(): string {
  const params = new URLSearchParams()
  params.set('projectIds', [...activeProjects.value].join(','))
  params.set('includePersonal', String(includePersonal.value))
  if (timeRange.value !== 'all') params.set('days', timeRange.value)
  params.set('limit', '120')
  params.set('minScore', '0.30')
  return params.toString()
}

async function loadGlobal(): Promise<void> {
  isLoading.value = true
  loadError.value = ''
  try {
    const result = await apiFetch<GraphResult>(`/api/graph/global?${buildGlobalQuery()}`, { method: 'GET' }, null)
    graph.value = result
    selectedNode.value = null
    canvas.setGraph(result.nodes, result.edges, null)
    canvas.fitView()
  } catch (error) {
    loadError.value = error instanceof Error ? error.message : '图谱加载失败'
  } finally {
    isLoading.value = false
  }
}

async function loadLocal(memoryId: string): Promise<void> {
  isLoading.value = true
  loadError.value = ''
  try {
    const params = new URLSearchParams({ memoryId, depth: '2', limit: '120', minScore: '0.30' })
    const result = await apiFetch<GraphResult>(`/api/graph/neighborhood?${params}`, { method: 'GET' }, null)
    graph.value = result
    localCenterId.value = memoryId
    selectedNode.value = result.nodes.find((node) => node.id === memoryId) ?? null
    canvas.setGraph(result.nodes, result.edges, memoryId)
    canvas.fitView()
  } catch (error) {
    loadError.value = error instanceof Error ? error.message : '局部图谱加载失败'
  } finally {
    isLoading.value = false
  }
}

async function reload(): Promise<void> {
  if (mode.value === 'local' && localCenterId.value) {
    await loadLocal(localCenterId.value)
  } else {
    await loadGlobal()
  }
}

function switchMode(next: GraphMode): void {
  if (mode.value === next) return
  mode.value = next
  if (next === 'local') {
    const center = selectedNode.value?.id ?? localCenterId.value ?? graph.value?.nodes[0]?.id
    if (center) {
      void loadLocal(center)
    } else {
      mode.value = 'global'
    }
  } else {
    localCenterId.value = null
    void loadGlobal()
  }
}

function selectNode(node: GraphNode | null): void {
  selectedNode.value = node
  canvas.setSelected(node?.id ?? null)
}

watch(selectedNode, (node) => {
  if (mode.value === 'local' && node && node.id !== localCenterId.value) {
    void loadLocal(node.id)
  }
})

function toggleProject(projectId: string): void {
  const next = new Set(activeProjects.value)
  if (next.has(projectId)) next.delete(projectId)
  else next.add(projectId)
  activeProjects.value = next
  canvas.refreshVisibility()
  canvas.reheat(0.5)
}

function toggleAllProjects(): void {
  const enable = !allProjectsActive.value
  includePersonal.value = enable
  activeProjects.value = enable ? new Set(graphProjects.value.map((project) => project.id)) : new Set()
  canvas.refreshVisibility()
  canvas.reheat(0.5)
}

function togglePersonal(): void {
  includePersonal.value = !includePersonal.value
  canvas.refreshVisibility()
  canvas.reheat(0.5)
}

function onStrengthInput(): void {
  canvas.reheat(0.4)
  canvas.requestRender()
}

function onTimeRangeChange(): void {
  void reload()
}

watch(searchLower, (value) => {
  if (!value) return
  const match = (graph.value?.nodes ?? []).find((node) => node.title.toLowerCase().includes(value))
  if (match) {
    selectNode(match)
  }
})

async function queueRebuild(): Promise<void> {
  try {
    await apiFetch('/api/graph/rebuild', { method: 'POST' }, null)
    rebuildQueued.value = true
    window.setTimeout(() => {
      rebuildQueued.value = false
      void reload()
    }, 2600)
  } catch {
    loadError.value = '排队重建失败，请稍后重试'
  }
}

function openFullMemory(): void {
  if (selectedNode.value) emit('open-memory', selectedNode.value.id)
}

function searchRelated(): void {
  emit('open-search', selectedNode.value?.title ?? '')
}

function onKeyDown(event: KeyboardEvent): void {
  if (event.ctrlKey && event.key.toLowerCase() === 'f') {
    event.preventDefault()
    document.querySelector<HTMLInputElement>('.graph-search input')?.focus()
  }
}

watch(
  () => props.isDark,
  () => canvas.requestRender(),
)

let buildingReloadTimer: number | null = null
watch(
  () => graph.value?.buildStatus,
  (status) => {
    if (status === 'BUILDING' && buildingReloadTimer == null) {
      buildingReloadTimer = window.setTimeout(() => {
        buildingReloadTimer = null
        if (!isLoading.value) void reload()
      }, 2600)
    }
  },
)

onMounted(() => {
  activeProjects.value = new Set(graphProjects.value.map((project) => project.id))
  window.addEventListener('keydown', onKeyDown)
  if (mode.value === 'local' && localCenterId.value) {
    void loadLocal(localCenterId.value)
  } else {
    void loadGlobal()
  }
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeyDown)
  if (buildingReloadTimer != null) {
    window.clearTimeout(buildingReloadTimer)
    buildingReloadTimer = null
  }
})

defineExpose({ reload })
</script>

<template>
  <section class="graph-page page-enter">
    <!-- 顶部标题栏 -->
    <header class="graph-topbar">
      <div class="graph-identity">
        <span class="graph-orbit">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="12" cy="12" r="2.2"></circle>
            <ellipse cx="12" cy="12" rx="10" ry="4.5" transform="rotate(28 12 12)"></ellipse>
            <circle cx="4" cy="6.5" r="1" fill="currentColor"></circle>
            <circle cx="19.5" cy="17.5" r="1" fill="currentColor"></circle>
          </svg>
        </span>
        <div>
          <h2>知识图谱</h2>
          <span class="graph-subtitle">
            {{ summaryText }}
            <i :class="['graph-build-dot', graph?.buildStatus === 'BUILDING' ? 'is-building' : '']"></i>
            {{ graph?.buildStatus === 'BUILDING' ? '正在建立关联…' : '实时同步' }}
          </span>
        </div>
      </div>

      <nav class="graph-scope" role="tablist" aria-label="图谱范围">
        <button
          type="button"
          :class="{ active: mode === 'global' }"
          role="tab"
          @click="switchMode('global')"
        >
          <span class="scope-dot"></span>
          全局
        </button>
        <button
          type="button"
          :class="{ active: mode === 'local' }"
          role="tab"
          @click="switchMode('local')"
        >
          <span class="scope-ring"></span>
          局部
        </button>
      </nav>
    </header>

    <!-- 画布主舞台 -->
    <div class="graph-stage">
      <!-- 星云装饰 -->
      <div class="graph-nebula"></div>
      <div class="graph-stars"></div>

      <canvas ref="canvasRoot" tabindex="0" aria-label="可平移、缩放和拖动节点的记忆关系图谱"></canvas>

      <!-- 中央搜索 -->
      <div class="graph-search" :class="{ 'has-match': search.trim() && searchMatches > 0, 'no-match': search.trim() && searchMatches === 0 }">
        <span class="search-glyph">⌕</span>
        <input
          v-model="search"
          placeholder="搜索记忆标题…"
          aria-label="搜索图谱中的记忆"
        />
        <kbd>⌘F</kbd>
        <small v-if="search.trim()" class="search-counter">{{ searchMatches }} / {{ (graph?.nodes?.length ?? 0) }}</small>
      </div>

      <!-- 状态遮罩 -->
      <transition name="fade">
        <div v-if="isLoading" class="graph-overlay">
          <span class="overlay-spinner"></span>
          <p>正在从记忆深处唤醒关系…</p>
        </div>
        <div v-else-if="loadError" class="graph-overlay">
          <span class="overlay-icon danger">!</span>
          <p>{{ loadError }}</p>
          <button type="button" class="ghost-button" @click="reload">重试</button>
        </div>
        <div v-else-if="graph && graph.nodes.length === 0" class="graph-overlay">
          <span class="overlay-constellation">✦</span>
          <h4>{{ graph.totalNodes > 0 ? '筛选已隐藏全部节点' : '还没有可展示的关系' }}</h4>
          <p>
            {{
              graph.totalNodes > 0
                ? '试试在右侧面板中放开项目或个人记忆。'
                : '创建更多记忆后，它们会自动建立语义与关键词之间的联系。'
            }}
          </p>
          <div class="overlay-actions">
            <button v-if="graph.totalNodes > 0" type="button" class="ghost-button" @click="toggleAllProjects">
              显示全部
            </button>
            <button v-else type="button" class="ghost-button accent" @click="queueRebuild">
              {{ rebuildQueued ? '已排队，构建中…' : '立即重建关系' }}
            </button>
          </div>
        </div>
      </transition>

      <!-- 右下节点详情（卡片式） -->
      <transition name="slide-in-right">
        <article v-if="selectedNode" class="graph-detail">
          <button class="detail-close" type="button" aria-label="关闭详情" @click="selectNode(null)">×</button>
          <header class="detail-head">
            <span class="memory-type-chip" :style="{ '--chip-hue': colorOf(selectedNode) }">
              {{ memoryTypeLabels[selectedNode.memoryType.toUpperCase()] ?? '记忆' }}
            </span>
            <span v-if="selectedNode.importance" class="importance-pips" :style="'--count:' + selectedNode.importance">
              <i v-for="i in 5" :key="i"></i>
            </span>
          </header>
          <h3 class="detail-title">{{ selectedNode.title }}</h3>
          <p class="detail-summary">{{ selectedNode.summary || '（这条记忆没有摘要）' }}</p>

          <div class="detail-meta">
            <span class="meta-pill">
              <i :style="{ background: colorOf(selectedNode) }"></i>
              {{ selectedNode.projectName ?? '个人记忆' }}
            </span>
          </div>

          <div v-if="selectedNode.tags.length" class="detail-tags">
            <span v-for="tag in selectedNode.tags" :key="tag">#{{ tag }}</span>
          </div>

          <button type="button" class="primary-btn" @click="openFullMemory">打开完整记忆</button>

          <footer v-if="selectedNeighbors.length" class="detail-foot">
            <header>
              <span>关联记忆</span>
              <b>{{ selectedNeighbors.length }}</b>
            </header>
            <ul class="neighbor-list">
              <li v-for="neighbor in selectedNeighbors" :key="neighbor.node.id">
                <button type="button" @click="selectNode(neighbor.node)">
                  <i :style="{ background: colorOf(neighbor.node) }"></i>
                  <div>
                    <strong>{{ neighbor.node.title }}</strong>
                    <small>{{ signalLabels[neighbor.signal] ?? '关联' }} · {{ Math.round(neighbor.score * 100) }}%</small>
                  </div>
                </button>
              </li>
            </ul>
          </footer>
        </article>
      </transition>

      <!-- 右上控制面板 -->
      <aside class="graph-panel" :class="{ collapsed: !filterPanelOpen }">
        <header class="graph-panel-head">
          <span class="panel-title">控制</span>
          <button
            class="panel-toggle"
            type="button"
            :aria-label="filterPanelOpen ? '折叠面板' : '展开面板'"
            @click="filterPanelOpen = !filterPanelOpen"
          >
            <span class="chevron" :class="{ open: filterPanelOpen }"></span>
          </button>
        </header>

        <transition name="fade-slide" mode="out-in">
          <div v-if="filterPanelOpen" class="graph-panel-body" key="body">
            <section class="panel-section">
              <header>显示</header>
              <label class="panel-row">
                <span>节点标签</span>
                <input v-model="showLabels" type="checkbox" />
              </label>
              <label class="panel-row">
                <span>关系分数</span>
                <input v-model="showScores" type="checkbox" />
              </label>
            </section>

            <section class="panel-section">
              <header>筛选</header>
              <div class="panel-block">
                <span class="panel-label">项目</span>
                <button type="button" :class="['chip-all', { active: allProjectsActive }]" @click="toggleAllProjects">
                  全部
                </button>
                <div class="chip-grid">
                  <button
                    v-for="project in graphProjects"
                    :key="project.id"
                    type="button"
                    :class="['chip', { active: activeProjects.has(project.id) }]"
                    :style="{ '--chip-color': colorOf({ scope: 'Project', projectId: project.id } as GraphNode) }"
                    @click="toggleProject(project.id)"
                  >
                    {{ project.name }}
                  </button>
                  <button
                    type="button"
                    :class="['chip', { active: includePersonal }]"
                    :style="{ '--chip-color': isDark ? PERSONAL_COLOR_DARK : PERSONAL_COLOR_LIGHT }"
                    @click="togglePersonal"
                  >
                    个人
                  </button>
                </div>
              </div>
            </section>

            <section class="panel-section">
              <header>关系</header>
              <div class="panel-row slider-row">
                <span>最低强度</span>
                <div class="slider-wrap">
                  <input v-model.number="strengthPercent" type="range" min="10" max="90" @input="onStrengthInput" />
                  <output>{{ strengthPercent }}%</output>
                </div>
              </div>
              <div class="panel-row slider-row">
                <span>连线粗细</span>
                <div class="slider-wrap">
                  <input v-model.number="linkStrength" type="range" min="0.3" max="2.5" step="0.1" @input="onStrengthInput" />
                  <output>{{ linkStrength.toFixed(1) }}×</output>
                </div>
              </div>
              <div class="panel-row slider-row">
                <span>节点大小</span>
                <div class="slider-wrap">
                  <input v-model.number="nodeSizeScale" type="range" min="0.5" max="2" step="0.1" @input="canvas.updateRadii(); canvas.reheat(0.4)" />
                  <output>{{ nodeSizeScale.toFixed(1) }}×</output>
                </div>
              </div>
              <div class="panel-row slider-row">
                <span>文字淡入</span>
                <div class="slider-wrap">
                  <input v-model.number="textFadeThreshold" type="range" min="0.2" max="1.5" step="0.1" @input="canvas.requestRender()" />
                  <output>{{ textFadeThreshold.toFixed(1) }}×</output>
                </div>
              </div>
            </section>

            <section class="panel-section">
              <header>时间范围</header>
              <div class="segmented">
                <button type="button" :class="{ active: timeRange === 'all' }" @click="timeRange = 'all'; onTimeRangeChange()">不限</button>
                <button type="button" :class="{ active: timeRange === '30' }" @click="timeRange = '30'; onTimeRangeChange()">30 天</button>
                <button type="button" :class="{ active: timeRange === '7' }" @click="timeRange = '7'; onTimeRangeChange()">7 天</button>
              </div>
            </section>

            <section v-if="legendEntries.length" class="panel-section">
              <header>图例</header>
              <div class="legend-list">
                <span v-for="entry in legendEntries" :key="entry.key">
                  <i :style="{ background: entry.color }"></i>{{ entry.label }}
                </span>
              </div>
            </section>

            <section class="panel-section help-section">
              <header>操作</header>
              <div class="help-list">
                <span>拖拽节点会带动邻接</span>
                <span>双击节点打开记忆</span>
                <span>滚轮缩放 · 空白平移</span>
              </div>
            </section>
          </div>
        </transition>
      </aside>

      <!-- 底部工具 -->
      <div class="graph-tools">
        <button type="button" aria-label="放大" @click="canvas.zoomIn()">＋</button>
        <button type="button" aria-label="缩小" @click="canvas.zoomOut()">−</button>
        <button type="button" aria-label="适应窗口" @click="canvas.fitView()">⌗</button>
        <span></span>
        <button type="button" aria-label="重建关系" :disabled="rebuildQueued" @click="queueRebuild">
          {{ rebuildQueued ? '构建中…' : '重建关系' }}
        </button>
      </div>

      <button v-if="selectedNode" type="button" class="graph-search-related" @click="searchRelated">
        ⌕ 搜索相关
      </button>
    </div>
  </section>
</template>
