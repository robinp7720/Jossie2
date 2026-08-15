import type {
  AccountConfig,
  ActivityEvent,
  Attachment,
  ChatImport,
  Conversation as BaseConversation,
  ConversationListItem,
  DashboardResponse,
  GoalDetail,
  GoalTask,
  GoalWithTasks,
  GraphEdge,
  GraphNode,
  IntegrationStatus,
  MemoryEntryWithMetadata,
  Message as GeneratedMessage,
  PendingAction,
  ScheduledTask,
  ServerEvent,
  ToolCall,
  WorkerStatus,
  WorkRun,
  WorkRunDetail,
  WorkRunStep,
  WorkSummary,
} from './generated/contracts'

export type {
  ActivityEvent,
  ChatImport,
  GoalDetail,
  GoalTask,
  GraphEdge,
  GraphNode,
  PendingAction,
  ScheduledTask,
  ServerEvent,
  ToolCall,
  WorkerStatus,
  WorkRun,
  WorkRunDetail,
  WorkRunStep,
  WorkSummary,
}

export type Conversation = BaseConversation & Partial<Pick<ConversationListItem,
  'preview' | 'matched_message_id' | 'message_count'>>

export type FileAttachment = Attachment

// The UI also constructs optimistic messages before the server has assigned all
// persistence-only fields, so those fields stay optional at this boundary.
export type Message = Omit<GeneratedMessage,
  'conversation_id' | 'tool_calls' | 'tool_call_id' | 'name' | 'attachments'> & {
  conversation_id?: string
  tool_calls?: ToolCall[] | null
  tool_call_id?: string | null
  name?: string | null
  attachments?: FileAttachment[] | null
}

export type OnboardingStatus = IntegrationStatus
export type Account = AccountConfig
export type GraphResponse = { nodes: GraphNode[]; edges: GraphEdge[] }
export type Memory = MemoryEntryWithMetadata
export type Goal = GoalWithTasks
export type Dashboard = DashboardResponse
