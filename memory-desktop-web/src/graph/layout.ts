/**
 * 圆形 Obsidian 记忆图谱布局算法（纯函数，无 DOM 依赖，可单测）。
 *
 * 实施计划 §圆形布局算法：
 * - 初始坐标使用确定性黄金角圆盘分布（sunflower），不用随机坐标：
 *   角度按黄金角递增，半径按 sqrt(index / count) 分布。
 * - 重要度高、置顶、收藏和连接数高的节点优先排在圆心附近。
 * - 局部图谱把中心记忆固定在 (0, 0)；全局图谱只固定整体质心。
 * - 自定义圆形边界力：接近半径边界时渐进向心，越界后按圆周法线拉回；
 *   节点拖动同样限制在圆形边界内（禁止矩形截断）。
 * - 力模拟稳定后立即停止；筛选/拖动/切换中心时由调用方重新升温。
 */

/** 黄金角（弧度）：137.50776405003785°。 */
export const GOLDEN_ANGLE = Math.PI * (3 - Math.sqrt(5));

/** 世界坐标下的圆形节点云半径（固定值；视口适配由缩放完成）。 */
export const WORLD_RADIUS = 420;

/** 硬边界：任何节点不得越过（相对 WORLD_RADIUS 的比例）。 */
export const HARD_BOUNDARY_FACTOR = 1.0;

/** 软边界：超过该半径开始渐进向心。 */
const SOFT_BOUNDARY_FACTOR = 0.78;

/** 布局输入种子：priority 越大越靠近圆心。 */
export interface LayoutSeed {
  id: string
  priority: number
}

/** 模拟节点。 */
export interface SimulationNode {
  id: string
  x: number
  y: number
  vx: number
  vy: number
  /** 固定不动（局部图谱中心记忆）。 */
  fixed: boolean
}

/** 模拟边（无向），strength ∈ 0..1。 */
export interface SimulationLink {
  source: string
  target: string
  strength: number
}

/** 一次模拟步进的配置。 */
export interface SimulationTuning {
  /** 斥力强度。 */
  repulsion: number
  /** 连线弹簧强度。 */
  spring: number
  /** 期望边长（弱关系更长）。 */
  linkDistance: number
  /** 全局质心回拉强度。 */
  gravity: number
  /** 速度衰减。 */
  damping: number
}

export const DEFAULT_TUNING: SimulationTuning = {
  repulsion: 620,
  spring: 0.026,
  linkDistance: 96,
  gravity: 0.016,
  damping: 0.7,
};

/** 黄金角圆盘初始分布：priority 降序后按 sunflower 公式落位。 */
export function initializePositions(seeds: LayoutSeed[], radius: number = WORLD_RADIUS): SimulationNode[] {
  const ordered = [...seeds].sort((left, right) => right.priority - left.priority);
  const count = ordered.length;
  return ordered.map((seed, index) => {
    const angle = index * GOLDEN_ANGLE;
    // index 0 在圆心；其余按 sqrt 均匀铺满圆盘。
    const ringRadius = radius * Math.sqrt(index / Math.max(1, count));
    return {
      id: seed.id,
      x: Math.cos(angle) * ringRadius,
      y: Math.sin(angle) * ringRadius,
      vx: 0,
      vy: 0,
      fixed: false,
    };
  });
}

/** 计算节点优先级：置顶 > 收藏 > 重要度 > 连接数（权重递减）。 */
export function nodePriority(input: {
  isPinned: boolean
  isFavorite: boolean
  importance: number
  degree: number
  isCenter?: boolean
}): number {
  if (input.isCenter) return 1e6;
  return (
    (input.isPinned ? 4096 : 0) +
    (input.isFavorite ? 2048 : 0) +
    input.importance * 256 +
    Math.min(input.degree, 16) * 16
  );
}

