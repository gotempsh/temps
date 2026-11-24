// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Where each part of an architecture blueprint sits in its diagram.
 *
 * The layout is layered and computed, with no graph library: one column per
 * kind of part, in the order data travels (who uses it, the code Temps runs,
 * the state it keeps, the services outside), and apps ordered by role so the
 * pages sit above the API and the API above the workers. The other columns are
 * ordered by what they connect to, so a part sits level with the apps that use
 * it and fewer flows cross.
 *
 * Flows are drawn as curves between the facing edges of their two parts. Each
 * flow gets its own point on those edges, spread out in the order of the part
 * at the other end, so flows fan out instead of leaving from a single point,
 * and each label sits by its own arrowhead rather than in the middle, where
 * crossing flows would put their labels on top of each other.
 */

export type BlueprintKind = 'client' | 'app' | 'data' | 'external'

export interface LayoutComponent {
  id: string
  kind: BlueprintKind
  role?: string | null
}

export interface LayoutFlow {
  from: string
  to: string
}

export interface PlacedNode {
  id: string
  x: number
  y: number
  width: number
  height: number
}

export interface PlacedEdge {
  index: number
  from: string
  to: string
  /** SVG path data for the curve. */
  path: string
  /** Where the flow's label sits: just before its arrowhead. */
  labelX: number
  labelY: number
  /** How the label is aligned on `labelX`, as SVG `text-anchor`. */
  labelAnchor: 'start' | 'middle' | 'end'
}

export interface BlueprintLayout {
  nodes: PlacedNode[]
  edges: PlacedEdge[]
  width: number
  height: number
  /** The column titles that have parts, with their x position. */
  columns: Array<{ kind: BlueprintKind; title: string; x: number }>
}

export const NODE_WIDTH = 180
export const NODE_HEIGHT = 58
/** Room between columns for the curves and the labels by their arrowheads. */
const COLUMN_GAP = 128
/** Space kept between a flow's label and the edge its arrow points at. */
const LABEL_OFFSET = 12
const ROW_GAP = 22
const PADDING = 16
const HEADER = 22

const COLUMN_ORDER: BlueprintKind[] = ['client', 'app', 'data', 'external']

const COLUMN_TITLES: Record<BlueprintKind, string> = {
  client: 'Used from',
  app: 'Runs on Temps',
  data: 'Keeps state',
  external: 'Outside services',
}

const ROLE_ORDER = ['web', 'static', 'api', 'worker', 'cron']

function roleRank(role: string | null | undefined): number {
  const rank = ROLE_ORDER.indexOf(role ?? '')
  return rank === -1 ? ROLE_ORDER.length : rank
}

