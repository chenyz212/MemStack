import { describe, expect, it } from 'vitest'
import {
  HARD_BOUNDARY_FACTOR,
  WORLD_RADIUS,
  initializePositions,
  layoutBounds,
  nodePriority,
  settleSimulation,
  type SimulationLink,
  type LayoutSeed,
} from './layout'

/** 确定性的合成图数据：sunflower + 邻近连线。 */
function syntheticGraph(count: number): { seeds: LayoutSeed[]; links: SimulationLink[] } {
  const seeds: LayoutSeed[] = []
  for (let index = 0; index < count; index += 1) {
    seeds.push({
      id: `node-${index}`,
      priority: nodePriority({
        isPinned: index % 17 === 0,
        isFavorite: index % 9 === 0,
        importance: (index % 5) + 1,
        degree: index % 8,
      }),
    })
  }
  const links: SimulationLink[] = [];
  for (let index = 1; index < count; index += 1) {
    links.push({
      source: `node-${index}`,
      target: `node-${Math.floor((index - 1) / 2)}`,
      strength: 0.45 + ((index * 7) % 50) / 100,
    })
  }
  return { seeds, links }
}

describe('圆形布局算法', () => {
  it('120 节点稳定后 95% 位于目标圆形半径内', () => {
    const { seeds, links } = syntheticGraph(120)
    const nodes = initializePositions(seeds)
    settleSimulation(nodes, links)
    const bounds = layoutBounds(nodes)
    const within = nodes.filter((node) => Math.hypot(node.x, node.y) <= WORLD_RADIUS).length
    expect(within / nodes.length).toBeGreaterThanOrEqual(0.95)
    expect(bounds.maxRadius).toBeLessThanOrEqual(WORLD_RADIUS * HARD_BOUNDARY_FACTOR + 1e-6)
  })

  it('300 节点稳定后所有节点不越过圆形硬边界', () => {
    const { seeds, links } = syntheticGraph(300)
    const nodes = initializePositions(seeds)
    settleSimulation(nodes, links)
    for (const node of nodes) {
      expect(Math.hypot(node.x, node.y)).toBeLessThanOrEqual(WORLD_RADIUS * HARD_BOUNDARY_FACTOR + 1e-6)
    }
  })

  it('稳定布局的 X/Y 跨度比在 0.90–1.10（不得拉成长条或椭圆）', () => {
    for (const count of [24, 72, 120, 300]) {
      const { seeds, links } = syntheticGraph(count)
      const nodes = initializePositions(seeds)
      settleSimulation(nodes, links)
      const { spanX, spanY } = layoutBounds(nodes)
      const ratio = spanX / Math.max(1e-6, spanY)
      expect(ratio).toBeGreaterThanOrEqual(0.9)
      expect(ratio).toBeLessThanOrEqual(1.1)
    }
  })

  it('初始分布是确定性的黄金角圆盘', () => {
    const seeds: LayoutSeed[] = Array.from({ length: 40 }, (_, index) => ({ id: `n${index}`, priority: index }))
    const first = initializePositions(seeds)
    const second = initializePositions(seeds)
    expect(first).toEqual(second)
    // 高优先级节点更靠近圆心。
    const firstRadius = Math.hypot(first[0].x, first[0].y)
    const lastRadius = Math.hypot(first[first.length - 1].x, first[first.length - 1].y)
    expect(firstRadius).toBeLessThan(lastRadius)
    // 半径按 sqrt(index/count) 分布。
    const expected = WORLD_RADIUS * Math.sqrt(10 / 40)
    expect(Math.hypot(first[10].x, first[10].y)).toBeCloseTo(expected, 6)
  })

  it('节点优先级：置顶 > 收藏 > 重要度 > 度数', () => {
    const pinned = nodePriority({ isPinned: true, isFavorite: false, importance: 1, degree: 0 })
    const favorite = nodePriority({ isPinned: false, isFavorite: true, importance: 5, degree: 8 })
    expect(pinned).toBeGreaterThan(favorite)
    const highImportance = nodePriority({ isPinned: false, isFavorite: false, importance: 5, degree: 0 })
    const lowImportance = nodePriority({ isPinned: false, isFavorite: false, importance: 1, degree: 8 })
    expect(highImportance).toBeGreaterThan(lowImportance)
  })
})
