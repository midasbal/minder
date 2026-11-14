import type { ReactNode } from 'react';

import { CONTRACT_ACCOUNT } from '../../chain/contract';
import { reachedChain, type AssetRow, type Section, type Snapshot } from '../../chain/snapshot';
import type { ConsoleState, ConsoleStore } from '../../chain/store';
import { EXPLORER_URL } from '../../links';
import { ExternalLink } from '../ExternalLink';

/** A section's content, or the plain reason it could not be read. Never a blank. */
function Read<T>({ section, children }: { section: Section<T>; children: (value: T) => ReactNode }) {
  if (section.ok) return <>{children(section.value)}</>;
  return <p className="unread">Couldn't read this from the chain. {section.message}</p>;
}

function Facts({ title, source, children }: { title: string; source: string; children: ReactNode }) {
  return (
    <section className="facts">
      <h3 className="facts__title">{title}</h3>
      <p className="facts__source">
        <code>{source}</code>
      </p>
      {children}
    </section>
  );
}

function Row({ name, children }: { name: string; children: ReactNode }) {
  return (
    <div className="kv__row">
      <dt>{name}</dt>
      <dd>{children}</dd>
    </div>
  );
}

function Codes({ items }: { items: string[] | null }) {
  if (items === null) return <>no limit of its own</>;
  if (items.length === 0) return <>none</>;
  return (
    <span className="codes">
      {items.map((item) => (
        <code key={item}>{item}</code>
      ))}
    </span>
  );
}

