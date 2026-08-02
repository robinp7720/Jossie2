import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { AgentRunStatus } from './AgentRunStatus'
import type { PendingAction } from '../types'

const action: PendingAction = {
  id: 'action-1', batch_id: 'batch-1', conversation_id: 'conversation-1', run_id: 'run-1',
  call_id: 'call-1', tool_name: 'mail_send', title: 'Send email',
  summary: 'To ada@example.com — Status update\nThe migration is complete.',
  effect: 'external_write', status: 'pending', created_at: '2026-08-02T12:00:00Z',
  updated_at: '2026-08-02T12:00:00Z',
}

afterEach(cleanup)

describe('AgentRunStatus', () => {
  it('shows safe action details and dispatches an approval', () => {
    const decide = vi.fn()
    render(<AgentRunStatus steps={[]} actions={[action]} onDecision={decide} />)
    expect(screen.getByText('ada@example.com', { exact: false })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Approve' }))
    expect(decide).toHaveBeenCalledWith(action, true)
  })

  it('renders progress and uncertain outcomes without decision buttons', () => {
    render(<AgentRunStatus
      steps={[{ id: 'one', label: 'Using mail search', status: 'done' }]}
      actions={[{ ...action, status: 'uncertain', result_error: 'Outcome unknown' }]}
      onDecision={() => undefined}
    />)
    expect(screen.getByText('Using mail search')).toBeTruthy()
    expect(screen.getByText('Verify this action manually before retrying.')).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Approve' })).toBeNull()
  })
})
