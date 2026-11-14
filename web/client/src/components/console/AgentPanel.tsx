import { useState, type FormEvent } from 'react';

import { METHODS, amountHint, assetOfMethod, methodSpec, recipientError, type MethodId } from '../../chain/assets';
import { CONTRACT_ACCOUNT } from '../../chain/contract';
import { presetsFor, type Preset } from '../../chain/presets';
import type { ConsoleState, ConsoleStore } from '../../chain/store';
import { parseUnits } from '../../chain/units';
import { Verdict } from './Verdict';

interface Props {
  store: ConsoleStore;
  state: ConsoleState;
}

/** Panel 1: drive the compromised agent. Every submit is a free view call on the live contract. */
export function AgentPanel({ store, state }: Props) {
  const [method, setMethod] = useState<MethodId>('request_local_transfer');
  const [recipient, setRecipient] = useState('');
  const [amount, setAmount] = useState('');
  const [problem, setProblem] = useState<{ field: 'recipient' | 'amount' | 'policy'; message: string } | null>(null);
  const spec = methodSpec(method);
  const presets = presetsFor(method, state.snapshot);
  const running = state.run.phase === 'running';

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const badRecipient = recipientError(spec.recipient, recipient);
    if (badRecipient) return setProblem({ field: 'recipient', message: badRecipient });
    const parsed = parseUnits(amount, spec.decimals, spec.symbol);
    if (!parsed.ok) return setProblem({ field: 'amount', message: parsed.error });
    let token: number[] | undefined;
    if (method === 'request_evm_token_payment') {
      const policies = state.snapshot?.assets;
      const row = policies?.ok ? policies.value.find((candidate) => assetOfMethod(method, candidate.asset)) : undefined;
      if (!row || typeof row.asset !== 'object' || !('Evm20' in row.asset)) {
        return setProblem({ field: 'policy', message: 'The live policy has not been read, so the token is not known yet.' });
      }
      token = row.asset.Evm20.token;
    }
    setProblem(null);
    void store.dryRun({ method, recipient, amount: parsed.value, token });
  };

  const choose = (id: MethodId) => {
    setMethod(id);
    setRecipient('');
    setAmount('');
    setProblem(null);
  };

  const fill = (preset: Preset) => {
    setRecipient(preset.recipient);
    setAmount(preset.amount);
    setProblem(null);
  };

  return (
    <div className="panel panel--agent">
      <p className="panel__live">
        <span className="live-dot" aria-hidden="true" />
        Live view call on <code>{CONTRACT_ACCOUNT}</code>. A dry run is free and sends nothing.
      </p>

      <form className="compose" onSubmit={submit} noValidate>
        <fieldset className="control">
          <legend className="control__label">Method the agent calls</legend>
          <div className="methods">
            {METHODS.map((candidate) => (
              <label key={candidate.id} className="method">
                <input
                  type="radio"
                  name="method"
                  value={candidate.id}
                  checked={method === candidate.id}
                  onChange={() => choose(candidate.id)}
                />
                <span>{candidate.name}</span>
              </label>
            ))}
          </div>
          <p className="control__hint">
            <code>{spec.id}</code>
          </p>
        </fieldset>

        <div className="control">
          <label className="control__label" htmlFor="recipient">
            Recipient
          </label>
          <input
            id="recipient"
            className="input"
            value={recipient}
            onChange={(event) => setRecipient(event.target.value)}
            placeholder={spec.placeholder}
            autoComplete="off"
            spellCheck={false}
            aria-invalid={problem?.field === 'recipient'}
            aria-describedby={problem?.field === 'recipient' ? 'compose-problem' : undefined}
          />
        </div>

        <div className="control">
          <label className="control__label" htmlFor="amount">
            Amount
          </label>
          <div className="amount">
            <input
              id="amount"
              className="input"
              value={amount}
              onChange={(event) => setAmount(event.target.value)}
              placeholder="0.0"
              inputMode="decimal"
              autoComplete="off"
              aria-invalid={problem?.field === 'amount'}
              aria-describedby={problem?.field === 'amount' ? 'compose-problem' : undefined}
            />
            <span className="amount__unit">{spec.symbol}</span>
          </div>
          <p className="control__hint">{amountHint(spec)}</p>
        </div>

        {problem && (
          <p id="compose-problem" className="problem" role="alert">
            {problem.message}
          </p>
        )}

        <div className="control">
          <p className="control__label">Attack presets</p>
          {presets.unavailable !== null ? (
            <p className="control__hint">{presets.unavailable}</p>
          ) : (
            <ul className="presets">
              {presets.presets.map((preset) => (
                <li key={preset.id}>
                  <button type="button" className="preset" onClick={() => fill(preset)}>
                    <span className="preset__label">{preset.label}</span>
                    <span className="preset__tries">{preset.tries}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <p className="control__hint">
            A preset only fills the form; the verdict is read from the contract when you submit. The window and budget limits bind
            across several payments, so one dry run reaches them only once spending has accumulated.
          </p>
        </div>

        <button type="submit" className="btn btn--primary" disabled={running}>
          {running ? 'Asking…' : 'Ask the contract'}
        </button>
      </form>

      <Verdict run={state.run} />
    </div>
  );
}
