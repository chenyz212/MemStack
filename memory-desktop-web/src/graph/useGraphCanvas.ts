/**
 * 记忆图谱 Canvas 交互组合函数 —— Obsidian 原生图谱美学 × Jarvis UI 光晕效果。
 *
 * 视觉设计：
 * - 深紫星云背景（在 CSS 层已处理，Canvas 在此基础上绘制节点/连线）
 * - 贝塞尔曲线连线，强度决定曲率
 * - 聚焦边：双层发光 + 流动粒子（Jarvis 风格）
 * - 节点：彩色光晕 + 白色核心 + 高光，选中时脉动光环
 * - 节点标签：放大或聚焦时淡出显示
 * - 力导向物理：斥力 + 弹簧连线 + 向心重力
 */
import { onBeforeUnmount, onMounted, type Ref } from 'vue'
import type { GraphEdge, GraphNode } from '../api'
import {
  DEFAULT_TUNING,
  WORLD_RADIUS,
  clampToCircle,
  fitTransform,
  initializePositions,
  isSettled,
  layoutBounds,
  nodePriority,
  stepSimulation,
  type SimulationLink,
  type SimulationNode,
} from './layout'

export interface CanvasNode extends SimulationNode {
  model: GraphNode
  radius: number
  label: string
  phase: number
  targetRadius: number
}

export interface GraphCanvasOptions {
  isDark: () => boolean
  colorOf: (node: GraphNode) => string
  isVisible: (node: GraphNode) => boolean
  minStrength: () => number
  showLabels: () => boolean
  showScores: () => boolean
  search: () => string
  onSelect: (node: GraphNode | null) => void
  onOpen: (node: GraphNode) => void
  reduceMotion: () => boolean
  textFadeThreshold: () => number
  nodeSizeScale: () => number
  linkStrength: () => number
}

const MIN_ZOOM = 0.3
const MAX_ZOOM = 2.6

