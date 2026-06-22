import type { Account, ActivityEvent, Conversation, Dashboard, Memory, Message, OnboardingStatus } from './types'

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
    throw new Error(text || `Request failed with status ${response.status}`)
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

export const listConversations = (config: ApiConfig) =>
  request<Conversation[]>(config, '/api/conversations')

export const getMessages = (config: ApiConfig, conversationId: string, limit?: number) =>
  request<Message[]>(
    config,
    `/api/conversations/${conversationId}/messages${limit ? `?limit=${limit}` : ''}`,
  )

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

  const token = config.token ? `?token=${encodeURIComponent(config.token)}` : ''
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
