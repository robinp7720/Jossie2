import type { Account, Conversation, Message, OnboardingStatus } from './types'

export type ApiConfig = {
  baseUrl: string
  token: string
}

const stripTrailingSlash = (value: string) => value.replace(/\/+$/, '')

const buildUrl = (config: ApiConfig, path: string) => {
  const base = stripTrailingSlash(config.baseUrl)
  return `${base}${path}`
}

const buildHeaders = (config: ApiConfig) => {
  const headers = new Headers()
  if (config.token) {
    headers.set('Authorization', `Bearer ${config.token}`)
  }
  headers.set('Content-Type', 'application/json')
  return headers
}

export const request = async <T>(
  config: ApiConfig,
  path: string,
  options: RequestInit = {},
): Promise<T> => {
  const response = await fetch(buildUrl(config, path), {
    ...options,
    headers: {
      ...Object.fromEntries(buildHeaders(config).entries()),
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

export const getMessages = (config: ApiConfig, conversationId: string) =>
  request<Message[]>(config, `/api/conversations/${conversationId}/messages`)

export const sendMessage = (
  config: ApiConfig,
  message: string,
  conversationId?: string | null,
) =>
  request<{ conversation_id: string; message: string }>(config, '/api/chat', {
    method: 'POST',
    body: JSON.stringify({
      message,
      ...(conversationId ? { conversation_id: conversationId } : {}),
    }),
  })

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

export const buildWebSocketUrl = (config: ApiConfig, path: string) => {
  const base = stripTrailingSlash(config.baseUrl)
  const wsBase = base.startsWith('https://')
    ? base.replace('https://', 'wss://')
    : base.replace('http://', 'ws://')

  const token = config.token ? `?token=${encodeURIComponent(config.token)}` : ''
  return `${wsBase}${path}${token}`
}
