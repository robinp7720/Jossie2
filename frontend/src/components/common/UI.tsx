import React from 'react';

interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: 'primary' | 'secondary' | 'ghost' | 'danger';
  size?: 'sm' | 'md' | 'lg';
  loading?: boolean;
}

export const Button: React.FC<ButtonProps> = ({
  children,
  variant = 'secondary',
  size = 'md',
  loading = false,
  className = '',
  disabled,
  ...props
}) => (
  <button
    className={`button ${variant} ${size} ${className}`.trim()}
    disabled={loading || disabled}
    {...props}
  >
    {loading ? 'Working…' : children}
  </button>
);

interface CardProps extends React.HTMLAttributes<HTMLDivElement> {
  children: React.ReactNode;
  title?: string;
  eyebrow?: string;
  subtitle?: string;
  headerActions?: React.ReactNode;
  tone?: 'default' | 'accent' | 'dark';
}

export const Card: React.FC<CardProps> = ({
  children,
  title,
  eyebrow,
  subtitle,
  className = '',
  headerActions,
  tone = 'default',
  ...props
}) => (
  <section className={`card card-${tone} ${className}`.trim()} {...props}>
    {(eyebrow || title || subtitle || headerActions) && (
      <header className="card-header">
        <div className="card-header-copy">
          {eyebrow ? <p className="card-eyebrow">{eyebrow}</p> : null}
          {title ? <h3>{title}</h3> : null}
          {subtitle ? <p className="card-subtitle">{subtitle}</p> : null}
        </div>
        {headerActions ? <div className="card-header-actions">{headerActions}</div> : null}
      </header>
    )}
    {children}
  </section>
);

interface ChipProps extends React.HTMLAttributes<HTMLSpanElement> {
  children: React.ReactNode;
  active?: boolean;
  onClick?: () => void;
  variant?: 'neutral' | 'accent' | 'success' | 'warning';
}

export const Chip: React.FC<ChipProps> = ({
  children,
  active = false,
  onClick,
  className = '',
  variant = 'neutral',
  ...props
}) => {
  const classes = `chip chip-${variant} ${active ? 'active' : ''} ${className}`.trim();

  if (onClick) {
    return (
      <button type="button" className={`${classes} chip-button`} onClick={onClick}>
        {children}
      </button>
    );
  }

  return (
    <span className={classes} {...props}>
      {children}
    </span>
  );
};
