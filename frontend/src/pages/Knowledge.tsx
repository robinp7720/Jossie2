import { KnowledgeGraph } from '../components/KnowledgeGraph'

export function Knowledge() {
  const [nodes, setNodes] = useState<GraphNode[]>([])
  useEffect(() => { void fetchGraph(api, 500).then((data) => setNodes(data.nodes)) }, [])
  const types = useMemo(() => new Set(nodes.map((node) => node.node_type)).size, [nodes])
  return <section className="page knowledge-page"><header className="page-head"><div><p className="eyebrow">CONNECTED CONTEXT</p><h1>Knowledge.</h1><p className="muted-copy">The people, projects, and relationships that give Jossie better context.</p></div><div className="knowledge-stats"><span>{nodes.length} entities</span><span>{types} types</span></div></header><div className="knowledge-canvas"><KnowledgeGraph apiConfig={api} /></div></section>
}

