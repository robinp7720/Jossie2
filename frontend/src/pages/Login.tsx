import { useState } from 'react'
import type { FormEvent } from 'react'
import { login } from '../api'
import { api } from '../config'

export function Login({ onAuthenticated }: { onAuthenticated: () => void }) {
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    try {
      await login(api, password)
      onAuthenticated()
    } catch {
      setError('The password was not accepted.')
    } finally {
      setSubmitting(false)
    }
  }

  return <main className="login-page">
    <section className="login-panel">
      <div className="brand-lockup"><span className="brand-orb">J</span><span>Jossie</span></div>
      <p className="eyebrow">PRIVATE COMPANION</p>
      <h1>Your thinking space,<br />kept private.</h1>
      <p className="muted-copy">Sign in to continue to Jossie’s memories, knowledge, and current work.</p>
      <form onSubmit={submit} className="login-form">
        <label>Password<input autoFocus type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" /></label>
        {error && <p className="form-error" role="alert">{error}</p>}
        <button className="button primary full" disabled={submitting || !password}>{submitting ? 'Signing in…' : 'Enter Jossie'}</button>
      </form>
      <p className="login-note">This is a private, single-owner workspace.</p>
    </section>
    <div className="login-art" aria-hidden="true"><div className="art-ring ring-one" /><div className="art-ring ring-two" /><div className="art-core" /></div>
  </main>
}

