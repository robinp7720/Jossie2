import type { Account, ActivityEvent, ChatImport, Conversation, Dashboard, Goal, GoalDetail, Memory, Message, OnboardingStatus, PendingAction, WorkRun, WorkRunDetail, WorkSummary } from './types'

export type ApiConfig = {
  baseUrl: string
  token: string
}

const stripTrailingSlash = (value: string) => value.replace(/\/+$/, '')

const buildUrl = (config: ApiConfig, path: string) => {
  const base = stripTrailingSlash(config.baseUrl)
  return `${base}${path}`
}

const buildHeaders = (config: ApiConfig, options: RequestInit = {}) => {
  const headers = new Headers()
  if (config.token) {
    headers.set('Authorization', `Bearer ${config.token}`)
  }

  // If the body is NOT FormData, default to application/json
  if (!(options.body instanceof FormData)) {
    headers.set('Content-Type', 'application/json')
  }

  return headers
}

export const request = async <T>(
  config: ApiConfig,
  path: string,
  options: RequestInit = {},
): Promise<T> => {
  const response = await fetch(buildUrl(config, path), {
    ...options,
    credentials: 'include',
    headers: {
      ...Object.fromEntries(buildHeaders(config, options).entries()),
      ...(options.headers ?? {}),
    },
  })

  const contentType = response.headers.get('content-type') ?? ''
  const text = await response.text()

  if (!response.ok) {
    let message = (() => {
      if (!text) return `Request failed with status ${response.status}`
      try {
        const error = JSON.parse(text) as { error?: unknown }
        return typeof error.error === 'string' ? error.error : text
      } catch {
        return text
      }
    })()
    throw new Error(message)
  }

  if (!text) {
    return undefined as T
  }

  const isJson =
    contentType.includes('application/json') ||
    text.trim().startsWith('{') ||
    text.trim().startsWith('[')

  if (!isJson) {
    const snippet = text.trim().slice(0, 200)
    throw new Error(
      `Non-JSON response. Check the API base URL. Received: ${snippet || 'empty body'}`,
    )
  }

  return JSON.parse(text) as T
}

export const listConversations = (
  config: ApiConfig,
  filters: { view?: 'active' | 'archived' | 'all'; q?: string; limit?: number; before?: string } = {},
) => {
  const query = new URLSearchParams()
  if (filters.view) query.set('view', filters.view)
  if (filters.q) query.set('q', filters.q)
  if (filters.limit) query.set('limit', String(filters.limit))
  if (filters.before) query.set('before', filters.before)
  return request<Conversation[]>(config, `/api/conversations${query.size ? `?${query}` : ''}`)
}

export const createConversation = (config: ApiConfig) =>
  request<Conversation>(config, '/api/conversations', { method: 'POST' })

export const updateConversation = (
  config: ApiConfig,
  conversationId: string,
  payload: { title?: string; archived?: boolean },
) => request<Conversation>(config, `/api/conversations/${encodeURIComponent(conversationId)}`, {
  method: 'PATCH', body: JSON.stringify(payload),
})

export const deleteConversation = (config: ApiConfig, conversationId: string) =>
  request<{ conversation_id: string; deleted: boolean; deleted_files: number }>(
    config,
    `/api/conversations/${encodeURIComponent(conversationId)}`,
    { method: 'DELETE' },
  )

export const getMessages = (
  config: ApiConfig,
  conversationId: string,
  limit = 100,
  cursor: { before?: string; around?: string } = {},
) => {
  const query = new URLSearchParams({ limit: String(limit) })
  if (cursor.before) query.set('before', cursor.before)
  if (cursor.around) query.set('around', cursor.around)
  return request<Message[]>(config, `/api/conversations/${conversationId}/messages?${query}`)
}

