/**
 * 总览图谱预览：真实记忆的 24 节点圆形 Canvas（复用同一圆形布局规则）。
 * 点击进入全局图谱；无交互模拟，静态渲染。
 */
import type { GraphEdge, GraphNode } from '../api'
import {
  DEFAULT_TUNING,
  initializePositions,
  nodePriority,
  settleSimulation,
  type SimulationLink,
} from './layout'

export interface PreviewOptions {
  isDark: boolean
  colorOf: (node: GraphNode) => string
}

/** 渲染 24 节点预览：节点 + 关系 + 中心记忆中枢圆。 */
export function renderGraphPreview(
  canvas: HTMLCanvasElement,
  nodes: GraphNode[],
  edges: GraphEdge[],
  options: PreviewOptions,
): void {
  const bounds = canvas.getBoundingClientRect()
  if (bounds.width < 10 || bounds.height < 10) return
  const ratio = Math.min(window.devicePixelRatio || 1, 2)
  canvas.width = Math.round(bounds.width * ratio)
  canvas.height = Math.round(bounds.height * ratio)
  const context = canvas.getContext('2d')
  if (!context) return
  context.setTransform(ratio, 0, 0, ratio, 0, 0)
  context.clearRect(0, 0, bounds.width, bounds.height)
  if (nodes.length === 0) return

  const seeds = nodes.map((node) => ({
    id: node.id,
    priority: nodePriority({
      isPinned: node.isPinned,
      isFavorite: node.isFavorite,
      importance: node.importance,
      degree: node.degree,
    }),
  }))
  const placed = initializePositions(seeds)
  const links: SimulationLink[] = edges.map((edge) => ({
    source: edge.memoryIdA,
    target: edge.memoryIdB,
    strength: edge.combinedScore,
  }))
  settleSimulation(placed, links, 200, DEFAULT_TUNING)

  const centerX = bounds.width / 2
  const centerY = bounds.height / 2
  // 同一 X/Y 比例：按圆形包围半径缩放到预览画布（保持正圆，不拉伸）。
  let maxRadius = 1
  for (const node of placed) {
    maxRadius = Math.max(maxRadius, Math.hypot(node.x, node.y))
  }
  const previewRadius = Math.min(bounds.width, bounds.height) * 0.4
  const scale = previewRadius / maxRadius
  const positionOf = (id: string) => {
    const node = placed.find((item) => item.id === id) ?? { x: 0, y: 0 }
    return { x: centerX + node.x * scale, y: centerY + node.y * scale }
  }
  const byId = new Map(placed.map((node) => [node.id, node]))

  // 关系线。
  const lineColor = options.isDark ? 'rgba(137, 167, 158, .2)' : 'rgba(87, 117, 109, .2)'
  context.lineWidth = 0.8
  context.strokeStyle = lineColor
  for (const edge of edges) {
    if (!byId.has(edge.memoryIdA) || !byId.has(edge.memoryIdB)) continue
    const from = positionOf(edge.memoryIdA)
    const to = positionOf(edge.memoryIdB)
    context.beginPath()
    context.moveTo(from.x, from.y)
    context.lineTo(to.x, to.y)
    context.stroke()
  }

  // 节点。
  for (const model of nodes) {
    const position = positionOf(model.id)
    const radius = model.isPinned ? 5 : 3.4
    context.beginPath()
    context.arc(position.x, position.y, radius + 2.4, 0, Math.PI * 2)
    context.fillStyle = options.isDark ? 'rgba(17, 25, 23, .92)' : 'rgba(255, 254, 250, .95)'
    context.fill()
    context.beginPath()
    context.arc(position.x, position.y, radius, 0, Math.PI * 2)
    context.fillStyle = options.colorOf(model)
    context.fill()
  }

  // 中心记忆中枢圆。
  context.beginPath()
  context.arc(centerX, centerY, 13, 0, Math.PI * 2)
  context.fillStyle = options.isDark ? 'rgba(25, 34, 31, .96)' : '#fffefa'
  context.fill()
  context.beginPath()
  context.arc(centerX, centerY, 11, 0, Math.PI * 2)
  context.fillStyle = '#087e6c'
  context.fill()
  context.fillStyle = '#ffffff'
  context.font = '9px "Segoe UI", sans-serif'
  context.textAlign = 'center'
  context.textBaseline = 'middle'
  context.fillText('⌬', centerX, centerY + 0.5)
}
