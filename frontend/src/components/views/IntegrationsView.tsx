import React, { useState, useEffect, useMemo } from 'react';
import { useAppContext } from '../../context/AppContext';
import { Card, Button, Chip } from '../common/UI';
import { addAccount } from '../../api';

export const IntegrationsView: React.FC = () => {
  const {
    apiConfig,
    onboarding,
    refreshOnboarding,
    refreshAccounts,
    canConnect,
    setStatusMessage,
  } = useAppContext();

  const [accountForm, setAccountForm] = useState({
    integration: 'email',
    name: '',
    username: '',
    password: '',
    imap_host: '',
    imap_port: '993',
    smtp_host: '',
    smtp_port: '587',
    refresh_token: '',
    google_email: '',
    customJson: `{\n  "key": "value"\n}`,
  });

  useEffect(() => {
    refreshOnboarding();
  }, [refreshOnboarding]);

  const googleOauthUrl = useMemo(() => {
    const base = apiConfig.baseUrl.replace(/\/+$/, '');
    if (!apiConfig.token) return `${base}/setup/google`;
    const encoded = encodeURIComponent(apiConfig.token);
    return `${base}/setup/google?token=${encoded}`;
  }, [apiConfig.baseUrl, apiConfig.token]);

  const handleGoogleConnect = (accountName?: string) => {
    const url = new URL(googleOauthUrl);
    const trimmed = accountName?.trim();
    if (trimmed) url.searchParams.set('account_name', trimmed);
    window.open(url.toString(), '_blank');
  };

  const handleAccountSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!canConnect) return;

    try {
      if (accountForm.integration === 'google' && !accountForm.refresh_token.trim()) {
        setStatusMessage('Google refresh token is required for manual setup.');
        return;
      }

      let payload: { integration: string; name: string; config: Record<string, unknown> };

      if (accountForm.integration === 'email') {
        payload = {
          integration: 'email',
          name: accountForm.name || 'Email Account',
          config: {
            username: accountForm.username,
            password: accountForm.password,
            imap_host: accountForm.imap_host,
            imap_port: Number(accountForm.imap_port),
            smtp_host: accountForm.smtp_host,
            smtp_port: Number(accountForm.smtp_port),
          },
        };
      } else if (accountForm.integration === 'google') {
        payload = {
          integration: 'google',
          name: accountForm.name || 'Google Account',
          config: {
            refresh_token: accountForm.refresh_token,
            email: accountForm.google_email,
          },
        };
      } else {
        payload = {
          integration: accountForm.integration,
          name: accountForm.name || 'Custom Account',
          config: JSON.parse(accountForm.customJson || '{}'),
        };
      }

      await addAccount(apiConfig, payload);
      setStatusMessage('Account added.');
      refreshAccounts();
    } catch (error) {
      setStatusMessage(error instanceof Error ? error.message : String(error));
    }
  };

  const readyCount = onboarding.filter((integration) => integration.status === 'ready').length;

  return (
    <section className="view integrations-view">
      <header className="hero">
        <div className="hero-copy">
          <p className="eyebrow">Integrations</p>
          <h1>Control external systems from one place.</h1>
          <p className="hero-text">
            Review onboarding state, launch OAuth, and add server-stored accounts
            without dropping out of the workspace.
          </p>
        </div>
        <div className="hero-rail">
          <div className="chip-row wrap">
            <Chip variant="success">{readyCount} ready</Chip>
            <Chip variant="neutral">{onboarding.length} integrations</Chip>
            <Chip variant={canConnect ? 'accent' : 'warning'}>
              {canConnect ? 'Control plane online' : 'Connection required'}
            </Chip>
          </div>
          <div className="inline-actions">
            <Button variant="primary" onClick={() => handleGoogleConnect()}>
              Connect Google
            </Button>
            <Button variant="ghost" onClick={refreshOnboarding}>
              Refresh status
            </Button>
          </div>
        </div>
      </header>

      <div className="view-grid">
        <section className="stack">
          <div className="section-banner">
            <div>
              <p className="eyebrow">Onboarding status</p>
              <h2>Installed capabilities</h2>
            </div>
          </div>

          <div className="card-grid">
            {onboarding.map((integration, index) => (
              <Card
                key={integration.name}
                eyebrow="Integration"
                title={integration.name}
                subtitle={`Status: ${integration.status}`}
                className="integration-card"
                headerActions={
                  <Chip
                    variant={integration.status === 'ready' ? 'success' : 'warning'}
                  >
                    {integration.status}
                  </Chip>
                }
                style={{ ['--delay' as any]: `${index * 50}ms` }}
              >
                {integration.details?.fields?.length ? (
                  <div className="field-list">
                    {integration.details.fields.map((field) => (
                      <div key={field.name} className="field">
                        <span>{field.label ?? field.name}</span>
                        <span className="muted">{field.type ?? 'text'}</span>
                      </div>
                    ))}
                  </div>
                ) : (
                  <p className="support-copy">
                    No additional onboarding fields are exposed for this integration.
                  </p>
                )}

                {integration.name === 'google' ? (
                  <div className="inline-actions top-gap">
                    <Button variant="primary" size="sm" onClick={() => handleGoogleConnect()}>
                      Launch OAuth
                    </Button>
                    <Button variant="ghost" size="sm" onClick={() => handleGoogleConnect(accountForm.name)}>
                      OAuth with label
                    </Button>
                  </div>
                ) : null}
              </Card>
            ))}
          </div>
        </section>

        <Card
          eyebrow="Provisioning"
          title="Add or label an account"
          subtitle="Use the server-backed account registry for email, Google, or custom integration settings."
          className="form-card"
          tone="accent"
        >
          <form className="form" onSubmit={handleAccountSubmit}>
            <label>
              Integration
              <select
                value={accountForm.integration}
                onChange={(event) =>
                  setAccountForm((prev) => ({
                    ...prev,
                    integration: event.target.value,
                  }))
                }
              >
                <option value="email">email</option>
                <option value="google">google</option>
                <option value="custom">custom</option>
              </select>
            </label>
            <label>
              Friendly name
              <input
                value={accountForm.name}
                onChange={(event) =>
                  setAccountForm((prev) => ({
                    ...prev,
                    name: event.target.value,
                  }))
                }
                placeholder="Work inbox"
              />
            </label>

            {accountForm.integration === 'email' && (
              <div className="field-grid">
                <label>
                  Username
                  <input
                    value={accountForm.username}
                    onChange={(event) =>
                      setAccountForm((prev) => ({
                        ...prev,
                        username: event.target.value,
                      }))
                    }
                    placeholder="me@example.com"
                  />
                </label>
                <label>
                  Password
                  <input
                    type="password"
                    value={accountForm.password}
                    onChange={(event) =>
                      setAccountForm((prev) => ({
                        ...prev,
                        password: event.target.value,
                      }))
                    }
                    placeholder="app password"
                  />
                </label>
                <label>
                  IMAP host
                  <input
                    value={accountForm.imap_host}
                    onChange={(event) =>
                      setAccountForm((prev) => ({
                        ...prev,
                        imap_host: event.target.value,
                      }))
                    }
                    placeholder="imap.example.com"
                  />
                </label>
                <label>
                  IMAP port
                  <input
                    value={accountForm.imap_port}
                    onChange={(event) =>
                      setAccountForm((prev) => ({
                        ...prev,
                        imap_port: event.target.value,
                      }))
                    }
                  />
                </label>
                <label>
                  SMTP host
                  <input
                    value={accountForm.smtp_host}
                    onChange={(event) =>
                      setAccountForm((prev) => ({
                        ...prev,
                        smtp_host: event.target.value,
                      }))
                    }
                    placeholder="smtp.example.com"
                  />
                </label>
                <label>
                  SMTP port
                  <input
                    value={accountForm.smtp_port}
                    onChange={(event) =>
                      setAccountForm((prev) => ({
                        ...prev,
                        smtp_port: event.target.value,
                      }))
                    }
                  />
                </label>
              </div>
            )}

            {accountForm.integration === 'google' && (
              <div className="callout">
                <p>
                  Preferred flow: launch Google OAuth so the server creates the account and
                  stores the refresh token. Manual token entry is still available below.
                </p>
                <div className="inline-actions">
                  <Button
                    variant="primary"
                    type="button"
                    onClick={() => handleGoogleConnect(accountForm.name)}
                  >
                    Launch OAuth
                  </Button>
                  <Chip variant="neutral">Callback: /oauth/callback</Chip>
                </div>
              </div>
            )}

            {accountForm.integration === 'google' && (
              <div className="field-grid">
                <label>
                  Refresh token
                  <input
                    value={accountForm.refresh_token}
                    onChange={(event) =>
                      setAccountForm((prev) => ({
                        ...prev,
                        refresh_token: event.target.value,
                      }))
                    }
                    placeholder="Paste Google refresh token"
                  />
                </label>
                <label>
                  Account email
                  <input
                    value={accountForm.google_email}
                    onChange={(event) =>
                      setAccountForm((prev) => ({
                        ...prev,
                        google_email: event.target.value,
                      }))
                    }
                    placeholder="name@gmail.com"
                  />
                </label>
              </div>
            )}

            {accountForm.integration === 'custom' && (
              <label>
                Config JSON
                <textarea
                  rows={8}
                  value={accountForm.customJson}
                  onChange={(event) =>
                    setAccountForm((prev) => ({
                      ...prev,
                      customJson: event.target.value,
                    }))
                  }
                />
              </label>
            )}

            <div className="inline-actions">
              <Button variant="primary" type="submit" disabled={!canConnect}>
                Save account
              </Button>
              <Chip variant="neutral">{googleOauthUrl}</Chip>
            </div>
          </form>
        </Card>
      </div>
    </section>
  );
};
