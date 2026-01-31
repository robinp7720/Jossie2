export type Conversation = {
  id: string
  title: string
  created_at: string
  updated_at: string
}

export type Message = {
  id: string
  role: 'user' | 'assistant' | 'tool' | 'system'
  content: string
  created_at: string
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
