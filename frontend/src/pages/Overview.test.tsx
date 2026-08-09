import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { Overview } from './Overview'

afterEach(cleanup)

describe('Overview', () => {
  it('renders its loading state independently of the application shell', () => {
    render(<Overview dashboard={null} onNavigate={vi.fn()} />)
    expect(screen.getByText('YOUR PRIVATE COMPANION')).toBeTruthy()
    expect(document.querySelector('.loading-lines')).toBeTruthy()
  })
})
