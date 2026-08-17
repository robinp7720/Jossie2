import { useEffect, useMemo } from 'react'
import { fetchGraph } from '../api'
import { KnowledgeGraph } from '../components/KnowledgeGraph'
import { api } from '../config'
import { useLatestAsync } from '../hooks/useLatestAsync'
import type { GraphEdge, GraphNode } from '../types'

export function Knowledge() {
  const { value, error, clearError, run } = useLatestAsync<{
    nodes: GraphNode[]
    edges: GraphEdge[]
  }>({ nodes: [], edges: [] }, 'Unable to load knowledge summary.')
  useEffect(() => {
    void run(() => fetchGraph(api, 500))
  }, [run])
  const nodes = value.nodes
  const types = useMemo(
    () => new Set(nodes.map((node) => node.node_type)).size,
    [nodes],
  )
  return (
    <section className="page knowledge-page">
      <header className="page-head">
        <div>
          <p className="eyebrow">CONNECTED CONTEXT</p>
          <h1>Knowledge.</h1>
          <p className="muted-copy">
            The people, projects, and relationships that give Jossie better
            context.
          </p>
        </div>
        <div className="knowledge-stats">
          <span>{nodes.length} entities</span>
          <span>{types} types</span>
        </div>
      </header>
      {error && (
        <div className="toast-error" role="alert">
          {error}
          <button onClick={clearError}>×</button>
        </div>
      )}
      <div className="knowledge-canvas">
        <KnowledgeGraph apiConfig={api} />
      </div>
    </section>
  )
}