export function layoutBlueprint(
  components: readonly LayoutComponent[],
  flows: readonly LayoutFlow[]
): BlueprintLayout {
  const columns = COLUMN_ORDER.map((kind) =>
    components
      .map((component, index) => ({ component, index }))
      .filter(({ component }) => component.kind === kind)
      .sort(
        (a, b) =>
          (kind === 'app'
            ? roleRank(a.component.role) - roleRank(b.component.role)
            : 0) || a.index - b.index
      )
      .map(({ component }) => component)
  )
  const used = columns
    .map((parts, index) => ({ parts, kind: COLUMN_ORDER[index] }))
    .filter(({ parts }) => parts.length > 0)
  const tallest = Math.max(0, ...used.map(({ parts }) => parts.length))
  const contentHeight =
    tallest * NODE_HEIGHT + Math.max(0, tallest - 1) * ROW_GAP

  const neighbours = new Map<string, string[]>()
  for (const flow of flows) {
    if (flow.from === flow.to) continue
    neighbours.set(flow.from, [...(neighbours.get(flow.from) ?? []), flow.to])
    neighbours.set(flow.to, [...(neighbours.get(flow.to) ?? []), flow.from])
  }

  const placed = new Map<string, PlacedNode>()
  const columnX = (column: number) =>
    PADDING + column * (NODE_WIDTH + COLUMN_GAP)
  const place = (parts: LayoutComponent[], column: number) => {
    const height = parts.length * NODE_HEIGHT + (parts.length - 1) * ROW_GAP
    // Shorter columns are centred against the tallest one.
    const top = PADDING + HEADER + (contentHeight - height) / 2
    parts.forEach((part, row) => {
      placed.set(part.id, {
        id: part.id,
        x: columnX(column),
        y: top + row * (NODE_HEIGHT + ROW_GAP),
        width: NODE_WIDTH,
        height: NODE_HEIGHT,
      })
    })
  }
  // Apps keep their role order; every other column then lines up with what it
  // is connected to. Columns are placed apps first, so each later column sees
  // the positions it is sorted against.
  const appColumn = used.findIndex(({ kind }) => kind === 'app')
  const placementOrder = used
    .map((_, column) => column)
    .sort((a, b) => Number(b === appColumn) - Number(a === appColumn))
  for (const column of placementOrder) {
    const { parts } = used[column]
    place(
      column === appColumn ? parts : byConnections(parts, neighbours, placed),
      column
    )
  }
  const nodes = used.flatMap(({ parts }) =>
    parts.flatMap((part) => placed.get(part.id) ?? [])
  )

  const routed = flows
    .map((flow, index) => ({
      flow,
      index,
      from: placed.get(flow.from),
      to: placed.get(flow.to),
    }))
    .filter(
      (edge): edge is typeof edge & { from: PlacedNode; to: PlacedNode } =>
        !!edge.from && !!edge.to && edge.from !== edge.to
    )
  const ports = assignPorts(routed)
  const edges: PlacedEdge[] = routed.map(({ flow, index, from, to }) => ({
    index,
    from: flow.from,
    to: flow.to,
    ...curve(from, to, ports.get(`${index}:from`), ports.get(`${index}:to`)),
  }))

  const width =
    used.length === 0
      ? 0
      : PADDING * 2 + used.length * NODE_WIDTH + (used.length - 1) * COLUMN_GAP
  return {
    nodes,
    edges,
    width,
    height: used.length === 0 ? 0 : PADDING * 2 + HEADER + contentHeight,
    columns: used.map(({ kind }, column) => ({
      kind,
      title: COLUMN_TITLES[kind],
      x: columnX(column),
    })),
  }
}

const centreY = (node: PlacedNode) => node.y + node.height / 2

/**
 * Sort a column by the average height of the placed parts it connects to.
 * Parts with no placed connection keep their document order, after the rest.
 */
function byConnections(
  parts: LayoutComponent[],
  neighbours: ReadonlyMap<string, string[]>,
  placed: ReadonlyMap<string, PlacedNode>
): LayoutComponent[] {
  const weight = (part: LayoutComponent) => {
    const ys = (neighbours.get(part.id) ?? []).flatMap((id) => {
      const node = placed.get(id)
      return node ? [centreY(node)] : []
    })
    return ys.length === 0
      ? Number.POSITIVE_INFINITY
      : ys.reduce((sum, y) => sum + y, 0) / ys.length
  }
  return parts
    .map((part, index) => ({ part, index, weight: weight(part) }))
    .sort((a, b) => a.weight - b.weight || a.index - b.index)
    .map(({ part }) => part)
}

type Side = 'left' | 'right'

/**
 * Which edge of each end a flow uses: facing edges between columns, and the
 * right edge of both for a flow within one column.
 */
function sides(from: PlacedNode, to: PlacedNode): { from: Side; to: Side } {
  if (from.x === to.x) return { from: 'right', to: 'right' }
  return to.x > from.x
    ? { from: 'right', to: 'left' }
    : { from: 'left', to: 'right' }
}

/**
 * Give every flow its own height on each edge it touches. Flows sharing an
 * edge are spread evenly over it in the order of the part at their other end,
 * so the fan does not cross itself.
 */
function assignPorts(
  edges: ReadonlyArray<{ index: number; from: PlacedNode; to: PlacedNode }>
): Map<string, number> {
  const groups = new Map<
    string,
    Array<{ key: string; otherY: number; index: number; node: PlacedNode }>
  >()
  const add = (
    node: PlacedNode,
    side: Side,
    key: string,
    other: PlacedNode,
    index: number
  ) => {
    const group = `${node.id}:${side}`
    groups.set(group, [
      ...(groups.get(group) ?? []),
      { key, otherY: centreY(other), index, node },
    ])
  }
  for (const { index, from, to } of edges) {
    const side = sides(from, to)
    add(from, side.from, `${index}:from`, to, index)
    add(to, side.to, `${index}:to`, from, index)
  }
  const ports = new Map<string, number>()
  for (const members of groups.values()) {
    members.sort((a, b) => a.otherY - b.otherY || a.index - b.index)
    members.forEach((member, slot) => {
      const { node } = member
      ports.set(
        member.key,
        node.y + (node.height * (slot + 1)) / (members.length + 1)
      )
    })
  }
  return ports
}