export function useGraphCanvas(canvasRef: Ref<HTMLCanvasElement | null>, options: GraphCanvasOptions) {
  let nodes: CanvasNode[] = []
  let links: SimulationLink[] = []
  let edgeList: GraphEdge[] = []
  let nodeById = new Map<string, CanvasNode>()
  let visibleIds = new Set<string>()
  let centerId: string | null = null
  let hoveredId: string | null = null
  let selectedId: string | null = null
  let zoom = 1
  let panX = 0
  let panY = 0
  let frame: number | null = null
  let simulationEnergy = 0
  let draggingNodeId: string | null = null
  let draggingCanvas = false
  let lastPointerX = 0
  let lastPointerY = 0
  let pointerStartX = 0
  let pointerStartY = 0
  let pointerMoved = false
  let resizeObserver: ResizeObserver | null = null
  let time = 0

  function setGraph(graphNodes: GraphNode[], graphEdges: GraphEdge[], centerMemoryId: string | null) {
    centerId = centerMemoryId
    edgeList = graphEdges
    const seeds = graphNodes.map((model) => ({
      id: model.id,
      priority: nodePriority({
        isPinned: model.isPinned,
        isFavorite: model.isFavorite,
        importance: model.importance,
        degree: model.degree,
        isCenter: model.id === centerMemoryId,
      }),
    }))
    const placed = initializePositions(seeds)
    const placedById = new Map(placed.map((node) => [node.id, node]))
    nodes = graphNodes.map((model) => {
      const seed = placedById.get(model.id)!
      return {
        id: model.id,
        x: seed.x,
        y: seed.y,
        vx: 0,
        vy: 0,
        fixed: model.id === centerMemoryId,
        model,
        radius: 0,
        label: model.title,
        phase: Math.random() * Math.PI * 2,
        targetRadius: 0,
      }
    })
    nodeById = new Map(nodes.map((node) => [node.id, node]))
    links = graphEdges.map((edge) => ({
      source: edge.memoryIdA,
      target: edge.memoryIdB,
      strength: edge.combinedScore,
    }))
    refreshVisibility()
    updateRadii()
    if (options.reduceMotion()) {
      settleFully()
    } else {
      reheat(1)
    }
    requestRender()
  }

  function settleFully() {
    for (let step = 0; step < 360; step += 1) {
      stepSimulation(nodes, links, DEFAULT_TUNING)
    }
    nodes.forEach((node) => {
      node.vx = 0
      node.vy = 0
    })
  }

  function updateRadii() {
    const scale = options.nodeSizeScale()
    for (const node of nodes) {
      const base = node.id === centerId ? 14 : 5 + Math.min(node.model.degree, 12) * 0.65
      const boost = node.model.isPinned ? 3 : node.model.isFavorite ? 2 : 0
      node.targetRadius = (base + boost) * scale
    }
  }

  function refreshVisibility() {
    visibleIds = new Set(nodes.filter((node) => options.isVisible(node.model)).map((node) => node.id))
  }

  function toScreen(node: { x: number; y: number }, width: number, height: number) {
    return { x: width / 2 + panX + node.x * zoom, y: height / 2 + panY + node.y * zoom }
  }

  function toWorld(screenX: number, screenY: number, width: number, height: number) {
    return { x: (screenX - width / 2 - panX) / zoom, y: (screenY - height / 2 - panY) / zoom }
  }

  function visibleEdges(): GraphEdge[] {
    const min = options.minStrength()
    return edgeList.filter(
      (edge) =>
        edge.combinedScore >= min && visibleIds.has(edge.memoryIdA) && visibleIds.has(edge.memoryIdB),
    )
  }

  /**
   * 核心渲染：Obsidian 深空图谱 + Jarvis UI 光晕。
   * 分层：连线(含聚焦/非聚焦) → 聚焦流动粒子 → 节点 → 标签
   */
  function render() {
    const canvas = canvasRef.value
    if (!canvas) return
    const bounds = canvas.getBoundingClientRect()
    if (bounds.width < 10 || bounds.height < 10) return
    const ratio = Math.min(window.devicePixelRatio || 1, 2)
    const pixelWidth = Math.round(bounds.width * ratio)
    const pixelHeight = Math.round(bounds.height * ratio)
    if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
      canvas.width = pixelWidth
      canvas.height = pixelHeight
    }
    const ctx = canvas.getContext('2d')
    if (!ctx) return
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0)
    const W = bounds.width
    const H = bounds.height
    ctx.clearRect(0, 0, W, H)

    const dark = options.isDark()
    time += 0.016

    const edges = visibleEdges()
    const focusId = hoveredId ?? selectedId
    const neighborIds = new Set<string>()
    const neighborEdges = new Set<string>()
    if (focusId) {
      neighborIds.add(focusId)
      for (const edge of edges) {
        if (edge.memoryIdA === focusId || edge.memoryIdB === focusId) {
          neighborIds.add(edge.memoryIdA)
          neighborIds.add(edge.memoryIdB)
          neighborEdges.add(edge.memoryIdA < edge.memoryIdB ? `${edge.memoryIdA}|${edge.memoryIdB}` : `${edge.memoryIdB}|${edge.memoryIdA}`)
        }
      }
    }
    const search = options.search()

    // 1. 连线层
    drawEdges(ctx, edges, W, H, dark, focusId, neighborIds, neighborEdges)

    // 2. 聚焦流动粒子
    if (focusId != null) drawFlowParticles(ctx, edges, W, H, dark, neighborEdges)

    // 3. 节点层
    drawNodes(ctx, W, H, dark, focusId, neighborIds, search)

    // 4. 标签层
    drawLabels(ctx, W, H, dark, focusId, neighborIds, search)
  }

  /** 绘制连线：贝塞尔曲线。 */
  function drawEdges(
    ctx: CanvasRenderingContext2D,
    edges: GraphEdge[],
    W: number,
    H: number,
    dark: boolean,
    focusId: string | null,
    neighborIds: Set<string>,
    neighborEdges: Set<string>,
  ) {
    const scale = Math.max(0.75, zoom)
    const linkScale = options.linkStrength()
    const baseColor = dark ? 'rgba(200, 180, 240,' : 'rgba(90, 70, 150,'
    const highlightColor = dark ? 'rgba(230, 215, 255,' : 'rgba(80, 50, 140,'

    // 非聚焦边
    for (const edge of edges) {
      const source = nodeById.get(edge.memoryIdA)
      const target = nodeById.get(edge.memoryIdB)
      if (!source || !target) continue
      const key = edge.memoryIdA < edge.memoryIdB ? `${edge.memoryIdA}|${edge.memoryIdB}` : `${edge.memoryIdB}|${edge.memoryIdA}`
      const isNeighborEdge = neighborEdges.has(key)
      if (focusId != null && isNeighborEdge) continue

      const from = toScreen(source, W, H)
      const to = toScreen(target, W, H)

      const alpha = focusId == null
        ? 0.12 + edge.combinedScore * 0.22
        : 0.025
      ctx.strokeStyle = `${baseColor}${alpha})`
      ctx.lineWidth = 0.7 * scale * linkScale
      drawBezier(ctx, from.x, from.y, to.x, to.y, edge.combinedScore)
      ctx.stroke()
    }

    // 聚焦边：先画宽光晕，再画主线
    if (focusId != null) {
      for (const edge of edges) {
        const source = nodeById.get(edge.memoryIdA)
        const target = nodeById.get(edge.memoryIdB)
        if (!source || !target) continue
        const key = edge.memoryIdA < edge.memoryIdB ? `${edge.memoryIdA}|${edge.memoryIdB}` : `${edge.memoryIdB}|${edge.memoryIdA}`
        if (!neighborEdges.has(key)) continue

        const from = toScreen(source, W, H)
        const to = toScreen(target, W, H)
        const alpha = 0.55 + edge.combinedScore * 0.35
        const width = 1.4 * scale * linkScale + edge.combinedScore * 1.0

        // 外发光
        ctx.strokeStyle = `${dark ? 'rgba(200, 170, 255,' : 'rgba(120, 80, 200,'}${alpha * 0.28})`
        ctx.lineWidth = width + 5
        ctx.shadowBlur = 18
        ctx.shadowColor = dark ? 'rgba(200, 170, 255, 0.5)' : 'rgba(120, 80, 200, 0.35)'
        drawBezier(ctx, from.x, from.y, to.x, to.y, edge.combinedScore)
        ctx.stroke()
        ctx.shadowBlur = 0

        // 主线
        ctx.strokeStyle = `${highlightColor}${alpha})`
        ctx.lineWidth = width
        drawBezier(ctx, from.x, from.y, to.x, to.y, edge.combinedScore)
        ctx.stroke()
      }
    }
  }

  /** 聚焦边上的流动粒子。 */
  function drawFlowParticles(
    ctx: CanvasRenderingContext2D,
    edges: GraphEdge[],
    W: number,
    H: number,
    dark: boolean,
    neighborEdges: Set<string>,
  ) {
    const color = dark ? 'rgba(230, 200, 255,' : 'rgba(120, 80, 200,'
    for (const edge of edges) {
      const source = nodeById.get(edge.memoryIdA)
      const target = nodeById.get(edge.memoryIdB)
      if (!source || !target) continue
      const key = edge.memoryIdA < edge.memoryIdB ? `${edge.memoryIdA}|${edge.memoryIdB}` : `${edge.memoryIdB}|${edge.memoryIdA}`
      if (!neighborEdges.has(key)) continue

      const from = toScreen(source, W, H)
      const to = toScreen(target, W, H)

      // 每个边上 3 个粒子循环
      for (let i = 0; i < 3; i++) {
        const t = ((time * 0.35 + i / 3) % 1 + 1) % 1
        const pos = bezierPoint(from.x, from.y, to.x, to.y, t, edge.combinedScore)
        const size = 2.2 + edge.combinedScore * 1.8

        ctx.fillStyle = `${color}0.9)`
        ctx.shadowBlur = 10
        ctx.shadowColor = dark ? 'rgba(230, 200, 255, 0.8)' : 'rgba(120, 80, 200, 0.6)'
        ctx.beginPath()
        ctx.arc(pos.x, pos.y, size, 0, Math.PI * 2)
        ctx.fill()
      }
    }
    ctx.shadowBlur = 0
  }

  /** 绘制节点：彩色光晕 + 白色核心 + 高光。 */
  function drawNodes(
    ctx: CanvasRenderingContext2D,
    W: number,
    H: number,
    dark: boolean,
    focusId: string | null,
    neighborIds: Set<string>,
    search: string,
  ) {
    const scale = Math.max(0.75, zoom)
    for (const node of nodes) {
      if (!visibleIds.has(node.id)) continue
      const pos = toScreen(node, W, H)

      node.radius += (node.targetRadius - node.radius) * 0.15
      node.phase += 0.02

      const highlighted = focusId == null || neighborIds.has(node.id)
      const selected = node.id === selectedId
      const hovered = node.id === hoveredId
      const searchMatch = Boolean(search) && node.label.toLowerCase().includes(search)
      const dim = !highlighted && focusId != null
      const r = Math.max(2, node.radius * scale)

      const nodeColor = options.colorOf(node.model)

      if (dim) {
        ctx.globalAlpha = 0.18
      }

      // —— 外光晕层（多层） ——
      if (!dim) {
        const pulse = selected ? 0.5 + Math.sin(node.phase * 1.6) * 0.3 : 1
        const glowRadius = r * (selected ? 5 * pulse : hovered ? 3.2 : 2.2)
        const glow = ctx.createRadialGradient(pos.x, pos.y, r * 0.5, pos.x, pos.y, glowRadius)
        const glowAlpha = selected ? 0.45 : hovered ? 0.32 : searchMatch ? 0.32 : 0.18
        glow.addColorStop(0, hexToRgba(nodeColor, glowAlpha))
        glow.addColorStop(0.4, hexToRgba(nodeColor, glowAlpha * 0.4))
        glow.addColorStop(1, hexToRgba(nodeColor, 0))
        ctx.beginPath()
        ctx.arc(pos.x, pos.y, glowRadius, 0, Math.PI * 2)
        ctx.fillStyle = glow
        ctx.fill()
      }

      // —— 白色外环 ——
      ctx.beginPath()
      ctx.arc(pos.x, pos.y, r + 1.2, 0, Math.PI * 2)
      ctx.fillStyle = dark ? '#f2ecff' : '#ffffff'
      ctx.fill()

      // —— 彩色核心 ——
      ctx.beginPath()
      ctx.arc(pos.x, pos.y, r, 0, Math.PI * 2)
      ctx.fillStyle = nodeColor
      ctx.fill()

      // —— 高光 ——
      if (!dim) {
        ctx.beginPath()
        ctx.arc(pos.x - r * 0.3, pos.y - r * 0.3, r * 0.38, 0, Math.PI * 2)
        ctx.fillStyle = 'rgba(255, 255, 255, 0.58)'
        ctx.fill()
      }

      // —— 选中/悬停环 ——
      if (selected || hovered) {
        ctx.beginPath()
        ctx.arc(pos.x, pos.y, r + (selected ? 5 : 3.5), 0, Math.PI * 2)
        ctx.strokeStyle = selected
          ? (dark ? 'rgba(230, 215, 255, 0.95)' : 'rgba(120, 80, 200, 0.85)')
          : (dark ? 'rgba(230, 215, 255, 0.55)' : 'rgba(120, 80, 200, 0.5)')
        ctx.lineWidth = selected ? 2 : 1.4
        ctx.stroke()
      }

      // —— 中心标记（center 节点）——
      if (node.id === centerId) {
        const ripple = 0.5 + Math.sin(node.phase * 1.4) * 0.5
        ctx.beginPath()
        ctx.arc(pos.x, pos.y, r + 8 + ripple * 3, 0, Math.PI * 2)
        ctx.strokeStyle = dark ? `rgba(230, 215, 255, ${0.5 - ripple * 0.3})` : `rgba(120, 80, 200, ${0.45 - ripple * 0.3})`
        ctx.lineWidth = 1.3
        ctx.stroke()
      }

      // —— 搜索命中脉冲 ——
      if (searchMatch && !selected) {
        const pulse = 0.55 + Math.sin(node.phase * 2.2) * 0.45
        ctx.beginPath()
        ctx.arc(pos.x, pos.y, r + 6 + pulse * 4, 0, Math.PI * 2)
        ctx.strokeStyle = dark
          ? `rgba(255, 220, 140, ${0.5 - pulse * 0.35})`
          : `rgba(200, 140, 30, ${0.45 - pulse * 0.3})`
        ctx.lineWidth = 1.6
        ctx.stroke()
      }

      ctx.globalAlpha = 1
    }
  }

  /** 绘制标签。 */
  function drawLabels(
    ctx: CanvasRenderingContext2D,
    W: number,
    H: number,
    dark: boolean,
    focusId: string | null,
    neighborIds: Set<string>,
    search: string,
  ) {
    if (!options.showLabels()) return

    const fade = options.textFadeThreshold()
    if (zoom < fade && !focusId) return

    for (const node of nodes) {
      if (!visibleIds.has(node.id)) continue
      const selected = node.id === selectedId
      const hovered = node.id === hoveredId
      const searchMatch = Boolean(search) && node.label.toLowerCase().includes(search)
      const neighbor = focusId != null && neighborIds.has(node.id)
      const important = node.model.isPinned || node.model.degree >= 6 || node.model.importance >= 4

      const show = selected || hovered || searchMatch || neighbor || important
      if (!show) continue

      if (zoom < fade * 1.5 && !selected && !hovered && !searchMatch) continue

      const pos = toScreen(node, W, H)
      const r = node.radius * Math.max(0.75, zoom)
      const text = node.label.length > 24 ? `${node.label.slice(0, 23)}…` : node.label
      const fontSize = selected ? 12.5 : hovered ? 11.5 : 11

      ctx.font = `${selected ? '600' : '500'} ${fontSize}px "SF Pro Text", "PingFang SC", "Microsoft YaHei", system-ui, sans-serif`
      ctx.textAlign = 'left'
      ctx.textBaseline = 'middle'

      const textWidth = ctx.measureText(text).width
      const tagX = pos.x + r + 8
      const tagY = pos.y
      const padX = 7
      const padY = 3

      const bgAlpha = selected ? 0.95 : hovered ? 0.85 : 0.7
      ctx.globalAlpha = bgAlpha
      ctx.fillStyle = dark ? 'rgba(20, 14, 44, 0.92)' : 'rgba(255, 255, 255, 0.94)'
      ctx.beginPath()
      roundRect(ctx, tagX - padX, tagY - fontSize / 2 - padY, textWidth + padX * 2, fontSize + padY * 2, 6)
      ctx.fill()

      ctx.globalAlpha = 1
      ctx.fillStyle = dark ? '#f2ecff' : '#2a1f4a'
      ctx.fillText(text, tagX, tagY)
    }
    ctx.globalAlpha = 1
  }

  function requestRender() {
    if (frame != null) return
    frame = requestAnimationFrame(tick)
  }

  function tick(_now: number) {
    frame = null
    if (document.hidden) return
    if (simulationEnergy > 0.04 || draggingNodeId != null) {
      stepSimulation(nodes, links, DEFAULT_TUNING)
      simulationEnergy *= draggingNodeId != null ? 0.995 : 0.94
      if (isSettled(nodes) && draggingNodeId == null) simulationEnergy = 0
    }
    render()
    if (simulationEnergy > 0.04 || draggingNodeId != null || draggingCanvas) {
      requestRender()
    }
  }

  function reheat(energy = 0.8) {
    if (options.reduceMotion()) {
      settleFully()
      requestRender()
      return
    }
    simulationEnergy = Math.max(simulationEnergy, energy)
    requestRender()
  }

  function nodeAt(clientX: number, clientY: number): string | null {
    const canvas = canvasRef.value
    if (!canvas) return null
    const bounds = canvas.getBoundingClientRect()
    const x = clientX - bounds.left
    const y = clientY - bounds.top
    let found: string | null = null
    for (const node of nodes) {
      if (!visibleIds.has(node.id)) continue
      const position = toScreen(node, bounds.width, bounds.height)
      const hitRadius = Math.max(9, node.radius * zoom + 6)
      if (Math.hypot(position.x - x, position.y - y) <= hitRadius) found = node.id
    }
    return found
  }

  function setZoom(next: number, anchorX?: number, anchorY?: number) {
    const canvas = canvasRef.value
    if (!canvas) return
    const bounds = canvas.getBoundingClientRect()
    const clamped = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, next))
    const x = anchorX ?? bounds.width / 2
    const y = anchorY ?? bounds.height / 2
    const world = toWorld(x, y, bounds.width, bounds.height)
    zoom = clamped
    panX = x - bounds.width / 2 - world.x * zoom
    panY = y - bounds.height / 2 - world.y * zoom
    requestRender()
  }

  function fitView() {
    const canvas = canvasRef.value
    if (!canvas) return
    const bounds = canvas.getBoundingClientRect()
    const visible = nodes.filter((node) => visibleIds.has(node.id))
    const { maxRadius } = layoutBounds(visible)
    const { zoom: fitted } = fitTransform(Math.max(maxRadius, WORLD_RADIUS * 0.7), bounds.width, bounds.height)
    zoom = fitted
    panX = 0
    panY = 0
    requestRender()
  }

  function zoomIn() {
    setZoom(zoom * 1.22)
  }

  function zoomOut() {
    setZoom(zoom / 1.22)
  }

  function setSelected(id: string | null) {
    selectedId = id
    requestRender()
  }

  function onPointerDown(event: PointerEvent) {
    const canvas = canvasRef.value
    if (!canvas) return
    canvas.setPointerCapture(event.pointerId)
    pointerStartX = lastPointerX = event.clientX
    pointerStartY = lastPointerY = event.clientY
    pointerMoved = false
    const hitId = nodeAt(event.clientX, event.clientY)
    if (hitId != null) {
      draggingNodeId = hitId
      const node = nodeById.get(hitId)
      if (node) {
        node.vx = 0
        node.vy = 0
      }
      canvas.style.cursor = 'grabbing'
      reheat(1.1)
    } else {
      draggingCanvas = true
    }
  }

  function onPointerMove(event: PointerEvent) {
    const canvas = canvasRef.value
    if (!canvas) return
    const deltaX = event.clientX - lastPointerX
    const deltaY = event.clientY - lastPointerY
    if (Math.hypot(event.clientX - pointerStartX, event.clientY - pointerStartY) > 3) pointerMoved = true
    if (draggingNodeId != null) {
      const node = nodeById.get(draggingNodeId)
      if (node && !node.fixed) {
        const next = clampToCircle(node.x + deltaX / zoom, node.y + deltaY / zoom)
        node.x = next.x
        node.y = next.y
        node.vx = deltaX / zoom
        node.vy = deltaY / zoom
      }
      reheat(1.15)
    } else if (draggingCanvas) {
      panX += deltaX
      panY += deltaY
      requestRender()
    } else {
      const hovered = nodeAt(event.clientX, event.clientY)
      if (hovered !== hoveredId) {
        hoveredId = hovered
        canvas.style.cursor = hovered == null ? 'grab' : 'pointer'
        requestRender()
      }
    }
    lastPointerX = event.clientX
    lastPointerY = event.clientY
  }

  function onPointerUp(event: PointerEvent) {
    const canvas = canvasRef.value
    if (!canvas) return
    const dragId = draggingNodeId
    if (!pointerMoved && dragId != null) {
      options.onSelect(nodeById.get(dragId)?.model ?? null)
      setSelected(dragId)
    } else if (!pointerMoved && dragId == null && draggingCanvas) {
      options.onSelect(null)
      setSelected(null)
    }
    draggingNodeId = null
    draggingCanvas = false
    canvas.style.cursor = 'grab'
    canvas.releasePointerCapture(event.pointerId)
    reheat(0.8)
  }

  function onPointerLeave() {
    if (hoveredId != null) {
      hoveredId = null
      requestRender()
    }
  }

  function onDoubleClick(event: MouseEvent) {
    const hitId = nodeAt(event.clientX, event.clientY)
    const node = hitId != null ? nodeById.get(hitId) : null
    if (node) options.onOpen(node.model)
  }

  function onWheel(event: WheelEvent) {
    event.preventDefault()
    const canvas = canvasRef.value
    if (!canvas) return
    const bounds = canvas.getBoundingClientRect()
    const factor = event.deltaY < 0 ? 1.12 : 0.89
    setZoom(zoom * factor, event.clientX - bounds.left, event.clientY - bounds.top)
  }

  function onVisibilityChange() {
    if (document.hidden) {
      if (frame != null) {
        cancelAnimationFrame(frame)
        frame = null
      }
    } else {
      requestRender()
    }
  }

  onMounted(() => {
    const canvas = canvasRef.value
    if (!canvas) return
    canvas.style.cursor = 'grab'
    canvas.addEventListener('pointerdown', onPointerDown)
    canvas.addEventListener('pointermove', onPointerMove)
    canvas.addEventListener('pointerup', onPointerUp)
    canvas.addEventListener('pointerleave', onPointerLeave)
    canvas.addEventListener('dblclick', onDoubleClick)
    canvas.addEventListener('wheel', onWheel, { passive: false })
    document.addEventListener('visibilitychange', onVisibilityChange)
    resizeObserver = new ResizeObserver(() => requestRender())
    resizeObserver.observe(canvas)
    requestRender()
  })

  onBeforeUnmount(() => {
    const canvas = canvasRef.value
    if (canvas) {
      canvas.removeEventListener('pointerdown', onPointerDown)
      canvas.removeEventListener('pointermove', onPointerMove)
      canvas.removeEventListener('pointerup', onPointerUp)
      canvas.removeEventListener('pointerleave', onPointerLeave)
      canvas.removeEventListener('dblclick', onDoubleClick)
      canvas.removeEventListener('wheel', onWheel)
    }
    document.removeEventListener('visibilitychange', onVisibilityChange)
    resizeObserver?.disconnect()
    resizeObserver = null
    if (frame != null) {
      cancelAnimationFrame(frame)
      frame = null
    }
  })

  return {
    setGraph,
    refreshVisibility,
    reheat,
    fitView,
    zoomIn,
    zoomOut,
    setSelected,
    requestRender,
    updateRadii,
  }
}

