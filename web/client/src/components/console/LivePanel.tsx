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

function Asset({ row, windowSeconds }: { row: AssetRow; windowSeconds: number | null }) {
  return (
    <div className="asset">
      <h4 className="asset__title">{row.label}</h4>
      <dl className="kv">
        <Row name="Per-transaction cap">{row.perTx}</Row>
        <Row name={windowSeconds === null ? 'Window limit' : `Window limit (${windowSeconds} s)`}>{row.window}</Row>
        <Row name="Budget for this grant">{row.budget ?? 'none'}</Row>
        <Row name="Window left now">{row.headroom.ok ? row.headroom.value.windowRemaining : 'unread'}</Row>
        <Row name="Budget left now">
          {row.headroom.ok ? (row.headroom.value.budgetRemaining ?? 'no budget set') : 'unread'}
        </Row>
        <Row name="Committed / in flight">
          {row.usage.ok ? `${row.usage.value.committed} / ${row.usage.value.reserved}` : 'unread'}
        </Row>
      </dl>
      {(!row.headroom.ok || !row.usage.ok) && (
        <p className="unread">
          Couldn't read the headroom or usage for this asset. {!row.headroom.ok ? row.headroom.message : !row.usage.ok ? row.usage.message : ''}
        </p>
      )}
    </div>
  );
}

function Body({ snapshot }: { snapshot: Snapshot }) {
  const windowSeconds = snapshot.contract.ok ? snapshot.contract.value.windowSeconds : null;
  return (
    <>
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
                No full-access key exists on the contract account, so no one can sign as it or change its code.{' '}
                <ExternalLink href={EXPLORER_URL}>Check on the explorer</ExternalLink>
              </p>
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
            </>
          )}
        </Read>
      </Facts>

      <Facts title="Deployed code" source="view_code, hashed in this browser">
        <Read section={snapshot.code}>
          {(code) => (
            <>
              <p className="headline">
                <code className="break">{code.sha256}</code>
              </p>
              <p className="facts__note">
                SHA-256 of the {code.bytes.toLocaleString('en-US')} bytes of wasm the chain holds for this account.{' '}
                {code.matches ? (
                  <span className="is-verified">It matches the build recorded in this repository.</span>
                ) : (
                  <span>It does NOT match the build recorded in this repository ({code.expected}).</span>
                )}
              </p>
            </>
          )}
        </Read>
      </Facts>

      <Facts title="State" source="get_config, get_status">
        <Read section={snapshot.contract}>
          {(contract) => (
            <dl className="kv">
              <Row name="Status">{contract.status}</Row>
              <Row name="Owner">
                <code>{contract.owner}</code>
              </Row>
              <Row name="Guardian">{contract.guardian === null ? 'none' : <code>{contract.guardian}</code>}</Row>
              <Row name="Spend window">
                {contract.windowSeconds} s, in {contract.buckets} buckets
              </Row>
              <Row name="Timelock on loosening">{contract.timelockSeconds} s</Row>
            </dl>
          )}
        </Read>
      </Facts>

      <Facts title="Mandate" source="get_mandate">
        <Read section={snapshot.mandate}>
          {(mandate) =>
            mandate === null ? (
              <p className="facts__note">No mandate is granted: the agent can do nothing.</p>
            ) : (
              <dl className="kv">
                <Row name="Agent key">
                  <code>{mandate.agentKey}</code>
                </Row>
                <Row name="Expires">{mandate.expiry} (UTC)</Row>
                <Row name="Revoked">{mandate.revoked ? 'yes' : 'no'}</Row>
                <Row name="Grant">#{mandate.epoch}</Row>
                <Row name="Assets in scope">{mandate.assets.join(', ') || 'none'}</Row>
                <Row name="NEAR recipients in scope">
                  <Codes items={mandate.nearRecipients} />
                </Row>
                <Row name="EVM recipients in scope">
                  <Codes items={mandate.evmRecipients} />
                </Row>
                <Row name="Extra cap, in base units">{mandate.perTxCap ?? 'none'}</Row>
              </dl>
            )
          }
        </Read>
      </Facts>

      <Facts title="Policy and headroom" source="get_asset_policies, get_headroom, get_budget_usage">
        <Read section={snapshot.assets}>
          {(rows) => (
            <div className="assets">
              {rows.map((row) => (
                <Asset key={row.key} row={row} windowSeconds={windowSeconds} />
              ))}
            </div>
          )}
        </Read>
      </Facts>

      <Facts title="Allowlists" source="get_recipients, get_evm_recipients">
        <dl className="kv">
          <Row name="NEAR recipients">
            {snapshot.nearRecipients.ok ? <Codes items={snapshot.nearRecipients.value} /> : <>unread: {snapshot.nearRecipients.message}</>}
          </Row>
          <Row name="EVM recipients">
            {snapshot.evmRecipients.ok ? <Codes items={snapshot.evmRecipients.value} /> : <>unread: {snapshot.evmRecipients.message}</>}
          </Row>
        </dl>
      </Facts>
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