export const downloadConversationExport = async (
  config: ApiConfig,
  conversationId: string,
  format: 'markdown' | 'json',
) => {
  const response = await fetch(buildUrl(config, `/api/conversations/${encodeURIComponent(conversationId)}/export?format=${format}`), {
    credentials: 'include', headers: buildHeaders(config),
  })
  if (!response.ok) throw new Error(await response.text() || `Export failed with status ${response.status}`)
  const disposition = response.headers.get('content-disposition') ?? ''
  const filename = disposition.match(/filename="([^"]+)"/)?.[1] ?? `conversation.${format === 'markdown' ? 'md' : 'json'}`
  const url = URL.createObjectURL(await response.blob())
  const anchor = document.createElement('a')
  anchor.href = url; anchor.download = filename; anchor.click()
  window.setTimeout(() => URL.revokeObjectURL(url), 0)
}

export const sendMessage = (
  config: ApiConfig,
  message: string,
  conversationId?: string | null,
  fileIds?: string[],
) =>
  request<{ conversation_id: string; message: string }>(config, '/api/chat', {
    method: 'POST',
    body: JSON.stringify({
      message,
      ...(conversationId ? { conversation_id: conversationId } : {}),
      ...(fileIds ? { file_ids: fileIds } : {}),
    }),
  })

export const uploadFile = (config: ApiConfig, file: File) => {
  const formData = new FormData()
  formData.append('file', file)

  return request<{ file_id: string; name: string }>(config, '/api/files', {
    method: 'POST',
    body: formData,
  })
}

export const deleteUploadedFile = (config: ApiConfig, fileId: string) =>
  request<void>(config, `/api/files/${encodeURIComponent(fileId)}`, { method: 'DELETE' })

export const downloadUploadedFile = async (config: ApiConfig, fileId: string, fallbackName: string) => {
  const response = await fetch(buildUrl(config, `/api/files/${encodeURIComponent(fileId)}`), {
    credentials: 'include', headers: buildHeaders(config),
  })
  if (!response.ok) throw new Error(await response.text() || `Download failed with status ${response.status}`)
  const disposition = response.headers.get('content-disposition') ?? ''
  const filename = disposition.match(/filename="([^"]+)"/)?.[1] ?? fallbackName
  const url = URL.createObjectURL(await response.blob())
  const anchor = document.createElement('a')
  anchor.href = url; anchor.download = filename; anchor.click()
  window.setTimeout(() => URL.revokeObjectURL(url), 0)
}

export const startChatImport = (
  config: ApiConfig,
  fileId: string,
  format: ChatImport['format'] = 'auto',
) => request<ChatImport>(config, '/api/chat-imports', {
  method: 'POST',
  body: JSON.stringify({ file_id: fileId, format }),
})

export const getChatImport = (config: ApiConfig, importId: string) =>
  request<ChatImport>(config, `/api/chat-imports/${encodeURIComponent(importId)}`)

export const listOnboarding = (config: ApiConfig) =>
  request<OnboardingStatus[]>(config, '/api/onboarding')

export const listAccounts = (config: ApiConfig) =>
  request<Account[]>(config, '/api/config/accounts')

export const addAccount = (
  config: ApiConfig,
  payload: {
    integration: string
    name: string
    config: Record<string, unknown>
  },
) =>
  request<string>(config, '/api/config/accounts', {
    method: 'POST',
    body: JSON.stringify(payload),
  })

export const deleteAccount = (config: ApiConfig, accountId: string) =>
  request<void>(config, `/api/config/accounts/${accountId}`, {
    method: 'DELETE',
  })

export const updateAccount = (
  config: ApiConfig,
  accountId: string,
  payload: { name: string; config: Record<string, unknown> },
) =>
  request<void>(config, `/api/config/accounts/${accountId}`, {
    method: 'PATCH',
    body: JSON.stringify(payload),
  })

export const cancelConversation = (config: ApiConfig, conversationId: string) =>
  request<{ conversation_id: string; status: string }>(
    config,
    `/api/conversations/${conversationId}/cancel`,
    {
      method: 'POST',
    },
  )

export const buildWebSocketUrl = (config: ApiConfig, path: string) => {
  const base = stripTrailingSlash(config.baseUrl || window.location.origin)
  const wsBase = base.startsWith('https://')
    ? base.replace('https://', 'wss://')
    : base.replace('http://', 'ws://')

  const token = config.token
    ? `${path.includes('?') ? '&' : '?'}token=${encodeURIComponent(config.token)}`
    : ''
  return `${wsBase}${path}${token}`
}

