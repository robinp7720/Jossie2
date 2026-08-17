import type { Dashboard } from '../types'
import type { Page } from '../navigation'
import { formatDate, initials } from '../utils/format'
import { ActivityList, Empty, PageLoading, Panel } from '../components/Shared'
import { MemoryCard } from './Memories'

export function Overview({
  dashboard,
  onNavigate,
}: {
  dashboard: Dashboard | null
  onNavigate: (page: Page) => void
}) {
  const hour = new Date().getHours()
  const greeting =
    hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening'
  if (!dashboard) return <PageLoading title={`${greeting}.`} />
  return (
    <section className="page overview-page">
      <header className="page-head overview-head">
        <div>
          <p className="eyebrow">YOUR PRIVATE COMPANION</p>
          <h1>{greeting}.</h1>
          <p className="muted-copy">
            Here’s the shape of your world in Jossie right now.
          </p>
        </div>
        <button className="button primary" onClick={() => onNavigate('chat')}>
          Ask Jossie <span>→</span>
        </button>
      </header>
      <section className="metric-grid">
        <Metric
          label="Memories"
          value={dashboard.stats.memories}
          detail={`${dashboard.stats.prompt_ready_memories} in active context`}
          mark="◫"
        />
        <Metric
          label="Knowledge"
          value={dashboard.stats.knowledge_nodes}
          detail={`${dashboard.stats.knowledge_edges} relationships`}
          mark="⌘"
        />
        <Metric
          label="Current work"
          value={dashboard.stats.active_goals + dashboard.stats.active_runs}
          detail={`${dashboard.stats.active_runs} running · ${dashboard.stats.waiting_work + dashboard.stats.blocked_goals} need attention`}
          mark="◌"
        />
        <Metric
          label="Recent activity"
          value={dashboard.recent_activity.length}
          detail="latest moments"
          mark="↗"
        />
      </section>
      <div className="overview-grid">
        <Panel
          title="What Jossie remembers"
          action="Browse memories"
          onAction={() => onNavigate('memories')}
          className="wide-panel"
        >
          <div className="memory-preview-grid">
            {dashboard.recent_memories.length ? (
              dashboard.recent_memories.map((memory) => (
                <MemoryCard key={memory.key} memory={memory} compact />
              ))
            ) : (
              <Empty copy="No memories yet. The details that matter will begin to appear here." />
            )}
          </div>
        </Panel>
        <Panel
          title="Recent activity"
          action="View timeline"
          onAction={() => onNavigate('activity')}
        >
          <ActivityList events={dashboard.recent_activity} />
        </Panel>
        <Panel
          title="Knowledge highlights"
          action="Open knowledge"
          onAction={() => onNavigate('knowledge')}
        >
          <div className="highlight-list">
            {dashboard.graph_highlights.length ? (
              dashboard.graph_highlights.map(({ node, connections }) => (
                <div className="highlight-row" key={node.id}>
                  <span className="node-avatar">{initials(node.label)}</span>
                  <div>
                    <strong>{node.label}</strong>
                    <small>{node.node_type}</small>
                  </div>
                  <span>{connections}</span>
                </div>
              ))
            ) : (
              <Empty copy="The knowledge graph will grow as Jossie learns durable relationships." />
            )}
          </div>
        </Panel>
        <Panel title="Coming up">
          <div className="task-list">
            {dashboard.upcoming_tasks.length ? (
              dashboard.upcoming_tasks.map((task) => (
                <div className="task-row" key={task.id}>
                  <span className="task-dot" />
                  <div>
                    <strong>{task.task_type.replace(/_/g, ' ')}</strong>
                    <small>
                      {task.schedule_type} · {formatDate(task.next_run_at)}
                    </small>
                  </div>
                </div>
              ))
            ) : (
              <Empty copy="No scheduled work is waiting." />
            )}
          </div>
        </Panel>
      </div>
    </section>
  )
}

function Metric({
  label,
  value,
  detail,
  mark,
}: {
  label: string
  value: number
  detail: string
  mark: string
}) {
  return (
    <article className="metric-card">
      <span className="metric-mark">{mark}</span>
      <p>{label}</p>
      <strong>{value}</strong>
      <small>{detail}</small>
    </article>
  )
}
