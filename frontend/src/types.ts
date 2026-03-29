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

export type Message = {
  id: string
  role: 'user' | 'assistant' | 'tool' | 'system'
  content: string
  created_at: string
  name?: string | null
  tool_call_id?: string | null
  tool_calls?: ToolCall[] | null
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