/** One row per asset: the policy and what is left of it, so the three assets read side by side. */
function PolicyTable({ rows, windowSeconds }: { rows: AssetRow[]; windowSeconds: number | null }) {
  const unread = rows.flatMap((row) => [row.headroom, row.usage]).find((section) => !section.ok);
  return (
    <>
      <div className="ptable-wrap">
        <table className="ptable">
          <caption className="sr-only">Policy and headroom per asset, in whole units of each asset</caption>
          <thead>
            <tr>
              <th scope="col">Asset</th>
              <th scope="col">Per tx</th>
              <th scope="col">{windowSeconds === null ? 'Window' : `${windowSeconds} s window`}, left / limit</th>
              <th scope="col">Budget, left / total</th>
              <th scope="col">Committed</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.key}>
                <th scope="row" title={row.label}>
                  {row.symbol}
                </th>
                <td>{row.perTx}</td>
                <td>{row.headroom.ok ? `${row.headroom.value.windowRemaining} / ${row.window}` : 'unread'}</td>
                <td>
                  {row.headroom.ok
                    ? row.headroom.value.budgetRemaining === null || row.budget === null
                      ? 'none set'
                      : `${row.headroom.value.budgetRemaining} / ${row.budget}`
                    : 'unread'}
                </td>
                <td>{row.usage.ok ? row.usage.value.committed : 'unread'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {unread && !unread.ok && <p className="unread">Couldn't read part of this table from the chain. {unread.message}</p>}
    </>
  );
}

function More({ summary, children }: { summary: string; children: ReactNode }) {
  return (
    <details className="more">
      <summary>{summary}</summary>
      {children}
    </details>
  );
}

function Body({ snapshot }: { snapshot: Snapshot }) {
  const windowSeconds = snapshot.contract.ok ? snapshot.contract.value.windowSeconds : null;
  return (
    <>
      <div className="vitals">
        <Facts title="Keyless" source="view_access_key_list">
          <Read section={snapshot.keys}>
            {(keys) => (
              <>
                <p className="headline">
                  <span className={keys.fullAccess === 0 ? 'is-verified' : undefined}>
                    {keys.fullAccess} full-access {keys.fullAccess === 1 ? 'key' : 'keys'}
                  </span>
                  , {keys.restricted} restricted {keys.restricted === 1 ? 'key' : 'keys'}
                </p>
                <p className="facts__note">
                  No one can sign as the contract or change its code. <ExternalLink href={EXPLORER_URL}>Explorer</ExternalLink>
                </p>
                {keys.keys.length > 0 && (
                  <More summary="The restricted key">
                    {keys.keys.map((key) => (
                      <dl className="kv" key={key.publicKey}>
                        <Row name="Key">
                          <code>{key.publicKey}</code>
                        </Row>
                        <Row name="May call only">
                          <Codes items={key.methods} />
                        </Row>
                        <Row name="On">
                          <code>{key.receiver}</code>
                        </Row>
                        <Row name="Allowance left">{key.allowance}</Row>
                      </dl>
                    ))}
                  </More>
                )}
              </>
            )}
          </Read>
        </Facts>

        <Facts title="Deployed code" source="view_code, hashed in this browser">
          <Read section={snapshot.code}>
            {(code) => (
              <>
                <p className="headline headline--hash">
                  <code className="break">{code.sha256}</code>
                </p>
                <p className="facts__note">
                  SHA-256 of the {code.bytes.toLocaleString('en-US')}-byte wasm the chain holds.{' '}
                  {code.matches ? (
                    <span className="is-verified">Matches the repository build.</span>
                  ) : (
                    <span>Does NOT match the repository build ({code.expected}).</span>
                  )}
                </p>
              </>
            )}
          </Read>
        </Facts>
      </div>

      <Facts title="State" source="get_config, get_status">
        <Read section={snapshot.contract}>
          {(contract) => (
            <p className="headline">
              {contract.status} · {contract.windowSeconds} s spend window · {contract.timelockSeconds} s timelock on loosening
            </p>
          )}
        </Read>
      </Facts>

      <Facts title="Policy and headroom" source="get_asset_policies, get_headroom, get_budget_usage">
        <Read section={snapshot.assets}>{(rows) => <PolicyTable rows={rows} windowSeconds={windowSeconds} />}</Read>
      </Facts>

      <div className="expanders">
        <Read section={snapshot.mandate}>
          {(mandate) =>
            mandate === null ? (
              <p className="facts__note">No mandate is granted: the agent can do nothing.</p>
            ) : (
              <More summary={`Mandate · grant #${mandate.epoch} · expires ${mandate.expiry} UTC · ${mandate.revoked ? 'revoked' : 'not revoked'}`}>
                <dl className="kv">
                  <Row name="Agent key">
                    <code>{mandate.agentKey}</code>
                  </Row>
                  <Row name="Assets in scope">{mandate.assets.join(', ') || 'none'}</Row>
                  <Row name="NEAR recipients in scope">
                    <Codes items={mandate.nearRecipients} />
                  </Row>
                  <Row name="EVM recipients in scope">
                    <Codes items={mandate.evmRecipients} />
                  </Row>
                  <Row name="Extra cap, in base units">{mandate.perTxCap ?? 'none'}</Row>
                </dl>
              </More>
            )
          }
        </Read>
        {snapshot.contract.ok && (
          <More summary="Roles · owner and guardian">
            <dl className="kv">
              <Row name="Owner">
                <code>{snapshot.contract.value.owner}</code>
              </Row>
              <Row name="Guardian">
                {snapshot.contract.value.guardian === null ? 'none' : <code>{snapshot.contract.value.guardian}</code>}
              </Row>
            </dl>
          </More>
        )}
        <More summary="Allowlists · who the agent may pay">
          <dl className="kv">
            <Row name="NEAR recipients">
              {snapshot.nearRecipients.ok ? <Codes items={snapshot.nearRecipients.value} /> : <>unread: {snapshot.nearRecipients.message}</>}
            </Row>
            <Row name="EVM recipients">
              {snapshot.evmRecipients.ok ? <Codes items={snapshot.evmRecipients.value} /> : <>unread: {snapshot.evmRecipients.message}</>}
            </Row>
          </dl>
        </More>
      </div>
    </>
  );
}

const clock = (milliseconds: number) => new Date(milliseconds).toISOString().slice(11, 19);

/** Panel 2: what the chain says about the contract, read from the chain, with the time and block of the reading. */
export function LivePanel({ store, state }: { store: ConsoleStore; state: ConsoleState }) {
  const { snapshot, reading } = state;
  const busy = reading === 'loading';
  return (
    <div className="panel panel--live">
      <div className="panel__bar">
        <p className="panel__live">
          <span className={snapshot && reachedChain(snapshot) ? 'live-dot' : undefined} aria-hidden="true" />
          {snapshot === null
            ? 'Reading the contract from the chain…'
            : reachedChain(snapshot)
              ? `Read from the chain at ${clock(snapshot.readAt)} UTC, block ${snapshot.blockHeight.toLocaleString('en-US')}.`
              : `Tried at ${clock(snapshot.readAt)} UTC.`}
        </p>
        <button type="button" className="btn btn--secondary" onClick={() => void store.load(true)} disabled={busy}>
          {busy ? 'Reading…' : 'Refresh'}
        </button>
      </div>
      <p className="panel__source">
        Live testnet data from <code>{CONTRACT_ACCOUNT}</code>, fetched by this page straight from a public NEAR node. No server of ours
        is in between{snapshot?.endpoint ? <>, here <code>{new URL(snapshot.endpoint).host}</code></> : null}.
      </p>
      {snapshot !== null && !reachedChain(snapshot) && (
        <p className="unread" role="alert">
          Couldn't reach the NEAR RPC. Nothing below is shown because nothing was received. Use Refresh to retry.
        </p>
      )}
      {snapshot !== null && <Body snapshot={snapshot} />}
    </div>
  );
}
