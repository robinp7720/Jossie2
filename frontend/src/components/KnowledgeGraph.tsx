import { useEffect, useMemo, useRef, useState } from 'react';
import * as d3 from 'd3';
import { fetchGraph } from '../api';
import type { ApiConfig } from '../api';
import type { GraphNode, GraphEdge } from '../types';
import { Button, Chip } from './common/UI';

type KnowledgeGraphProps = {
  apiConfig: ApiConfig;
};

type SimulationNode = GraphNode & d3.SimulationNodeDatum;
type SimulationLink = GraphEdge & d3.SimulationLinkDatum<SimulationNode>;

export const KnowledgeGraph = ({ apiConfig }: KnowledgeGraphProps) => {
  const graphRef = useRef<HTMLDivElement>(null);
  const [data, setData] = useState<{ nodes: GraphNode[]; edges: GraphEdge[] } | null>(null);
  const [filter, setFilter] = useState('');
  const [status, setStatus] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [selectedNode, setSelectedNode] = useState<GraphNode | null>(null);
  const [nodeTypes, setNodeTypes] = useState<string[]>([]);
  const [viewport, setViewport] = useState({ width: 0, height: 0 });
  const colorScale = useRef(d3.scaleOrdinal(d3.schemeTableau10));

  const loadGraph = async () => {
    if (!apiConfig.token) {
      setStatus('Token required');
      setData(null);
      return;
    }

    setLoading(true);
    setStatus('Loading graph…');

    try {
      const result = await fetchGraph(apiConfig, 1000);
      setData(result);
      setStatus(`Loaded ${result.nodes.length} nodes and ${result.edges.length} edges`);
      setNodeTypes(Array.from(new Set(result.nodes.map((node) => node.node_type || 'Unknown'))));
    } catch (error) {
      setStatus(`Error: ${error instanceof Error ? error.message : String(error)}`);
      setData(null);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadGraph();
  }, [apiConfig.token, apiConfig.baseUrl]);

  useEffect(() => {
    const element = graphRef.current;
    if (!element) return;

    const updateSize = () => {
      setViewport({
        width: element.clientWidth || 960,
        height: element.clientHeight || 640,
      });
    };

    updateSize();

    const observer = new ResizeObserver(() => updateSize());
    observer.observe(element);

    return () => observer.disconnect();
  }, []);

  const filteredGraph = useMemo(() => {
    if (!data) return null;

    let nodes = data.nodes;
    let edges = data.edges;

    if (filter.trim()) {
      const term = filter.toLowerCase();
      nodes = nodes.filter(
        (node) =>
          node.label.toLowerCase().includes(term) ||
          node.node_type.toLowerCase().includes(term),
      );

      const nodeIds = new Set(nodes.map((node) => node.id));
      edges = edges.filter(
        (edge) => nodeIds.has(edge.source_id) && nodeIds.has(edge.target_id),
      );
    }

    return { nodes, edges };
  }, [data, filter]);

  useEffect(() => {
    if (!filteredGraph || !graphRef.current || viewport.width === 0 || viewport.height === 0) {
      return;
    }

    const nodes: SimulationNode[] = filteredGraph.nodes.map((node) => ({ ...node }));
    const links: SimulationLink[] = filteredGraph.edges.map((edge) => ({
      ...edge,
      source: edge.source_id,
      target: edge.target_id,
    }));

    const container = d3.select(graphRef.current);
    container.selectAll('svg').remove();

    if (nodes.length === 0) {
      return;
    }

    const width = viewport.width;
    const height = viewport.height;

    const svg = container
      .append('svg')
      .attr('width', '100%')
      .attr('height', '100%')
      .attr('viewBox', [0, 0, width, height])
      .attr('preserveAspectRatio', 'xMidYMid meet');

    const root = svg.append('g');

    svg.call(
      d3
        .zoom<SVGSVGElement, unknown>()
        .scaleExtent([0.35, 4])
        .on('zoom', (event) => {
          root.attr('transform', event.transform);
        }),
    );

    const simulation = d3
      .forceSimulation(nodes)
      .force(
        'link',
        d3
          .forceLink<SimulationNode, SimulationLink>(links)
          .id((node) => node.id)
          .distance(110),
      )
      .force('charge', d3.forceManyBody().strength(-300))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force('collide', d3.forceCollide().radius(26));

    const link = root
      .append('g')
      .attr('stroke', 'rgba(117, 140, 156, 0.45)')
      .selectAll('line')
      .data(links)
      .join('line')
      .attr('stroke-width', (edge) => Math.max(1, Math.sqrt(edge.weight || 1)));

    const node = root
      .append('g')
      .selectAll<SVGGElement, SimulationNode>('g')
      .data(nodes)
      .join('g')
      .attr('cursor', 'pointer')
      .on('click', (event, datum) => {
        setSelectedNode(datum);
        event.stopPropagation();
      })
      .call(
        d3
          .drag<SVGGElement, SimulationNode>()
          .on('start', (event, datum) => {
            if (!event.active) simulation.alphaTarget(0.2).restart();
            datum.fx = datum.x;
            datum.fy = datum.y;
          })
          .on('drag', (event, datum) => {
            datum.fx = event.x;
            datum.fy = event.y;
          })
          .on('end', (event, datum) => {
            if (!event.active) simulation.alphaTarget(0);
            datum.fx = null;
            datum.fy = null;
          }),
      );

    node
      .append('circle')
      .attr('r', 11)
      .attr('fill', (datum) => colorScale.current(datum.node_type))
      .attr('stroke', '#f4eee3')
      .attr('stroke-width', 2.5);

    node
      .append('text')
      .text((datum) => datum.label)
      .attr('x', 16)
      .attr('y', 4)
      .attr('fill', '#0e1f2b')
      .style('font-size', '11px')
      .style('font-weight', '700')
      .style('letter-spacing', '0.02em')
      .style('pointer-events', 'none');

    simulation.on('tick', () => {
      link
        .attr('x1', (datum) => (datum.source as SimulationNode).x ?? 0)
        .attr('y1', (datum) => (datum.source as SimulationNode).y ?? 0)
        .attr('x2', (datum) => (datum.target as SimulationNode).x ?? 0)
        .attr('y2', (datum) => (datum.target as SimulationNode).y ?? 0);

      node.attr('transform', (datum) => `translate(${datum.x ?? 0},${datum.y ?? 0})`);
    });

    svg.on('click', () => setSelectedNode(null));

    return () => {
      simulation.stop();
    };
  }, [filteredGraph, viewport]);

  return (
    <div className="graph-shell">
      <div className="graph-toolbar">
        <div className="graph-toolbar-copy">
          <p className="eyebrow">Graph explorer</p>
          <h2>Entity map</h2>
        </div>
        <div className="graph-toolbar-actions">
          <input
            type="text"
            placeholder="Filter nodes by label or type"
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
            className="graph-search"
          />
          <Button variant="ghost" onClick={loadGraph} loading={loading}>
            Refresh
          </Button>
        </div>
      </div>

      <div className="graph-summary">
        <Chip variant="neutral">{filteredGraph?.nodes.length ?? 0} nodes</Chip>
        <Chip variant="neutral">{filteredGraph?.edges.length ?? 0} edges</Chip>
        <Chip variant={status?.startsWith('Error') ? 'warning' : 'accent'}>
          {status ?? 'Idle'}
        </Chip>
      </div>

      <div className="graph-workspace">
        <div className="graph-main">
          {nodeTypes.length > 0 ? (
            <div className="graph-legend">
              {nodeTypes.map((type) => (
                <div key={type} className="legend-item">
                  <span
                    className="legend-dot"
                    style={{ backgroundColor: colorScale.current(type) }}
                  />
                  <span>{type}</span>
                </div>
              ))}
            </div>
          ) : null}

          <div className="graph-container" ref={graphRef}>
            {!filteredGraph && !loading ? (
              <div className="graph-empty">
                Configure an auth token, then refresh to load graph memory.
              </div>
            ) : null}
            {filteredGraph && filteredGraph.nodes.length === 0 ? (
              <div className="graph-empty">No nodes match the current filter.</div>
            ) : null}
          </div>
        </div>

        <aside className="graph-sidebar">
          {selectedNode ? (
            <div className="graph-node-panel">
              <div className="graph-node-head">
                <div>
                  <p className="eyebrow">Selected node</p>
                  <h3>{selectedNode.label}</h3>
                </div>
                <Chip variant="accent">{selectedNode.node_type}</Chip>
              </div>
              <pre className="code graph-node-code">
                {JSON.stringify(selectedNode.properties, null, 2)}
              </pre>
              <Button variant="ghost" size="sm" onClick={() => setSelectedNode(null)}>
                Clear selection
              </Button>
            </div>
          ) : (
            <div className="graph-node-panel empty">
              <p className="eyebrow">Inspector</p>
              <h3>Select a node</h3>
              <p className="support-copy">
                Click any node in the graph to inspect its stored properties and type.
              </p>
            </div>
          )}
        </aside>
      </div>
    </div>
  );
};