function curve(
  from: PlacedNode,
  to: PlacedNode,
  fromY = centreY(from),
  toY = centreY(to)
): Pick<PlacedEdge, 'path' | 'labelX' | 'labelY' | 'labelAnchor'> {
  if (from.x === to.x) {
    // Same column: leave from the right edge and come back in a loop.
    const startX = from.x + from.width
    const endX = to.x + to.width
    const bulge = startX + 36
    return {
      path: `M ${startX} ${fromY} C ${bulge} ${fromY}, ${bulge} ${toY}, ${endX} ${toY}`,
      labelX: bulge + 4,
      labelY: (fromY + toY) / 2 + 3,
      labelAnchor: 'start',
    }
  }
  const forward = to.x > from.x
  const startX = forward ? from.x + from.width : from.x
  const endX = forward ? to.x : to.x + to.width
  const midX = (startX + endX) / 2
  return {
    path: `M ${startX} ${fromY} C ${midX} ${fromY}, ${midX} ${toY}, ${endX} ${toY}`,
    // The curve arrives level, so a label just above its last stretch reads
    // as belonging to that arrow and to no other.
    labelX: forward ? endX - LABEL_OFFSET : endX + LABEL_OFFSET,
    labelY: toY - 4,
    labelAnchor: forward ? 'end' : 'start',
  }
}

/**
 * Parts and flows a newer version adds, changes or removes compared with an
 * older one, for highlighting a proposal against what was accepted.
 */
export function blueprintChanges<
  C extends { id: string },
  F extends { from: string; to: string; kind: string; label: string },
>(
  previous: { components: readonly C[]; flows: readonly F[] } | null,
  next: { components: readonly C[]; flows: readonly F[] }
): {
  added: Set<string>
  changed: Set<string>
  removed: C[]
  addedFlows: Set<number>
  removedFlows: F[]
} {
  const flowKey = (flow: F) =>
    `${flow.from}>${flow.to}:${flow.kind}:${flow.label.trim()}`
  if (!previous) {
    return {
      added: new Set(),
      changed: new Set(),
      removed: [],
      addedFlows: new Set(),
      removedFlows: [],
    }
  }
  const before = new Map(previous.components.map((c) => [c.id, c]))
  const after = new Map(next.components.map((c) => [c.id, c]))
  const added = new Set<string>()
  const changed = new Set<string>()
  for (const component of next.components) {
    const old = before.get(component.id)
    if (!old) added.add(component.id)
    else if (JSON.stringify(old) !== JSON.stringify(component))
      changed.add(component.id)
  }
  const removed = previous.components.filter((c) => !after.has(c.id))
  const oldFlows = new Set(previous.flows.map(flowKey))
  const newFlows = new Set(next.flows.map(flowKey))
  const addedFlows = new Set<number>()
  next.flows.forEach((flow, index) => {
    if (!oldFlows.has(flowKey(flow))) addedFlows.add(index)
  })
  const removedFlows = previous.flows.filter((f) => !newFlows.has(flowKey(f)))
  return { added, changed, removed, addedFlows, removedFlows }
}

/**
 * The order to deploy the bound app parts in: an app another app calls is
 * deployed first, so a front end goes out after the API it talks to. Data and
 * outside parts are not deployed. Cycles fall back to the document order.
 */
export function deployOrder(
  components: readonly LayoutComponent[],
  flows: readonly LayoutFlow[]
): string[] {
  const apps = components.filter((c) => c.kind === 'app').map((c) => c.id)
  const appSet = new Set(apps)
  // `a` depends on `b` when a flow goes from `a` to the app `b`.
  const dependsOn = new Map(apps.map((id) => [id, new Set<string>()]))
  for (const flow of flows) {
    if (appSet.has(flow.from) && appSet.has(flow.to) && flow.from !== flow.to) {
      dependsOn.get(flow.from)?.add(flow.to)
    }
  }
  const order: string[] = []
  const done = new Set<string>()
  const visiting = new Set<string>()
  const visit = (id: string) => {
    if (done.has(id) || visiting.has(id)) return
    visiting.add(id)
    for (const dependency of dependsOn.get(id) ?? []) visit(dependency)
    visiting.delete(id)
    done.add(id)
    order.push(id)
  }
  apps.forEach(visit)
  return order
}