/** 将 hex 颜色转为 rgba。 */
function hexToRgba(hex: string, alpha: number): string {
  const normalized = hex.replace('#', '')
  const r = parseInt(normalized.substring(0, 2), 16)
  const g = parseInt(normalized.substring(2, 4), 16)
  const b = parseInt(normalized.substring(4, 6), 16)
  return `rgba(${r}, ${g}, ${b}, ${alpha})`
}

/** 绘制圆角矩形路径。 */
function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  ctx.moveTo(x + r, y)
  ctx.lineTo(x + w - r, y)
  ctx.quadraticCurveTo(x + w, y, x + w, y + r)
  ctx.lineTo(x + w, y + h - r)
  ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h)
  ctx.lineTo(x + r, y + h)
  ctx.quadraticCurveTo(x, y + h, x, y + h - r)
  ctx.lineTo(x, y + r)
  ctx.quadraticCurveTo(x, y, x + r, y)
}

/** 绘制贝塞尔曲线（曲率由强度决定，0 时退化为直线）。 */
function drawBezier(
  ctx: CanvasRenderingContext2D,
  x1: number,
  y1: number,
  x2: number,
  y2: number,
  strength: number,
) {
  const curvature = (1 - strength) * 0.28 // 强度越低，曲率越大
  const dx = x2 - x1
  const dy = y2 - y1
  const mx = (x1 + x2) / 2
  const my = (y1 + y2) / 2
  // 垂直方向偏移
  const nx = -dy
  const ny = dx
  const len = Math.hypot(nx, ny) || 1
  const offset = curvature * len
  const cx = mx + (nx / len) * offset
  const cy = my + (ny / len) * offset
  ctx.beginPath()
  ctx.moveTo(x1, y1)
  ctx.quadraticCurveTo(cx, cy, x2, y2)
}

/** 计算贝塞尔曲线上的点。 */
function bezierPoint(
  x1: number,
  y1: number,
  x2: number,
  y2: number,
  t: number,
  strength: number,
): { x: number; y: number } {
  const curvature = (1 - strength) * 0.28
  const dx = x2 - x1
  const dy = y2 - y1
  const mx = (x1 + x2) / 2
  const my = (y1 + y2) / 2
  const nx = -dy
  const ny = dx
  const len = Math.hypot(nx, ny) || 1
  const offset = curvature * len
  const cx = mx + (nx / len) * offset
  const cy = my + (ny / len) * offset
  const mt = 1 - t
  const x = mt * mt * x1 + 2 * mt * t * cx + t * t * x2
  const y = mt * mt * y1 + 2 * mt * t * cy + t * t * y2
  return { x, y }
}
