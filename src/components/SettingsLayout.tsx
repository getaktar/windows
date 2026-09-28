import { Switch, Text } from "@fluentui/react-components";
import type { ReactNode } from "react";

/** A Settings page: title, one-line subtitle, top-anchored content. */
export function SettingsPage({ title, subtitle, children }: { title: string; subtitle: string; children: ReactNode }) {
  return (
    <div className="settings-page">
      <div className="settings-page-inner">
        <header className="settings-header">
          <Text as="h1" size={600} weight="semibold">
            {title}
          </Text>
          <Text size={300} className="secondary">
            {subtitle}
          </Text>
        </header>
        {children}
      </div>
    </div>
  );
}

export function SettingsSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="settings-section">
      <Text as="h2" size={300} weight="semibold" className="settings-section-title">
        {title}
      </Text>
      {children}
    </section>
  );
}

/** Rounded group of rows, like the cards in Windows Settings. */
export function Card({ children }: { children: ReactNode }) {
  return <div className="card">{children}</div>;
}

export function CardRow({
  title,
  subtitle,
  icon,
  children,
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  icon?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="card-row">
      {icon && <div className="card-row-icon">{icon}</div>}
      <div className="card-row-text">
        <Text>{title}</Text>
        {subtitle && (
          <Text size={200} className="secondary">
            {subtitle}
          </Text>
        )}
      </div>
      {children && <div className="card-row-control">{children}</div>}
    </div>
  );
}

export function ToggleRow(props: {
  title: string;
  subtitle: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <CardRow title={props.title} subtitle={props.subtitle}>
      <Switch
        checked={props.checked}
        disabled={props.disabled}
        onChange={(_, data) => props.onChange(data.checked)}
        aria-label={props.title}
      />
    </CardRow>
  );
}
