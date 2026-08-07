export type Conversation = {
  id: string
  title: string | null
  created_at: string
  updated_at: string
}

export type ToolCall = {
  id: string
  name: string
  arguments: string
}

export type FileAttachment = {
  id: string
  name: string
  mime_type?: string
  size: number
}

export type Message = {
  id: string
  role: 'user' | 'assistant' | 'tool' | 'system'
  content: string
  created_at: string
  name?: string | null
  tool_call_id?: string | null
  tool_calls?: ToolCall[] | null
  attachments?: FileAttachment[] | null
}

export type OnboardingField = {
  name: string
  label?: string
  type?: string
  required?: boolean
  placeholder?: string
}

export type OnboardingStatus = {
  name: string
  status: string
  details?: {
    fields?: OnboardingField[]
  }
}

export type Account = {
  id: string
  integration: string
  name: string
  details?: Record<string, unknown>
}

export type GraphNode = {
  id: string
  label: string
  node_type: string
  properties: Record<string, unknown>
}

export type GraphEdge = {
  id: string
  source_id: string
  target_id: string
  relation: string
  weight: number
  properties: Record<string, unknown>
}

export type GraphResponse = {
  nodes: GraphNode[]
  edges: GraphEdge[]
}

export type Memory = {
  key: string
  content: string
  tags: string
  created_at: string
  updated_at: string
  prompt_scope: string
  importance: number
}

export type ChatImport = {
  id: string
  file_id: string
  format: 'auto' | 'whatsapp' | 'signal' | 'chatgpt' | 'generic'
  status: 'queued' | 'processing' | 'completed' | 'failed'
  total_messages: number
  analyzed_messages: number
  memories_saved: number
  nodes_saved: number
  edges_saved: number
  error?: string | null
  created_at: string
  updated_at: string
}

export type ActivityEvent = {
  id: string
  conversation_id: string | null
  run_id: string | null
  category: string
  title: string
  detail: string | null
  tone: 'normal' | 'success' | 'warn' | string
  created_at: string
}

export type PendingAction = {
  id: string
  batch_id: string
  conversation_id: string
  run_id: string
  call_id: string
  tool_name: string
  title: string
  summary: string
  effect: 'external_write' | 'destructive' | string
  status: 'pending' | 'executing' | 'uncertain' | string
  result_error?: string | null
  created_at: string
  updated_at: string
  resolved_at?: string | null
}

export type ScheduledTask = {
  id: string
  conversation_id: string
  task_type: string
  schedule_type: string
  schedule_value: string
  status: string
  next_run_at: string | null
  last_run_at?: string | null
  run_count?: number
  max_runs?: number | null
  last_error?: string | null
}

export type GoalTask = {
  id: string
  goal_id: string
  position: number
  title: string
  status: 'pending' | 'in_progress' | 'waiting' | 'blocked' | 'completed' | 'failed' | 'cancelled' | string
  summary?: string | null
  blocker?: string | null
  source_type?: string | null
  source_id?: string | null
  created_at: string
  updated_at: string
}

export type Goal = {
  id: string
  conversation_id?: string | null
  title: string
  objective: string
  status: 'active' | 'paused' | 'blocked' | 'completed' | 'cancelled' | string
  blocker?: string | null
  archived_at?: string | null
  created_at: string
  updated_at: string
  tasks: GoalTask[]
  completed_tasks: number
  total_tasks: number
}

export type WorkRun = {
  id: string
  goal_id?: string | null
  task_id?: string | null
  conversation_id?: string | null
  kind: string
  source_type?: string | null
  source_id?: string | null
  status: 'queued' | 'running' | 'waiting_for_approval' | 'completed' | 'failed' | 'cancelled' | 'interrupted' | string
  summary: string
  current_phase?: string | null
  error?: string | null
  visibility: string
  cancel_requested: boolean
  started_at?: string | null
  finished_at?: string | null
  created_at: string
  updated_at: string
}

export type WorkRunStep = {
  id: string
  run_id: string
  sequence: number
  kind: string
  label: string
  status: string
  summary?: string | null
  error?: string | null
  started_at: string
  finished_at?: string | null
}

export type WorkerStatus = {
  worker_key: string
  label: string
  status: 'idle' | 'running' | 'degraded' | 'disabled' | string
  current_run_id?: string | null
  detail?: string | null
  last_started_at?: string | null
  last_success_at?: string | null
  last_error_at?: string | null
  last_error?: string | null
  updated_at: string
}

export type WorkSummary = {
  goals: Goal[]
  active_runs: WorkRun[]
  recent_runs: WorkRun[]
  workers: WorkerStatus[]
  scheduled_tasks: ScheduledTask[]
  chat_imports: ChatImport[]
}

export type GoalDetail = Goal & { runs: WorkRun[] }
export type WorkRunDetail = WorkRun & { steps: WorkRunStep[] }

export type Dashboard = {
  stats: {
    memories: number
    prompt_ready_memories: number
    knowledge_nodes: number
    knowledge_edges: number
    pending_tasks: number
    active_goals: number
    active_runs: number
    waiting_work: number
    blocked_goals: number
  }
  recent_memories: Memory[]
  recent_activity: ActivityEvent[]
  recent_conversations: Conversation[]
  upcoming_tasks: ScheduledTask[]
  graph_highlights: Array<{ node: GraphNode; connections: number }>
}
