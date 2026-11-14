import { FREEZE_DRILL, type DrillEntry, type FreezeDrill as FreezeDrillData } from '../../chain/evidence';
import { nearTxUrl } from '../../chain/liverun';
import { reasonLine } from '../../chain/reasons';
import { ExternalLink } from '../ExternalLink';
import { elapsed, utcClock } from './time';

const shortHash = (hash: string) => `${hash.slice(0, 8)}…${hash.slice(-6)}`;

const WHO: Record<DrillEntry['id'], string> = {
  freeze: 'The guardian',
  'denied-while-frozen': 'The agent',
  'propose-unfreeze': 'The owner',
  'execute-unfreeze': 'The owner',
};

function Outcome({ entry, drill }: { entry: DrillEntry; drill: FreezeDrillData }) {
  switch (entry.id) {
    case 'freeze':
      return <span>The contract is <strong>Frozen</strong>, at once.</span>;
    case 'denied-while-frozen':
      return (
        <>
          <span className="stamp stamp--deny">{entry.reason}</span> <span>{reasonLine(entry.reason ?? '')}</span>
        </>
      );
    case 'propose-unfreeze':
      return <span>Queued, not applied: it is a loosening. Executable from {utcClock(drill.readyAt)} UTC.</span>;
    case 'execute-unfreeze':
      return (
        <>
          <span className="is-verified">{drill.finalStatus}</span> <span>again.</span>
        </>
      );
  }
}

/** The freeze drill: stopping the contract is instant, resuming it waits out the delay, even for the owner. */
export function FreezeDrill({ data = FREEZE_DRILL }: { data?: FreezeDrillData }) {
  const [freeze, refused, propose, execute] = data.entries as [DrillEntry, DrillEntry, DrillEntry, DrillEntry];
  return (
    <section className="run" aria-labelledby="drill-title">
      <p className="label">05 / Containment and recovery</p>
      <h2 className="run__title" id="drill-title">
        Stopping is instant. Resuming is deliberately delayed.
      </h2>
      <p className="run__lede">
        A real drill on the live contract. The guardian can halt everything with one call. But even the owner must wait out
        the {data.delaySeconds}-second timelock to resume, so whoever stole a key cannot quietly undo a freeze. Every step
        below is a transaction you can open.
      </p>
      <ol className="fd-steps">
        {data.entries.map((entry) => (
          <li key={entry.id} className="fd-step">
            <p className="fd-step__time">{utcClock(entry.blockTime)} UTC</p>
            <div className="fd-step__body">
              <p className="fd-step__what">
                <strong>{WHO[entry.id]}</strong> · <code>{entry.id === 'denied-while-frozen' ? 'request_local_transfer' : entry.id === 'freeze' ? 'freeze' : entry.id === 'propose-unfreeze' ? 'propose_change(Unfreeze)' : 'execute_change'}</code>
              </p>
              <p className="fd-step__desc">{entry.description}</p>
              <p className="fd-step__out">
                <Outcome entry={entry} drill={data} />
              </p>
              <p className="fd-step__tx">
                <ExternalLink href={nearTxUrl(entry.tx)}>
                  <code>{shortHash(entry.tx)}</code>
                </ExternalLink>
              </p>
            </div>
          </li>
        ))}
      </ol>
      <p className="run__note">
        From the freeze to the refused request: {elapsed(freeze.blockTime, refused.blockTime)}. From the proposal to the
        execution: {elapsed(propose.blockTime, execute.blockTime)}, against a {data.delaySeconds}-second delay. The times are
        the blocks&rsquo; own, in UTC.
      </p>
    </section>
  );
}
