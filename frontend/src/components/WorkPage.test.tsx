import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { WorkPage } from './WorkPage'

const getWork = vi.fn()

vi.mock('../api', () => ({
  buildWebSocketUrl: () => 'ws://example.test/api/events',
  getWork: (...args: unknown[]) => getWork(...args),
  getGoal: vi.fn(),
  getWorkRun: vi.fn(),
  updateGoal: vi.fn(),
  controlGoal: vi.fn(),
  cancelWorkRun: vi.fn(),
}))

class MockWebSocket {
  onmessage: ((event: MessageEvent) => void) | null = null
  close() {}
}

beforeEach(() => {
  vi.stubGlobal('WebSocket', MockWebSocket)
  getWork.mockResolvedValue({
    goals: [{
      id: 'goal-1', title: 'Prepare release', objective: 'Ship the release safely', status: 'active',
      created_at: '2026-08-07T12:00:00Z', updated_at: '2026-08-07T12:00:00Z',
      completed_tasks: 1, total_tasks: 2,
      tasks: [
        { id: 'task-1', goal_id: 'goal-1', position: 0, title: 'Validate build', status: 'completed', created_at: '2026-08-07T12:00:00Z', updated_at: '2026-08-07T12:00:00Z' },
        { id: 'task-2', goal_id: 'goal-1', position: 1, title: 'Publish release', status: 'in_progress', created_at: '2026-08-07T12:00:00Z', updated_at: '2026-08-07T12:00:00Z' },
      ],
    }],
    active_runs: [{
      id: 'run-1', kind: 'chat', status: 'running', summary: 'Prepare release', current_phase: 'Validating the build',
      goal_id: 'goal-1',
      visibility: 'significant', cancel_requested: false, created_at: '2026-08-07T12:00:00Z', updated_at: '2026-08-07T12:00:00Z',
    }],
    recent_runs: [],
    workers: [{ worker_key: 'heartbeat', label: 'Heartbeat checks', status: 'idle', detail: 'Ready', last_success_at: '2026-08-07T12:00:00Z', updated_at: '2026-08-07T12:00:00Z' }],
    scheduled_tasks: [],
    chat_imports: [],
  })
})

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
  vi.clearAllMocks()
})

describe('WorkPage', () => {
  it('shows current execution, outcome progress, and worker health', async () => {
    render(<WorkPage api={{ baseUrl: '', token: '' }} />)
    expect(await screen.findByText('Validating the build')).toBeTruthy()
    expect(screen.getByText('Prepare release')).toBeTruthy()
    expect(screen.getByText('1 of 2 complete')).toBeTruthy()
    expect(screen.getByText('Heartbeat checks')).toBeTruthy()
  })

  it('shows the next task for an open goal between execution runs', async () => {
    getWork.mockResolvedValueOnce({
      goals: [{
        id: 'goal-1', title: 'Prepare release', objective: 'Ship the release safely', status: 'active',
        created_at: '2026-08-07T12:00:00Z', updated_at: '2026-08-07T12:00:00Z',
        completed_tasks: 1, total_tasks: 2,
        tasks: [
          { id: 'task-1', goal_id: 'goal-1', position: 0, title: 'Validate build', status: 'completed', created_at: '2026-08-07T12:00:00Z', updated_at: '2026-08-07T12:00:00Z' },
          { id: 'task-2', goal_id: 'goal-1', position: 1, title: 'Publish release', status: 'in_progress', created_at: '2026-08-07T12:00:00Z', updated_at: '2026-08-07T12:00:00Z' },
        ],
      }],
      active_runs: [], recent_runs: [], scheduled_tasks: [], chat_imports: [],
      workers: [{ worker_key: 'heartbeat', label: 'Heartbeat checks', status: 'idle', detail: 'Ready', last_success_at: '2026-08-07T12:00:00Z', updated_at: '2026-08-07T12:00:00Z' }],
    })

    render(<WorkPage api={{ baseUrl: '', token: '' }} />)

    expect(await screen.findByText('Publish release')).toBeTruthy()
    expect(screen.getByText(/open goal · between runs/)).toBeTruthy()
    expect(screen.queryByText('Nothing is executing right now.')).toBeNull()
  })
})
