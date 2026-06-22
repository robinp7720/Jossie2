import { useEffect, useMemo, useRef, useState } from 'react'
import * as d3 from 'd3'
import { fetchGraph } from '../api'
import type { ApiConfig } from '../api'
import type { GraphEdge, GraphNode } from '../types'

type KnowledgeGraphProps = { apiConfig: ApiConfig }
type SimulationNode = GraphNode & d3.SimulationNodeDatum
type SimulationLink = GraphEdge & d3.SimulationLinkDatum<SimulationNode>
type GraphConnection = {
  edge: GraphEdge
  node: GraphNode
  direction: 'outgoing' | 'incoming'
}

const INITIAL_NODE_LIMIT = 90
const FILTERED_NODE_LIMIT = 150
const palette = ['#c8ee76', '#8fc7c3', '#b7a5f5', '#e7b773', '#e88886', '#7eaae5']

const shortLabel = (value: string, max = 24) =>
  value.length > max ? `${value.slice(0, max - 1)}…` : value

const readableRelation = (relation: string) => relation
  .trim()
  .replace(/[_-]+/g, ' ')
  .replace(/\s+/g, ' ')
  .toLowerCase()
  .replace(/^./, (letter) => letter.toUpperCase()) || 'Related to'

export function KnowledgeGraph({ apiConfig }: KnowledgeGraphProps) {
  const graphRef = useRef<HTMLDivElement>(null)
  const [data, setData] = useState<{ nodes: GraphNode[]; edges: GraphEdge[] } | null>(null)
  const [filter, setFilter] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [selectedNode, setSelectedNode] = useState<GraphNode | null>(null)
  const [viewport, setViewport] = useState({ width: 0, height: 0 })
  const colorScale = useRef(d3.scaleOrdinal<string, string>().range(palette))

  const loadGraph = async () => {
    setLoading(true)
    setError(null)
    try {
      const result = await fetchGraph(apiConfig, 500)
      setData(result)
    } catch (reason) {
      setData(null)
      setError(reason instanceof Error ? reason.message : 'Unable to load the knowledge graph.')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { void loadGraph() }, [apiConfig.baseUrl])

  useEffect(() => {
    const element = graphRef.current
    if (!element) return
    const resize = () => setViewport({ width: element.clientWidth, height: element.clientHeight })
    resize()
    const observer = new ResizeObserver(resize)
    observer.observe(element)
    return () => observer.disconnect()
  }, [])

  const graph = useMemo(() => {
    if (!data) return null
    const term = filter.trim().toLowerCase()
    const candidates = term
      ? data.nodes.filter((node) => node.label.toLowerCase().includes(term) || node.node_type.toLowerCase().includes(term))
      : data.nodes
    const initialNodes = candidates.slice(0, term ? FILTERED_NODE_LIMIT : INITIAL_NODE_LIMIT)
    const expandedIds = new Set<string>()
    if (selectedNode) {
      expandedIds.add(selectedNode.id)
      data.edges.forEach((edge) => {
        if (edge.source_id === selectedNode.id) expandedIds.add(edge.target_id)
        if (edge.target_id === selectedNode.id) expandedIds.add(edge.source_id)
      })
    }
    const initialIds = new Set(initialNodes.map((node) => node.id))
    const expandedNodes = data.nodes.filter((node) => expandedIds.has(node.id) && !initialIds.has(node.id))
    const visibleNodes = [...initialNodes, ...expandedNodes]
    const visibleIds = new Set(visibleNodes.map((node) => node.id))
    return {
      nodes: visibleNodes,
      edges: data.edges.filter((edge) => visibleIds.has(edge.source_id) && visibleIds.has(edge.target_id)),
      isLimited: candidates.length > visibleNodes.length,
      matchingCount: candidates.length,
      expandedNodeCount: expandedNodes.length,
    }
  }, [data, filter, selectedNode])

  const types = useMemo(() => {
    if (!graph) return []
    return Array.from(new Set(graph.nodes.map((node) => node.node_type || 'Unknown'))).slice(0, 8)
  }, [graph])

  const selectedConnections = useMemo<GraphConnection[]>(() => {
    if (!data || !selectedNode) return []
    const nodesById = new Map(data.nodes.map((node) => [node.id, node]))
    return data.edges.flatMap((edge) => {
      if (edge.source_id === selectedNode.id) {
        const node = nodesById.get(edge.target_id)
        return node ? [{ edge, node, direction: 'outgoing' as const }] : []
      }
      if (edge.target_id === selectedNode.id) {
        const node = nodesById.get(edge.source_id)
        return node ? [{ edge, node, direction: 'incoming' as const }] : []
      }
      return []
    }).sort((left, right) => left.node.label.localeCompare(right.node.label))
  }, [data, selectedNode])

  useEffect(() => {
    if (!graphRef.current || !graph || viewport.width < 1 || viewport.height < 1) return
    const container = d3.select(graphRef.current)
    container.selectAll('svg').remove()
    if (graph.nodes.length === 0) return

    const nodes: SimulationNode[] = graph.nodes.map((node) => ({ ...node }))
    const links: SimulationLink[] = graph.edges.map((edge) => ({ ...edge, source: edge.source_id, target: edge.target_id }))
    const selectedId = selectedNode?.id
    const connectedNodeIds = new Set<string>()
    if (selectedId) {
      connectedNodeIds.add(selectedId)
      links.forEach((edge) => {
        if (edge.source_id === selectedId) connectedNodeIds.add(edge.target_id)
        if (edge.target_id === selectedId) connectedNodeIds.add(edge.source_id)
      })
    }
    const isSelectedLink = (edge: SimulationLink) => edge.source_id === selectedId || edge.target_id === selectedId
    const width = viewport.width
    const height = viewport.height
    const svg = container.append('svg').attr('viewBox', `0 0 ${width} ${height}`).attr('role', 'img').attr('aria-label', 'Knowledge graph')
    const root = svg.append('g')
    svg.call(d3.zoom<SVGSVGElement, unknown>().scaleExtent([0.4, 4]).on('zoom', (event) => root.attr('transform', event.transform)))

    const simulation = d3.forceSimulation(nodes)
      .force('link', d3.forceLink<SimulationNode, SimulationLink>(links).id((node) => node.id).distance(80))
      .force('charge', d3.forceManyBody().strength(-190))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force('collide', d3.forceCollide().radius(20))

    const linksSelection = root.append('g')
      .selectAll('line')
      .data(links)
      .join('line')
      .attr('stroke', (edge) => !selectedId || isSelectedLink(edge) ? 'rgba(190, 223, 124, .8)' : 'rgba(151, 169, 192, .12)')
      .attr('stroke-width', (edge) => Math.max(1, Math.sqrt(edge.weight || 1)))
    const linkLabels = root.append('g')
      .attr('class', 'knowledge-link-labels')
      .selectAll<SVGTextElement, SimulationLink>('text')
      .data(links)
      .join('text')
      .text((edge) => shortLabel(readableRelation(edge.relation), 18))
      .attr('text-anchor', 'middle')
      .attr('dy', -5)
      .attr('fill', (edge) => !selectedId || isSelectedLink(edge) ? '#c9dc9a' : '#65707e')
      .attr('font-size', 8)
      .attr('pointer-events', 'none')
    const nodeSelection = root.append('g').selectAll<SVGGElement, SimulationNode>('g').data(nodes).join('g').attr('class', 'knowledge-node').attr('tabindex', 0)
      .on('click', (event, node) => { event.stopPropagation(); setSelectedNode(node) })
      .on('keydown', (event, node) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); setSelectedNode(node) } })
      .call(d3.drag<SVGGElement, SimulationNode>()
        .on('start', (event, node) => { if (!event.active) simulation.alphaTarget(.2).restart(); node.fx = node.x; node.fy = node.y })
        .on('drag', (event, node) => { node.fx = event.x; node.fy = event.y })
        .on('end', (event, node) => { if (!event.active) simulation.alphaTarget(0); node.fx = null; node.fy = null }))

    nodeSelection.attr('opacity', (node) => !selectedId || connectedNodeIds.has(node.id) ? 1 : .38)
    nodeSelection.append('circle')
      .attr('r', (node) => node.id === selectedId ? 11 : connectedNodeIds.has(node.id) ? 9 : 8)
      .attr('fill', (node) => colorScale.current(node.node_type || 'Unknown'))
      .attr('stroke', (node) => node.id === selectedId ? '#edf9c8' : connectedNodeIds.has(node.id) ? '#c8ee76' : '#0d1219')
      .attr('stroke-width', (node) => node.id === selectedId ? 3 : 2)
    nodeSelection.append('text')
      .text((node) => shortLabel(node.label))
      .attr('x', 12)
      .attr('y', 4)
      .attr('fill', '#dce4d6')
      .attr('font-size', 10)
      .attr('paint-order', 'stroke')
      .attr('stroke', '#111720')
      .attr('stroke-width', 3)
      .attr('stroke-linejoin', 'round')
      .attr('pointer-events', 'none')

    simulation.on('tick', () => {
      linksSelection.attr('x1', (edge) => (edge.source as SimulationNode).x ?? 0).attr('y1', (edge) => (edge.source as SimulationNode).y ?? 0).attr('x2', (edge) => (edge.target as SimulationNode).x ?? 0).attr('y2', (edge) => (edge.target as SimulationNode).y ?? 0)
      linkLabels.attr('x', (edge) => (((edge.source as SimulationNode).x ?? 0) + ((edge.target as SimulationNode).x ?? 0)) / 2).attr('y', (edge) => (((edge.source as SimulationNode).y ?? 0) + ((edge.target as SimulationNode).y ?? 0)) / 2)
      nodeSelection.attr('transform', (node) => `translate(${node.x ?? 0},${node.y ?? 0})`)
    })
    svg.on('click', () => setSelectedNode(null))
    return () => { simulation.stop() }
  }, [graph, selectedNode, viewport])

  const visibleLabel = graph ? `${graph.nodes.length} shown` : 'No nodes'
  return <section className="knowledge-explorer">
    <header className="knowledge-explorer-head">
      <div><p className="eyebrow">EXPLORE RELATIONSHIPS</p><h2>Entity map</h2></div>
      <button className="button secondary graph-refresh" onClick={() => void loadGraph()} disabled={loading}>{loading ? 'Loading…' : 'Refresh'}</button>
    </header>
    <div className="knowledge-controls">
      <input value={filter} onChange={(event) => setFilter(event.target.value)} placeholder="Filter people, projects, or types" aria-label="Filter graph nodes" />
      <div className="knowledge-summary"><span>{visibleLabel}</span><span>{graph?.edges.length ?? 0} links</span>{graph?.expandedNodeCount ? <span>{graph.expandedNodeCount} connected nodes expanded</span> : null}{graph?.isLimited && <span>Refine search to see more</span>}</div>
    </div>
    <div className="knowledge-workspace">
      <div className="knowledge-map-wrap">
        {types.length > 0 && <div className="knowledge-legend">{types.map((type) => <span key={type}><i style={{ background: colorScale.current(type) }} />{type}</span>)}</div>}
        <div className="knowledge-map" ref={graphRef}>
          {loading && <div className="knowledge-empty">Loading graph memory…</div>}
          {error && <div className="knowledge-empty error">{error}</div>}
          {!loading && !error && graph?.nodes.length === 0 && <div className="knowledge-empty">No entities match this filter.</div>}
          {!loading && !error && graph?.nodes.length && graph.nodes.length > 45 ? <div className="knowledge-map-note">Select a node to expand and highlight its direct connections. Zoom and drag to explore.</div> : null}
        </div>
      </div>
      <aside className="knowledge-inspector">
        {selectedNode ? <>
          <div className="inspector-title"><div><p className="eyebrow">SELECTED ENTITY</p><h3>{selectedNode.label}</h3></div><span>{selectedNode.node_type}</span></div>
          <section className="inspector-connections" aria-label={`Connections for ${selectedNode.label}`}>
            <p className="inspector-section-label">CONNECTED ENTITIES ({selectedConnections.length})</p>
            {selectedConnections.length ? <ul>
              {selectedConnections.map(({ edge, node, direction }) => <li key={edge.id}>
                <button type="button" onClick={() => setSelectedNode(node)}>
                  <strong>{node.label}</strong>
                  <span>{direction === 'outgoing' ? '→' : '←'} {readableRelation(edge.relation)}</span>
                  <small>{node.node_type}</small>
                </button>
              </li>)}
            </ul> : <p className="inspector-empty">No connected entities are stored for this node.</p>}
          </section>
          {Object.keys(selectedNode.properties).length > 0 && <details className="inspector-properties">
            <summary>Stored properties</summary>
            <pre>{JSON.stringify(selectedNode.properties, null, 2)}</pre>
          </details>}
          <button className="text-button" onClick={() => setSelectedNode(null)}>Clear selection</button>
        </> : <div className="inspector-placeholder"><p className="eyebrow">INSPECTOR</p><h3>Select an entity</h3><p>Tap a node to view its type and stored properties.</p></div>}
      </aside>
    </div>
  </section>
}
