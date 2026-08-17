import { useEffect, useState } from 'react'
import type { FormEvent } from 'react'
import {
  addAccount,
  deleteAccount,
  listAccounts,
  listOnboarding,
  updateAccount,
} from '../api'
import { Empty, Panel } from '../components/Shared'
import { api } from '../config'
import type { Account, OnboardingStatus } from '../types'

type AccountForm = {
  integration: 'email' | 'google'
  name: string
  email: string
  username: string
  password: string
  imapHost: string
  imapPort: string
  smtpHost: string
  smtpPort: string
  refreshToken: string
}

const emptyAccountForm = (
  integration: AccountForm['integration'] = 'email',
): AccountForm => ({
  integration,
  name: '',
  email: '',
  username: '',
  password: '',
  imapHost: '',
  imapPort: '993',
  smtpHost: '',
  smtpPort: '587',
  refreshToken: '',
})

const accountValue = (account: Account, key: string) => {
  const details = account.details
  const value =
    details && typeof details === 'object' && !Array.isArray(details)
      ? details[key]
      : undefined
  return typeof value === 'string' || typeof value === 'number'
    ? String(value)
    : ''
}

export function Connections() {
  const [accounts, setAccounts] = useState<Account[]>([])
  const [onboarding, setOnboarding] = useState<OnboardingStatus[]>([])
  const [form, setForm] = useState<AccountForm>(emptyAccountForm())
  const [editingAccount, setEditingAccount] = useState<Account | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const refresh = () =>
    Promise.all([
      listAccounts(api).then(setAccounts),
      listOnboarding(api).then(setOnboarding),
    ])

  useEffect(() => {
    void refresh()
  }, [])

  const startNew = (integration: AccountForm['integration'] = 'email') => {
    setEditingAccount(null)
    setForm(emptyAccountForm(integration))
    setError(null)
  }

  const startEdit = (account: Account) => {
    const integration = account.integration as AccountForm['integration']
    setEditingAccount(account)
    setForm({
      integration,
      name: account.name,
      email: accountValue(account, 'email'),
      username: accountValue(account, 'username'),
      password: '',
      imapHost: accountValue(account, 'imap_host'),
      imapPort: accountValue(account, 'imap_port') || '993',
      smtpHost: accountValue(account, 'smtp_host'),
      smtpPort: accountValue(account, 'smtp_port') || '587',
      refreshToken: '',
    })
    setError(null)
  }

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    setSaving(true)
    setError(null)
    const config =
      form.integration === 'email'
        ? {
            username: form.username.trim(),
            password: form.password,
            imap_host: form.imapHost.trim(),
            imap_port: Number(form.imapPort),
            smtp_host: form.smtpHost.trim(),
            smtp_port: Number(form.smtpPort),
          }
        : { email: form.email.trim(), refresh_token: form.refreshToken }
    try {
      if (editingAccount) {
        await updateAccount(api, editingAccount.id, {
          name: form.name.trim(),
          config,
        })
      } else {
        await addAccount(api, {
          integration: form.integration,
          name: form.name.trim() || `${form.integration} account`,
          config,
        })
      }
      startNew()
      await refresh()
    } catch (reason) {
      setError(
        reason instanceof Error
          ? reason.message
          : 'Unable to save the account.',
      )
    } finally {
      setSaving(false)
    }
  }

  const field = <K extends keyof AccountForm>(key: K, value: AccountForm[K]) =>
    setForm((current) => ({ ...current, [key]: value }))
  const isEmail = form.integration === 'email'

  return (
    <section className="page">
      <header className="page-head">
        <div>
          <p className="eyebrow">CONNECTED SERVICES</p>
          <h1>Connections.</h1>
          <p className="muted-copy">
            Manage the services that let Jossie do useful work beyond the
            conversation.
          </p>
        </div>
        <button
          className="button primary"
          onClick={() => window.open('/setup/google', '_blank')}
        >
          Connect Google with OAuth
        </button>
      </header>
      <div className="connections-grid">
        <Panel title="Connection status">
          <div className="integration-list">
            {onboarding.map((item) => (
              <div key={item.name} className="integration-row">
                <span
                  className={
                    item.status === 'Configured' ? 'ready-dot' : 'idle-dot'
                  }
                />
                <div>
                  <strong>{item.name}</strong>
                  <small>
                    {item.status === 'Configured' ? 'Ready' : 'Setup required'}
                  </small>
                </div>
              </div>
            ))}
          </div>
        </Panel>
        <Panel title="Saved accounts">
          <div className="account-list">
            {accounts.length ? (
              accounts.map((account) => (
                <div className="account-row" key={account.id}>
                  <div>
                    <strong>{account.name}</strong>
                    <small>
                      {account.integration}
                      {accountValue(account, 'email')
                        ? ` · ${accountValue(account, 'email')}`
                        : ''}
                    </small>
                  </div>
                  <div className="account-actions">
                    <button
                      className="text-button"
                      onClick={() => startEdit(account)}
                    >
                      Edit
                    </button>
                    <button
                      className="text-button danger"
                      onClick={async () => {
                        await deleteAccount(api, account.id)
                        if (editingAccount?.id === account.id) startNew()
                        await refresh()
                      }}
                    >
                      Remove
                    </button>
                  </div>
                </div>
              ))
            ) : (
              <Empty copy="No accounts configured." />
            )}
          </div>
        </Panel>
        <Panel
          title={editingAccount ? `Edit ${editingAccount.name}` : 'Add account'}
          className="add-account"
        >
          <form
            className="connection-form typed-account-form"
            onSubmit={submit}
          >
            {!editingAccount && (
              <label>
                Account type
                <select
                  value={form.integration}
                  onChange={(e) =>
                    startNew(e.target.value as AccountForm['integration'])
                  }
                >
                  <option value="email">Email (IMAP / SMTP)</option>
                  <option value="google">Google (manual token)</option>
                </select>
              </label>
            )}
            <label>
              Display name
              <input
                required
                value={form.name}
                onChange={(e) => field('name', e.target.value)}
                placeholder={isEmail ? 'Work inbox' : 'Google account'}
              />
            </label>
            {isEmail ? (
              <>
                <label>
                  Email or username
                  <input
                    required
                    value={form.username}
                    onChange={(e) => field('username', e.target.value)}
                    placeholder="me@example.com"
                    autoComplete="username"
                  />
                </label>
                <label>
                  Password
                  <input
                    required={!editingAccount}
                    type="password"
                    value={form.password}
                    onChange={(e) => field('password', e.target.value)}
                    placeholder={
                      editingAccount
                        ? 'Leave blank to keep the current password'
                        : 'App password'
                    }
                    autoComplete="new-password"
                  />
                </label>
                <label>
                  IMAP host
                  <input
                    required
                    value={form.imapHost}
                    onChange={(e) => field('imapHost', e.target.value)}
                    placeholder="imap.example.com"
                  />
                </label>
                <label>
                  IMAP port
                  <input
                    required
                    inputMode="numeric"
                    value={form.imapPort}
                    onChange={(e) => field('imapPort', e.target.value)}
                  />
                </label>
                <label>
                  SMTP host
                  <input
                    required
                    value={form.smtpHost}
                    onChange={(e) => field('smtpHost', e.target.value)}
                    placeholder="smtp.example.com"
                  />
                </label>
                <label>
                  SMTP port
                  <input
                    required
                    inputMode="numeric"
                    value={form.smtpPort}
                    onChange={(e) => field('smtpPort', e.target.value)}
                  />
                </label>
              </>
            ) : (
              <>
                <label>
                  Email address <span className="optional-label">optional</span>
                  <input
                    type="email"
                    value={form.email}
                    onChange={(e) => field('email', e.target.value)}
                    placeholder="me@gmail.com"
                  />
                </label>
                <label className="form-span">
                  Refresh token
                  <input
                    required={!editingAccount}
                    type="password"
                    value={form.refreshToken}
                    onChange={(e) => field('refreshToken', e.target.value)}
                    placeholder={
                      editingAccount
                        ? 'Leave blank to keep the current token'
                        : 'Paste a Google refresh token'
                    }
                    autoComplete="new-password"
                  />
                </label>
                <p className="form-hint form-span">
                  Prefer the OAuth button above. Use a refresh token only for
                  manual setup.
                </p>
              </>
            )}
            {error && <p className="form-error form-span">{error}</p>}
            <div className="form-actions form-span">
              <button className="button secondary" disabled={saving}>
                {saving
                  ? 'Saving…'
                  : editingAccount
                    ? 'Save changes'
                    : 'Add account'}
              </button>
              {editingAccount && (
                <button
                  type="button"
                  className="text-button"
                  onClick={() => startNew()}
                >
                  Cancel
                </button>
              )}
            </div>
          </form>
        </Panel>
      </div>
    </section>
  )
}
