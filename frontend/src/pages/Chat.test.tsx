import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { Chat } from './Chat'
import * as apiModule from '../api'

vi.mock('../api', async (importOriginal) => {
  const original = await importOriginal<typeof import('../api')>()
  return {
    ...original,
    listConversations: vi.fn().mockResolvedValue([]),
  }
})

describe('Chat conversation workspace', () => {
  beforeEach(() => vi.mocked(apiModule.listConversations).mockClear())

  it('offers search and separate active and archived thread views', async () => {
    render(
      <Chat
        conversations={[]}
        onRefresh={vi.fn().mockResolvedValue(undefined)}
      />,
    )

    expect(
      screen.getByRole('button', { name: 'New conversation' }),
    ).toBeTruthy()
    expect(screen.getByLabelText('Search conversations')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Archived' }))

    await waitFor(() =>
      expect(apiModule.listConversations).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({ view: 'archived' }),
      ),
    )
  })
})
