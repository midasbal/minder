import { nearTxUrl } from '../../chain/liverun';
import { reasonLine } from '../../chain/reasons';
import { RED_TEAM, type OnChainAttempt, type ProtocolRefusal, type RedTeam as RedTeamData } from '../../chain/evidence';
import { ExternalLink } from '../ExternalLink';
import { prettyJson } from './RawCall';
import { utcClock } from './time';

const shortHash = (hash: string) => `${hash.slice(0, 8)}…${hash.slice(-6)}`;

/** The reason groups, in the order a reader meets them. A code not listed is shown under "Other". */
const GROUPS: ReadonlyArray<{ title: string; codes: readonly string[] }> = [
  { title: 'Over the cap', codes: ['PerTxCapExceeded'] },
  { title: 'Not on the allowlist', codes: ['RecipientNotAllowed'] },
  { title: 'An asset with no policy', codes: ['AssetNotAllowed'] },
];

/** What the NEAR runtime's error means, in a line. These are the runtime's, not the contract's. */
const RUNTIME_LINE: Readonly<Record<string, string>> = {
  MethodNameMismatch: "The key may call only its three request methods, and this is not one of them.",
  RequiresFullAccess: 'This action needs a full-access key. The agent holds a function-call key, and the contract account has no full-access key at all.',
  ReceiverMismatch: 'The key may call only the contract it was added to, not another contract.',
};

function Verdict({ attempt }: { attempt: OnChainAttempt }) {
  if (attempt.allowed) return <span className="stamp stamp--allow">ALLOW</span>;
  return <span className="stamp stamp--deny">{attempt.reason}</span>;
}

function Link({ attempt }: { attempt: OnChainAttempt }) {
  return (
    <>
      <ExternalLink href={nearTxUrl(attempt.tx)}>
        <code>{shortHash(attempt.tx)}</code>
      </ExternalLink>{' '}
      <span className="result__note">{utcClock(attempt.blockTime)} UTC</span>
    </>
  );
}

function Denials({ denials }: { denials: OnChainAttempt[] }) {
  const known = new Set(GROUPS.flatMap((group) => group.codes));
  const groups = [...GROUPS, { title: 'Other', codes: [] as readonly string[] }]
    .map((group) => ({
      title: group.title,
      items: denials.filter((attempt) => (group.title === 'Other' ? !known.has(attempt.reason ?? '') : group.codes.includes(attempt.reason ?? ''))),
    }))
    .filter((group) => group.items.length > 0);
  return (
    <div className="rt-groups">
      {groups.map((group) => (
        <div key={group.title} className="rt-group">
          <h4 className="rt-group__title">{group.title}</h4>
          <ul className="rt-attacks">
            {group.items.map((attempt) => (
              <li key={attempt.id} className="rt-attack">
                <p className="rt-attack__what">{attempt.attack}</p>
                <p className="rt-attack__why">
                  <Verdict attempt={attempt} /> <span>{reasonLine(attempt.reason ?? '')}{attempt.leg ? ` (on the ${attempt.leg} leg)` : ''}</span>
                </p>
                <p className="rt-attack__tx">
                  <Link attempt={attempt} />
                </p>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}

function WindowBurst({ explanation, steps }: { explanation: string; steps: OnChainAttempt[] }) {
  return (
    <div className="rt-window">
      <h3 className="rt-window__title">The window: a limit one dry run cannot reach</h3>
      <p className="rt-window__lede">{explanation}</p>
      <ol className="rt-steps">
        {steps.map((step, index) => (
          <li key={step.id} className="rt-step">
            <p className="rt-step__n">Request {index + 1}</p>
            <p className="rt-step__what">{step.attack}</p>
            <p className="rt-step__result">
              <Verdict attempt={step} />
            </p>
            <p className="rt-step__tx">
              <Link attempt={step} />
            </p>
          </li>
        ))}
      </ol>
      <p className="rt-window__why">
        Every request was within the per-transaction cap. The third was refused because together they would pass the
        window&rsquo;s limit: {reasonLine('WindowLimitExceeded')}
      </p>
    </div>
  );
}

function Refusals({ refusals }: { refusals: ProtocolRefusal[] }) {
  return (
    <div className="rt-refusals">
      <h3 className="rt-refusals__title">Refused by the NEAR runtime, before any contract code ran</h3>
      <p className="rt-refusals__lede">
        These never reached a block, so there is no transaction to open: the runtime checks what the key is allowed to do
        first, and rejects the call outright. What each one shows is the raw error the RPC node returned.
      </p>
      <ul className="rt-attacks">
        {refusals.map((refusal) => (
          <li key={refusal.id} className="rt-attack">
            <p className="rt-attack__what">{refusal.attack}</p>
            <p className="rt-attack__why">
              <span className="stamp stamp--deny">{refusal.error}</span> <span>{RUNTIME_LINE[refusal.error] ?? 'Rejected by the runtime.'}</span>
            </p>
            <details className="more">
              <summary>The raw RPC error</summary>
              <pre className="raw__code">{prettyJson(refusal.rawRpcResponse)}</pre>
              <p className="rt-attack__note">
                No block, so no transaction on the explorer. The signed transaction would have had the hash{' '}
                <code className="break">{refusal.signedTransactionHash}</code>.
              </p>
            </details>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** The red-team log: what a compromised agent tried against the live contract, with the proof to open. */
export function RedTeam({ data = RED_TEAM }: { data?: RedTeamData }) {
  const refusedOnChain = data.denials.length + data.window.filter((step) => !step.allowed).length;
  return (
    <section className="run" aria-labelledby="redteam-title">
      <p className="label">03 / The red-team log</p>
      <h2 className="run__title" id="redteam-title">
        Every attack below was run against the live contract. It refused them all.
      </h2>
      <p className="run__lede">
        Most were refused on chain, so you can open each failed transaction and read the contract&rsquo;s own reason in its
        log. Unlike a dry run, these are real calls, signed with the agent&rsquo;s real key. {refusedOnChain} were refused by
        the contract and {data.refusals.length} by the NEAR runtime.
      </p>
      <Denials denials={data.denials} />
      <WindowBurst explanation={data.windowExplanation} steps={data.window} />
      <Refusals refusals={data.refusals} />
    </section>
  );
}
