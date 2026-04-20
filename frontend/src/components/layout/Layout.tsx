import React, { useState } from 'react';
import { Sidebar } from './Sidebar';
import { Inspector } from './Inspector';
import { ChatView } from '../views/ChatView/ChatView';
import { IntegrationsView } from '../views/IntegrationsView';
import { AccountsView } from '../views/AccountsView';
import { KnowledgeView } from '../views/KnowledgeView';
import { useEvents } from '../../hooks/useEvents';

type Tab = 'assistant' | 'integrations' | 'accounts' | 'knowledge';

export const Layout: React.FC = () => {
  const [activeTab, setActiveTab] = useState<Tab>('assistant');

  useEvents();

  return (
    <div className="shell">
      <div className="shell-orb shell-orb-a" />
      <div className="shell-orb shell-orb-b" />
      <div className="shell-grid" />

      <div className="app-shell">
        <Sidebar activeTab={activeTab} onTabChange={setActiveTab} />

        <main className="workspace" key={activeTab}>
          {activeTab === 'assistant' && <ChatView />}
          {activeTab === 'integrations' && <IntegrationsView />}
          {activeTab === 'accounts' && <AccountsView />}
          {activeTab === 'knowledge' && <KnowledgeView />}
        </main>

        <Inspector />
      </div>
    </div>
  );
};
