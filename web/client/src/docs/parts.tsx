import type { ReactNode } from 'react';

export function DocSection({ id, number, title, children }: { id: string; number: string; title: string; children: ReactNode }) {
  return (
    <section className="doc-section" aria-labelledby={id}>
      <h2 id={id} className="doc-h2">
        <span className="doc-h2__number">{number}</span>
        {title}
      </h2>
      {children}
    </section>
  );
}

export function Sub({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return (
    <div className="doc-sub">
      <h3 id={id} className="doc-h3">
        {title}
      </h3>
      {children}
    </div>
  );
}

/** A callout for something the reader must not skim past. */
export function Note({ title, children }: { title: string; children: ReactNode }) {
  return (
    <aside className="note">
      <p className="note__title">{title}</p>
      {children}
    </aside>
  );
}

export function Table({ caption, head, children }: { caption: string; head: string[]; children: ReactNode }) {
  return (
    <div className="table-wrap" role="region" aria-label={caption} tabIndex={0}>
      <table className="table">
        <caption className="sr-only">{caption}</caption>
        <thead>
          <tr>
            {head.map((cell) => (
              <th key={cell} scope="col">
                {cell}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

export function Pre({ children, label }: { children: string; label: string }) {
  return (
    <pre className="pre" aria-label={label} tabIndex={0}>
      <code>{children}</code>
    </pre>
  );
}
