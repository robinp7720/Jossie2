import React from 'react';
import { useAppContext } from '../../context/AppContext';
import { Card, Chip } from '../common/UI';
import { KnowledgeGraph } from '../KnowledgeGraph';

export const KnowledgeView: React.FC = () => {
  const { apiConfig } = useAppContext();

  return (
    <section className="view knowledge-view">
      <header className="hero">
        <div className="hero-copy">
          <p className="eyebrow">Knowledge</p>
          <h1>See what the system remembers and how it connects.</h1>
          <p className="hero-text">
            The graph view turns stored entities and relationships into an explorable
            map so memory feels inspectable instead of hidden.
          </p>
        </div>
        <div className="hero-rail">
          <div className="chip-row wrap">
            <Chip variant="accent">D3 graph</Chip>
            <Chip variant="neutral">Interactive filtering</Chip>
            <Chip variant="neutral">{apiConfig.baseUrl || 'No endpoint set'}</Chip>
          </div>
        </div>
      </header>

      <Card className="graph-card" style={{ padding: 0, overflow: 'hidden', minHeight: 680 }}>
        <KnowledgeGraph apiConfig={apiConfig} />
      </Card>
    </section>
  );
};
