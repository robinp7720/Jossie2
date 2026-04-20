import React, { useEffect } from 'react';
import { useAppContext } from '../../context/AppContext';
import { Card, Button, Chip } from '../common/UI';
import { deleteAccount } from '../../api';

export const AccountsView: React.FC = () => {
  const {
    apiConfig,
    accounts,
    refreshAccounts,
    canConnect,
    setStatusMessage,
  } = useAppContext();

  useEffect(() => {
    refreshAccounts();
  }, [refreshAccounts]);

  const handleDeleteAccount = async (accountId: string) => {
    if (!canConnect) return;
    try {
      await deleteAccount(apiConfig, accountId);
      refreshAccounts();
    } catch (error) {
      setStatusMessage(error instanceof Error ? error.message : String(error));
    }
  };

  return (
    <section className="view accounts-view">
      <header className="hero">
        <div className="hero-copy">
          <p className="eyebrow">Accounts</p>
          <h1>Inspect stored identities and credentials.</h1>
          <p className="hero-text">
            This view surfaces the server-side account registry so you can verify
            what integrations are configured and remove stale entries quickly.
          </p>
        </div>
        <div className="hero-rail">
          <div className="chip-row wrap">
            <Chip variant="neutral">{accounts.length} saved</Chip>
            <Chip variant={canConnect ? 'success' : 'warning'}>
              {canConnect ? 'Server connected' : 'Connection required'}
            </Chip>
          </div>
          <Button variant="ghost" onClick={refreshAccounts}>
            Refresh accounts
          </Button>
        </div>
      </header>

      <div className="card-grid">
        {accounts.map((account, index) => (
          <Card
            key={account.id}
            eyebrow={account.integration}
            title={account.name}
            subtitle={`ID ${account.id.slice(0, 8)}`}
            className="account-card"
            headerActions={
              <Button
                variant="ghost"
                size="sm"
                onClick={() => handleDeleteAccount(account.id)}
              >
                Remove
              </Button>
            }
            style={{ ['--delay' as any]: `${index * 60}ms` }}
          >
            <div className="chip-row wrap">
              <Chip variant="accent">{account.integration}</Chip>
            </div>
            <pre className="code">
              {JSON.stringify(account.details ?? {}, null, 2)}
            </pre>
          </Card>
        ))}
        {accounts.length === 0 && (
          <div className="empty-state">
            <p className="eyebrow">No accounts</p>
            <h2>No accounts configured yet.</h2>
            <p>Add one from the Integrations view to start using email, Google, or custom connectors.</p>
          </div>
        )}
      </div>
    </section>
  );
};
