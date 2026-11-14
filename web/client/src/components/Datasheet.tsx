import type { ReactNode } from 'react';

export interface SheetRow {
  key: string;
  value: ReactNode;
  /** A verified value is the one place besides a verdict that wears the allow colour. */
  verified?: boolean;
}

/** A datasheet panel: a titled list of key and value rows, and a footnote. */
export function Datasheet({
  id,
  title,
  tag,
  rows,
  footnote,
}: {
  id: string;
  title: string;
  tag: string;
  rows: SheetRow[];
  footnote: ReactNode;
}) {
  return (
    <section className="sheet" aria-labelledby={id}>
      <header className="sheet__head">
        <h3 className="sheet__title" id={id}>
          {title}
        </h3>
        <span className="sheet__tag">{tag}</span>
      </header>
      <dl className="sheet__rows">
        {rows.map(({ key, value, verified }) => (
          <div className="sheet__row" key={key}>
            <dt>{key}</dt>
            <dd className={verified ? 'is-verified' : undefined}>{value}</dd>
          </div>
        ))}
      </dl>
      <p className="sheet__foot">{footnote}</p>
    </section>
  );
}