/** 单步力模拟：斥力 + 弹簧 + 质心回拉 + 圆形边界力。 */
export function stepSimulation(
  nodes: SimulationNode[],
  links: SimulationLink[],
  tuning: SimulationTuning = DEFAULT_TUNING,
): void {
  const index = new Map(nodes.map((node, position) => [node.id, position] as const));
  const forces: Array<{ x: number; y: number }> = nodes.map(() => ({ x: 0, y: 0 }));

  // 成对斥力（O(n²)，n ≤ 300 可接受）。
  for (let first = 0; first < nodes.length; first += 1) {
    for (let second = first + 1; second < nodes.length; second += 1) {
      let dx = nodes[first].x - nodes[second].x;
      let dy = nodes[first].y - nodes[second].y;
      let distanceSquared = dx * dx + dy * dy;
      if (distanceSquared < 0.01) {
        dx = (first % 2 === 0 ? 1 : -1) * 0.1;
        dy = (second % 2 === 0 ? 1 : -1) * 0.1;
        distanceSquared = 0.02;
      }
      const distance = Math.sqrt(distanceSquared);
      const repulsion = Math.min(5.5, tuning.repulsion / Math.max(900, distanceSquared));
      const unitX = dx / distance;
      const unitY = dy / distance;
      forces[first].x += unitX * repulsion;
      forces[first].y += unitY * repulsion;
      forces[second].x -= unitX * repulsion;
      forces[second].y -= unitY * repulsion;
    }
  }

  // 连线弹簧：强关系更短。
  for (const link of links) {
    const source = nodes[index.get(link.source) ?? -1];
    const target = nodes[index.get(link.target) ?? -1];
    if (!source || !target) continue;
    const dx = target.x - source.x;
    const dy = target.y - source.y;
    const distance = Math.max(1, Math.hypot(dx, dy));
    const desired = tuning.linkDistance * (1.35 - link.strength * 0.55);
    const pull = (distance - desired) * tuning.spring * (0.7 + link.strength * 0.6);
    const unitX = dx / distance;
    const unitY = dy / distance;
    if (!source.fixed) {
      forces[index.get(link.source)!].x += unitX * pull;
      forces[index.get(link.source)!].y += unitY * pull;
    }
    if (!target.fixed) {
      forces[index.get(link.target)!].x -= unitX * pull;
      forces[index.get(link.target)!].y -= unitY * pull;
    }
  }

  const softRadius = WORLD_RADIUS * SOFT_BOUNDARY_FACTOR;
  const hardRadius = WORLD_RADIUS * HARD_BOUNDARY_FACTOR;
  for (let position = 0; position < nodes.length; position += 1) {
    const node = nodes[position];
    if (node.fixed) continue;
    // 全局质心回拉（保持圆形云整体居中，不指定中心记忆）。
    forces[position].x -= node.x * tuning.gravity;
    forces[position].y -= node.y * tuning.gravity;
    node.vx = (node.vx + forces[position].x) * tuning.damping;
    node.vy = (node.vy + forces[position].y) * tuning.damping;
    node.x += node.vx;
    node.y += node.vy;
    // 圆形边界：软边界外渐进向心，硬边界按法线拉回。
    const distance = Math.hypot(node.x, node.y);
    if (distance > softRadius && distance > 0) {
      const overshoot = (distance - softRadius) / (hardRadius - softRadius);
      const pullback = 0.18 + Math.min(1, overshoot) * 0.6;
      node.x -= (node.x / distance) * (distance - softRadius) * pullback;
      node.y -= (node.y / distance) * (distance - softRadius) * pullback;
    }
    const finalDistance = Math.hypot(node.x, node.y);
    if (finalDistance > hardRadius && finalDistance > 0) {
      node.x = (node.x / finalDistance) * hardRadius;
      node.y = (node.y / finalDistance) * hardRadius;
      node.vx *= 0.5;
      node.vy *= 0.5;
    }
  }
}

/** 模拟是否已稳定（全部节点动能低于阈值）。 */
export function isSettled(nodes: SimulationNode[], threshold = 0.05): boolean {
  return nodes.every((node) => Math.abs(node.vx) + Math.abs(node.vy) < threshold);
}

/** 逐步运行至稳定或达到上限步数，返回实际步数。 */
export function settleSimulation(
  nodes: SimulationNode[],
  links: SimulationLink[],
  maxSteps = 420,
  tuning: SimulationTuning = DEFAULT_TUNING,
): number {
  let step = 0;
  while (step < maxSteps) {
    stepSimulation(nodes, links, tuning);
    step += 1;
    if (step % 12 === 0 && isSettled(nodes)) break;
  }
  return step;
}

/** 节点云的边界统计：最大半径与 X/Y 跨度（布局验收用）。 */
export function layoutBounds(nodes: Array<{ x: number; y: number }>): {
  maxRadius: number
  spanX: number
  spanY: number
} {
  let maxRadius = 0;
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (const node of nodes) {
    maxRadius = Math.max(maxRadius, Math.hypot(node.x, node.y));
    minX = Math.min(minX, node.x);
    maxX = Math.max(maxX, node.x);
    minY = Math.min(minY, node.y);
    maxY = Math.max(maxY, node.y);
  }
  if (!Number.isFinite(minX)) {
    return { maxRadius: 0, spanX: 0, spanY: 0 };
  }
  return { maxRadius, spanX: maxX - minX, spanY: maxY - minY };
}

/** 把一个点约束在圆形边界内（节点拖动时使用）。 */
export function clampToCircle(x: number, y: number, radius: number = WORLD_RADIUS): { x: number; y: number } {
  const distance = Math.hypot(x, y);
  if (distance <= radius || distance === 0) return { x, y };
  return { x: (x / distance) * radius, y: (y / distance) * radius };
}

/** 适应窗口：按圆形包围半径计算居中缩放（留 8% 边距）。 */
export function fitTransform(
  boundingRadius: number,
  viewportWidth: number,
  viewportHeight: number,
): { zoom: number } {
  const usable = Math.min(viewportWidth, viewportHeight) * 0.42;
  if (boundingRadius <= 0) return { zoom: 1 };
  return { zoom: Math.max(0.2, Math.min(2.4, usable / boundingRadius)) };
}