export const fetchGraph = (config: ApiConfig, limit = 500) =>
  request<{ nodes: import('./types').GraphNode[]; edges: import('./types').GraphEdge[] }>(
    config,
    `/api/graph?limit=${limit}`,
  )

export const getSession = (config: ApiConfig) =>
  request<{ authenticated: boolean }>(config, '/api/auth/session')

export const login = (config: ApiConfig, password: string) =>
  request<{ authenticated: boolean }>(config, '/api/auth/login', {
    method: 'POST',
    body: JSON.stringify({ password }),
  })

export const logout = (config: ApiConfig) =>
  request<{ authenticated: boolean }>(config, '/api/auth/logout', { method: 'POST' })

export const getDashboard = (config: ApiConfig) =>
  request<Dashboard>(config, '/api/dashboard')

export const listMemories = (config: ApiConfig, query = '', scope = 'all', limit = 50) =>
  request<Memory[]>(
    config,
    `/api/memories?query=${encodeURIComponent(query)}&scope=${encodeURIComponent(scope)}&limit=${limit}`,
  )

export const listActivity = (config: ApiConfig, before?: string, limit = 30) =>
  request<{ items: ActivityEvent[]; next_cursor: string | null }>(
    config,
    `/api/activity?limit=${limit}${before ? `&before=${encodeURIComponent(before)}` : ''}`,
  )

export const listPendingActions = (config: ApiConfig, conversationId?: string) =>
  request<PendingAction[]>(
    config,
    `/api/actions/pending${conversationId ? `?conversation_id=${encodeURIComponent(conversationId)}` : ''}`,
  )

export const approveAction = (config: ApiConfig, actionId: string) =>
  request<{ action_id: string; status: string }>(config, `/api/actions/${actionId}/approve`, {
    method: 'POST',
  })

export const rejectAction = (config: ApiConfig, actionId: string) =>
  request<{ action_id: string; status: string }>(config, `/api/actions/${actionId}/reject`, {
    method: 'POST',
  })

export const getWork = (config: ApiConfig, conversationId?: string, includeQuiet = false) =>
  request<WorkSummary>(config, `/api/work?include_quiet=${includeQuiet}${conversationId ? `&conversation_id=${encodeURIComponent(conversationId)}` : ''}`)

export const getGoal = (config: ApiConfig, goalId: string) =>
  request<GoalDetail>(config, `/api/goals/${encodeURIComponent(goalId)}`)

export const updateGoal = (config: ApiConfig, goalId: string, payload: { title?: string; archived?: boolean }) =>
  request<Goal>(config, `/api/goals/${encodeURIComponent(goalId)}`, { method: 'PATCH', body: JSON.stringify(payload) })

export const controlGoal = (config: ApiConfig, goalId: string, action: 'pause' | 'resume' | 'cancel') =>
  request<Goal>(config, `/api/goals/${encodeURIComponent(goalId)}/${action}`, { method: 'POST' })

export const getWorkRun = (config: ApiConfig, runId: string) =>
  request<WorkRunDetail>(config, `/api/work/runs/${encodeURIComponent(runId)}`)

export const listWorkRuns = (config: ApiConfig, filters: { before?: string; kind?: string; status?: string; includeQuiet?: boolean } = {}) => {
  const query = new URLSearchParams()
  if (filters.before) query.set('before', filters.before)
  if (filters.kind) query.set('kind', filters.kind)
  if (filters.status) query.set('status', filters.status)
  if (filters.includeQuiet) query.set('include_quiet', 'true')
  return request<{ items: WorkRun[]; next_cursor: string | null }>(config, `/api/work/runs?${query.toString()}`)
}

export const cancelWorkRun = (config: ApiConfig, runId: string) =>
  request<WorkRun>(config, `/api/work/runs/${encodeURIComponent(runId)}/cancel`, { method: 'POST' })
