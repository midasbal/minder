import type { DryRunInput, DryRunResult, Leg } from '../../chain/contract';
import { reasonLine } from '../../chain/reasons';
import { formatUnits } from '../../chain/units';
import { methodSpec } from '../../chain/assets';
import type { RunState } from '../../chain/store';
import { RawCall } from './RawCall';

const LEG_NAME: Record<Leg['name'], string> = { payment: 'Payment', token: 'Token transfer', gas: 'ETH gas for the transfer' };

function LegLine({ leg, many }: { leg: Leg; many: boolean }) {
  const { verdict } = leg;
  return (
    <li className="leg">
      {many && <span className="leg__name">{LEG_NAME[leg.name]}</span>}
      <span className={`stamp stamp--${verdict.allow ? 'allow' : 'deny'}`}>{verdict.allow ? 'ALLOW' : 'DENY'}</span>
      {!verdict.allow && (
        <span className="leg__why">
          <code>{verdict.reason}</code> {reasonLine(verdict.reason)}
        </span>
      )}
    </li>
  );
}

function Result({ input, result }: { input: DryRunInput; result: DryRunResult }) {
  const spec = methodSpec(input.method);
  const request = `${spec.id}(${input.recipient.trim()}, ${formatUnits(input.amount, spec.decimals)} ${spec.symbol})`;
  const denied = result.legs.find((leg) => !leg.verdict.allow);
  return (
    <div className="verdict" role="status">
      <p className="verdict__request">
        <span className="verdict__tag">Request as the agent</span>
        <code>{request}</code>
      </p>
      <p className="verdict__head">
        <span className="verdict__tag">The contract's answer</span>
        <span className={`stamp stamp--${result.allowed ? 'allow' : 'deny'}`}>{result.allowed ? 'ALLOW' : 'DENY'}</span>
      </p>
      {result.legs.length > 1 && (
        <ul className="legs">
          {result.legs.map((leg) => (
            <LegLine key={leg.name} leg={leg} many />
          ))}
        </ul>
      )}
      {result.allowed ? (
        <p className="verdict__line">
          This request is inside the policy. A dry run sends nothing, so no funds moved; the real call would be signed with the
          agent key and is the one this page does not make.
        </p>
      ) : (
        denied &&
        !denied.verdict.allow && (
          <p className="verdict__line">
            <strong>The firewall refused it.</strong> <code>{denied.verdict.reason}</code>: {reasonLine(denied.verdict.reason)} No key was
            used and nothing was sent; the refusal is computed by the contract itself.
          </p>
        )
      )}
      <RawCall trace={result.trace} />
    </div>
  );
}

/** The outcome of the last dry run: the contract's own verdict, or a plain statement that it could not be got. */
export function Verdict({ run }: { run: RunState }) {
  if (run.phase === 'idle') {
    return <p className="verdict verdict--idle">No request yet. Compose one, or fire a preset: the answer comes from the live contract.</p>;
  }
  if (run.phase === 'running') return <p className="verdict verdict--idle" role="status">Asking the contract…</p>;
  if (run.phase === 'failed') {
    return (
      <p className="verdict verdict--error" role="alert">
        Couldn't get an answer from the contract. {run.message} No verdict is shown because none was received. Submit again to retry.
      </p>
    );
  }
  return <Result input={run.input} result={run.result} />;
}
