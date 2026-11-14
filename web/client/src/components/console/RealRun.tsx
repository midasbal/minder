import type { ReactNode } from 'react';

import { allThreeMatch, type Proof, type Sourced } from '../../chain/provenance';
import { nearTxUrl, sepoliaTxUrl, type LiveRun, type RecordedPayment } from '../../chain/liverun';
import { CONTRACT_ACCOUNT } from '../../chain/contract';
import type { Settlement } from '../../chain/settlement';
import type { ConsoleState, ConsoleStore } from '../../chain/store';
import { ExternalLink } from '../ExternalLink';

const shortHash = (hash: string) => `${hash.slice(0, 8)}…${hash.slice(-6)}`;
const shortAddress = (address: string) => `${address.slice(0, 8)}…${address.slice(-6)}`;

const ASSET: Record<RecordedPayment['kind'], string> = { near_transfer: 'NEAR', evm_eth: 'ETH on Sepolia', evm_erc20: 'USDC on Sepolia' };

/** How a payment ended: what a Sepolia node says now where it can, and otherwise the recorded status, said to be so. */
function Settled({ payment, state }: { payment: RecordedPayment; state: ConsoleState }) {
  const recorded = payment.status === 'success' ? <span className="is-verified">Succeeded</span> : <span>Status: {payment.status}</span>;
  if (payment.sepoliaTxHash === null) {
    return (
      <>
        {recorded} <span className="result__note">recorded</span>
      </>
    );
  }
  if (state.settlement.phase !== 'done') {
    return (
      <>
        {recorded} <span className="result__note">recorded; checking Sepolia now…</span>
      </>
    );
  }
  const live: Settlement | undefined = state.settlement.byHash[payment.sepoliaTxHash];
  if (live?.state === 'success') {
    return (
      <>
        <span className="is-verified">Succeeded</span> <span className="result__note">confirmed on Sepolia just now, block {live.blockNumber.toLocaleString('en-US')}</span>
      </>
    );
  }
  if (live?.state === 'reverted') {
    return (
      <>
        <span>Reverted on Sepolia</span> <span className="result__note">confirmed just now, block {live.blockNumber.toLocaleString('en-US')}; the record said {payment.status}</span>
      </>
    );
  }
  const why = live?.state === 'unread' ? `the live check failed: ${live.reason}` : 'Sepolia has no receipt for it just now';
  return (
    <>
      {recorded} <span className="result__note">recorded; {why}</span>
    </>
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

function Payment({ payment, state }: { payment: RecordedPayment; state: ConsoleState }) {
  return (
    <li className="payment">
      <p className="payment__what">
        <span className="payment__asset">{ASSET[payment.kind]}</span>
        <strong className="payment__amount">{payment.amountDisplay}</strong>
        <span className="payment__to">
          to <code>{payment.recipient}</code>
        </span>
      </p>
      <dl className="kv">
        <Row name="On NEAR: requested and gated">
          <ExternalLink href={nearTxUrl(payment.nearTxHash)}>
            <code>{shortHash(payment.nearTxHash)}</code>
          </ExternalLink>
        </Row>
        {payment.sepoliaTxHash !== null && (
          <Row name="On Sepolia: settled">
            <ExternalLink href={sepoliaTxUrl(payment.sepoliaTxHash)}>
              <code>{shortHash(payment.sepoliaTxHash)}</code>
            </ExternalLink>
          </Row>
        )}
        <Row name="Result">
          <Settled payment={payment} state={state} />
        </Row>
      </dl>
    </li>
  );
}

function Address({ label, who, value, matches }: { label: string; who: string; value: Sourced | string; matches: boolean | null }) {
  const address = typeof value === 'string' ? value : value.address;
  const source = typeof value === 'string' ? null : value;
  return (
    <li className="chain__link">
      <p className="chain__label">{label}</p>
      <p className="chain__address">
        <code className={matches === true ? 'is-verified' : undefined}>{address}</code>
      </p>
      <p className="chain__note">
        {who}
        {source && (
          <>
            {' '}
            · {source.source === 'live' ? 'read from the chain just now' : 'recorded with the run'}
            {source.reason ? `, because the live read failed: ${source.reason}` : ''}
          </>
        )}
      </p>
    </li>
  );
}

function PaymentProof({ payment, index, proof }: { payment: RecordedPayment; index: number; proof: Proof }) {
  const entry = proof.recoveries.find((candidate) => candidate.index === index);
  if (!entry || payment.rawTx === null) return null;
  const { recovery } = entry;
  const matches = allThreeMatch(proof, recovery);
  const signer = recovery.ok ? recovery.signed.address : null;
  return (
    <li className="proof">
      <h4 className="proof__title">
        {ASSET[payment.kind]}, {payment.amountDisplay}
      </h4>
      {payment.sepoliaTxHash !== null && (
        <p className="proof__tx">
          On Sepolia:{' '}
          <ExternalLink href={sepoliaTxUrl(payment.sepoliaTxHash)}>
            <code>{shortHash(payment.sepoliaTxHash)}</code>, open on Etherscan
          </ExternalLink>
        </p>
      )}
      {!recovery.ok ? (
        <p className="unread">Couldn't check this payment: {recovery.error}</p>
      ) : (
        <>
          <ol className="chain">
            <Address
              label="Signed the Sepolia transaction"
              who="Recovered in this browser from the signature in the raw transaction"
              value={recovery.signed.address}
              matches={matches}
            />
            <Address
              label="The contract's configured sender"
              who={`get_evm_config on ${CONTRACT_ACCOUNT}`}
              value={proof.sender}
              matches={matches}
            />
            <Address
              label="The address NEAR Chain Signatures derives for this contract"
              who={`${proof.signer}, path ${proof.path}`}
              value={proof.derived}
              matches={matches}
            />
          </ol>
          <p className="proof__verdict" role="status">
            {matches ? (
              <>
                <span className="is-verified">All three are the same address.</span> This real Sepolia transaction was signed by{' '}
                <code>{shortAddress(signer ?? '')}</code>, an address only this NEAR contract controls. The contract is the sole signer, and you
                just checked it here.
              </>
            ) : (
              <>
                <strong>The three addresses are not all the same.</strong> This transaction cannot be tied to the contract by these values.
              </>
            )}
          </p>
        </>
      )}
      <details className="more">
        <summary>The signed transaction</summary>
        {recovery.ok && (
          <dl className="kv">
            <Row name="To">
              <code>{recovery.signed.to ?? 'none'}</code>
            </Row>
            <Row name="Nonce">{recovery.signed.nonce}</Row>
            <Row name="Chain id">{recovery.signed.chainId}</Row>
          </dl>
        )}
        <pre className="raw__code">{payment.rawTx}</pre>
        {payment.sepoliaTxHash !== null && (
          <p className="proof__tx">
            This is the transaction{' '}
            <ExternalLink href={sepoliaTxUrl(payment.sepoliaTxHash)}>
              <code>{payment.sepoliaTxHash}</code>
            </ExternalLink>{' '}
            on Sepolia Etherscan.
          </p>
        )}
      </details>
    </li>
  );
}

function Provenance({ run, state, store }: { run: LiveRun; state: ConsoleState; store: ConsoleStore }) {
  const { proof } = state;
  const shown = proof.phase === 'done' ? proof.proof : proof.phase === 'running' ? proof.previous : null;
  return (
    <div className="provenance">
      <div className="provenance__head">
        <div>
          <h3 className="provenance__title">Proof: only this contract could have signed it</h3>
          <p className="provenance__lede">
            Each EVM payment above is a transaction signed through NEAR Chain Signatures. This page recomputes who signed it, and
            compares that with what the chain says the contract controls.
          </p>
        </div>
        <button type="button" className="btn btn--secondary" onClick={() => void store.verify(true)} disabled={proof.phase === 'running'}>
          {proof.phase === 'running' ? 'Checking…' : 'Check again'}
        </button>
      </div>
      {shown === null ? (
        <p className="unread" role="status">
          Checking the signatures in your browser…
        </p>
      ) : (
        <ul className="proofs">
          {run.payments.map((payment, index) => (
            <PaymentProof key={payment.nearTxHash} payment={payment} index={index} proof={shown} />
          ))}
        </ul>
      )}
    </div>
  );
}

/** The real run: payments that actually moved, under containment, and the proof of who signed them. */
export function RealRun({ run, state, store }: { run: LiveRun; state: ConsoleState; store: ConsoleStore }) {
  return (
    <section className="run" aria-labelledby="run-title">
      <p className="label">03 / The real run</p>
      <h2 className="run__title" id="run-title">
        Not a simulation. Real money moved, and the firewall gated every step.
      </h2>
      <p className="run__flow">
        <span>The agent requested it</span>
        <span aria-hidden="true">→</span>
        <span>the firewall allowed it</span>
        <span aria-hidden="true">→</span>
        <span>the contract signed it, with no master key</span>
        <span aria-hidden="true">→</span>
        <span>it settled on Sepolia</span>
      </p>
      <ol className="payments">
        {run.payments.map((payment) => (
          <Payment key={payment.nearTxHash} payment={payment} state={state} />
        ))}
      </ol>
      <p className="run__note">
        Recorded {run.recordedAt.slice(0, 10)} on testnet. Every hash is a link: open it on the explorer and check the amount, the
        recipient (<code>{run.vendorEvmAddress}</code> on Sepolia) and the status yourself.
      </p>
      <Provenance run={run} state={state} store={store} />
    </section>
  );
}
