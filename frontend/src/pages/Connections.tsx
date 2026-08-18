import { useEffect, useMemo, useState } from 'react'
import type { FormEvent } from 'react'
import { addAccount, deleteAccount, listAccounts, listIntegrationTypes, listOnboarding, updateAccount } from '../api'
import { Empty, Panel } from '../components/Shared'
import { api } from '../config'
import type { Account, ConnectionSpec, OnboardingStatus } from '../types'

const detailsOf = (account: Account) =>
  account.details && typeof account.details === 'object' && !Array.isArray(account.details)
    ? (account.details as Record<string, unknown>)
    : {}

export function Connections() {
  const [accounts, setAccounts] = useState<Account[]>([])
  const [onboarding, setOnboarding] = useState<OnboardingStatus[]>([])
  const [specs, setSpecs] = useState<ConnectionSpec[]>([])
  const [integration, setIntegration] = useState('email')
  const [name, setName] = useState('')
  const [values, setValues] = useState<Record<string, string>>({})
  const [editing, setEditing] = useState<Account | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const spec = useMemo(() => specs.find((item) => item.integration === integration), [specs, integration])

  const refresh = () => Promise.all([
    listAccounts(api).then(setAccounts),
    listOnboarding(api).then(setOnboarding),
    listIntegrationTypes(api).then((items) => {
      setSpecs(items)
      if (items.length && !items.some((item) => item.integration === integration)) setIntegration(items[0].integration)
    }),
  ])
  useEffect(() => { void refresh() }, [])

  const reset = (next = specs[0]?.integration ?? 'email') => {
    setEditing(null); setIntegration(next); setName(''); setError(null)
    const nextSpec = specs.find((item) => item.integration === next)
    setValues(Object.fromEntries((nextSpec?.fields ?? []).map((field) => [field.name, field.default_value ?? ''])))
  }
  const startEdit = (account: Account) => {
    setEditing(account); setIntegration(account.integration); setName(account.name); setError(null)
    const details = detailsOf(account)
    const accountSpec = specs.find((item) => item.integration === account.integration)
    setValues(Object.fromEntries((accountSpec?.fields ?? []).map((field) => {
      const value = details[field.name]
      return [field.name, field.secret || value === '[REDACTED]' ? '' : value == null ? field.default_value ?? '' : String(value)]
    })))
  }
  const submit = async (event: FormEvent) => {
    event.preventDefault(); if (!spec) return; setSaving(true); setError(null)
    const config = Object.fromEntries(spec.fields.map((field) => [field.name, field.input_type === 'number' ? Number(values[field.name]) : values[field.name] ?? '']))
    try {
      if (editing) await updateAccount(api, editing.id, { name: name.trim(), config })
      else await addAccount(api, { integration, name: name.trim() || spec.display_name, config })
      reset(); await refresh()
    } catch (reason) { setError(reason instanceof Error ? reason.message : 'Unable to save the account.') }
    finally { setSaving(false) }
  }

  return <section className="page">
    <header className="page-head"><div><p className="eyebrow">CONNECTED SERVICES</p><h1>Connections.</h1><p className="muted-copy">Manage the services that let Jossie do useful work beyond the conversation.</p></div>
      {spec?.oauth_available && <button className="button primary" onClick={() => window.open(`/setup/${encodeURIComponent(spec.integration)}`, '_blank')}>Connect {spec.display_name} with OAuth</button>}
    </header>
    <div className="connections-grid">
      <Panel title="Connection status"><div className="integration-list">{onboarding.map((item) => <div key={item.name} className="integration-row"><span className={item.status === 'Configured' ? 'ready-dot' : 'idle-dot'} /><div><strong>{item.name}</strong><small>{item.status === 'Configured' ? 'Ready' : 'Setup required'}</small></div></div>)}</div></Panel>
      <Panel title="Saved accounts"><div className="account-list">{accounts.length ? accounts.map((account) => <div className="account-row" key={account.id}><div><strong>{account.name}</strong><small>{account.integration}</small></div><div className="account-actions"><button className="text-button" onClick={() => startEdit(account)}>Edit</button><button className="text-button danger" onClick={async () => { await deleteAccount(api, account.id); if (editing?.id === account.id) reset(); await refresh() }}>Remove</button></div></div>) : <Empty copy="No accounts configured." />}</div></Panel>
      <Panel title={editing ? `Edit ${editing.name}` : 'Add account'} className="add-account">
        <form className="connection-form typed-account-form" onSubmit={submit}>
          {!editing && <label>Account type<select value={integration} onChange={(event) => reset(event.target.value)}>{specs.map((item) => <option key={item.integration} value={item.integration}>{item.display_name}</option>)}</select></label>}
          <label>Display name<input required value={name} onChange={(event) => setName(event.target.value)} placeholder={spec?.display_name ?? 'Account'} /></label>
          {spec?.fields.map((field) => <label key={field.name} className={spec.fields.length === 1 ? 'form-span' : undefined}>{field.label}{!field.required && <span className="optional-label"> optional</span>}<input type={field.input_type} required={field.required && (!editing || !field.secret)} value={values[field.name] ?? ''} onChange={(event) => setValues((current) => ({ ...current, [field.name]: event.target.value }))} placeholder={editing && field.secret ? 'Leave blank to keep the current value' : field.description ?? ''} autoComplete={field.secret ? 'new-password' : undefined} />{field.description && <small>{field.description}</small>}</label>)}
          {spec?.oauth_available && !editing && <p className="form-hint form-span">{spec.fields.length ? 'OAuth is preferred. Manual credentials are available for self-hosted or development setups.' : 'Use the OAuth button above to add this provider.'}</p>}
          {error && <p className="form-error form-span">{error}</p>}
          <div className="form-actions form-span"><button className="button secondary" disabled={saving || (!editing && (spec?.fields.length ?? 0) === 0)}>{saving ? 'Saving…' : editing ? 'Save changes' : 'Add account'}</button>{editing && <button type="button" className="text-button" onClick={() => reset()}>Cancel</button>}</div>
        </form>
      </Panel>
    </div>
  </section>
}
