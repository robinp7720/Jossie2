import { useEffect, useRef, useState } from 'react'
import * as d3 from 'd3'
import { fetchGraph } from '../api'
import type { ApiConfig } from '../api'
import type { GraphNode, GraphEdge } from '../types'

type KnowledgeGraphProps = {
  apiConfig: ApiConfig
}

type SimulationNode = GraphNode & d3.SimulationNodeDatum
type SimulationLink = GraphEdge & d3.SimulationLinkDatum<SimulationNode>

export const KnowledgeGraph = ({ apiConfig }: KnowledgeGraphProps) => {
  const graphRef = useRef<HTMLDivElement>(null)
  const [data, setData] = useState<{ nodes: GraphNode[]; edges: GraphEdge[] } | null>(null)
  const [filter, setFilter] = useState('')
  const [status, setStatus] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  
  // Legend state
  const [nodeTypes, setNodeTypes] = useState<string[]>([])
  const colorScale = useRef(d3.scaleOrdinal(d3.schemeTableau10))

  const loadGraph = async () => {
    if (!apiConfig.token) {
      setStatus('Token required')
      return
    }
    setLoading(true)
    setStatus('Loading...')
    try {
      const result = await fetchGraph(apiConfig, 1000)
      setData(result)
      setStatus(`Loaded ${result.nodes.length} nodes`)
      
      const types = Array.from(new Set(result.nodes.map(n => n.node_type || 'Unknown')))
      setNodeTypes(types)
    } catch (e) {
      setStatus(`Error: ${e instanceof Error ? e.message : String(e)}`)
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    loadGraph()
  }, [apiConfig.token, apiConfig.baseUrl])

  // D3 Rendering
  useEffect(() => {
    if (!data || !graphRef.current) return

    // Filter logic
    let nodes: SimulationNode[] = data.nodes.map(n => ({ ...n }))
    // Map edges to D3 links, ensuring source/target are set from source_id/target_id
    let links: SimulationLink[] = data.edges.map(e => ({ 
      ...e,
      source: e.source_id,
      target: e.target_id
    }))

    if (filter.trim()) {
      const term = filter.toLowerCase()
      nodes = nodes.filter(n => 
        n.label.toLowerCase().includes(term) || 
        n.node_type.toLowerCase().includes(term)
      )
      const nodeIds = new Set(nodes.map(n => n.id))
      // Filter links where both endpoints exist in the filtered node set
      links = links.filter(l => nodeIds.has(l.source as string) && nodeIds.has(l.target as string))
    }

    if (nodes.length === 0) {
      d3.select(graphRef.current).selectAll('svg').remove()
      return
    }

    // Clear previous
    const container = d3.select(graphRef.current)
    container.selectAll('svg').remove()

    const width = graphRef.current.clientWidth || 800
    const height = graphRef.current.clientHeight || 600

    const svg = container.append('svg')
      .attr('width', '100%')
      .attr('height', '100%')
      .attr('viewBox', [0, 0, width, height])
      .attr('style', 'max-width: 100%; height: auto;')

    const g = svg.append('g')

    // Zoom
    const zoom = d3.zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.1, 4])
      .on('zoom', (event) => {
        g.attr('transform', event.transform)
      })
    svg.call(zoom)

    // Simulation
    const simulation = d3.forceSimulation(nodes)
      .force('link', d3.forceLink<SimulationNode, SimulationLink>(links).id(d => d.id).distance(100))
      .force('charge', d3.forceManyBody().strength(-300))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force('collide', d3.forceCollide().radius(20))

    // Links
    const link = g.append('g')
      .attr('stroke', '#999')
      .attr('stroke-opacity', 0.6)
      .selectAll('line')
      .data(links)
      .join('line')
      .attr('stroke-width', d => Math.sqrt(d.weight || 1))

    // Nodes
    const node = g.append('g')
      .attr('stroke', '#fff')
      .attr('stroke-width', 1.5)
      .selectAll('g')
      .data(nodes)
      .join('g')
      .call(d3.drag<SVGGElement, SimulationNode>()
        .on('start', dragstarted)
        .on('drag', dragged)
        .on('end', dragended))

    node.append('circle')
      .attr('r', 8)
      .attr('fill', d => colorScale.current(d.node_type))

    node.append('title')
      .text(d => `${d.label} (${d.node_type})
${JSON.stringify(d.properties)}`)

    node.append('text')
      .text(d => d.label)
      .attr('x', 10)
      .attr('y', 4)
      .attr('stroke', 'none')
      .attr('fill', '#333')
      .style('font-size', '10px')
      .style('pointer-events', 'none')

    link.append('title').text(d => d.relation)

    simulation.on('tick', () => {
      link
        .attr('x1', d => (d.source as SimulationNode).x!)
        .attr('y1', d => (d.source as SimulationNode).y!)
        .attr('x2', d => (d.target as SimulationNode).x!)
        .attr('y2', d => (d.target as SimulationNode).y!)

      node
        .attr('transform', d => `translate(${d.x},${d.y})`)
    })

    function dragstarted(event: any, d: SimulationNode) {
      if (!event.active) simulation.alphaTarget(0.3).restart()
      d.fx = d.x
      d.fy = d.y
    }

    function dragged(event: any, d: SimulationNode) {
      d.fx = event.x
      d.fy = event.y
    }

    function dragended(event: any, d: SimulationNode) {
      if (!event.active) simulation.alphaTarget(0)
      d.fx = null
      d.fy = null
    }

    return () => {
      simulation.stop()
    }

  }, [data, filter])

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', gap: '16px' }}>
      <div className="graph-controls">
        <input 
          type="text" 
          placeholder="Filter nodes..." 
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          className="form input"
          style={{ minWidth: '250px', padding: '10px', borderRadius: '12px', border: '1px solid var(--border)' }}
        />
        <button className="button ghost" onClick={loadGraph} disabled={loading}>
          {loading ? 'Refreshing...' : 'Refresh'}
        </button>
        <span style={{ alignSelf: 'center', fontSize: '14px', color: 'var(--ink-soft)' }}>{status}</span>
      </div>
      
      {nodeTypes.length > 0 && (
        <div className="graph-legend">
          {nodeTypes.map(type => (
            <div key={type} className="legend-item">
              <span className="legend-dot" style={{ backgroundColor: colorScale.current(type) }} />
              <span>{type}</span>
            </div>
          ))}
        </div>
      )}

      <div className="graph-container" ref={graphRef}>
        {!data && !loading && (
          <div className="graph-empty">
            Graph data not loaded
          </div>
        )}
        {data && data.nodes.length === 0 && (
          <div className="graph-empty">
            No nodes found
          </div>
        )}
      </div>
    </div>
  )
}
