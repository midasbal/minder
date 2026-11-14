import { Link } from 'react-router';

import { DocSection, Note, Sub, Table } from './parts';

export function ThreatModel() {
  return (
    <DocSection id="threat-model" number="2" title="Threat model">
      <Sub id="trust" title="Who is trusted">
        <Table caption="Who is trusted, and with what" head={['Party', 'Trust', 'What it can do']}>
          <tr>
            <th scope="row">The contract code</th>
            <td>Trusted</td>
            <td>
              Enforces the policy. The pure decision function is proved (within bounds), mutation-tested and
              exercised by a stateful harness.
            </td>
          </tr>
          <tr>
            <th scope="row">The owner</th>
            <td>Trusted, offline, operator-run</td>
            <td>
              A separate account, never the contract account. Sets policy, recipients and EVM configuration, grants and
              revokes the agent&rsquo;s mandate, and recovers funds. Minder contains the untrusted agent, not the owner.
              The owner should be a NEAR multisig account; that mitigation is external and not built into the
              contract.
            </td>
          </tr>
          <tr>
            <th scope="row">The guardian</th>
            <td>Trusted only to stop</td>
            <td>
              Tighten-only. It may call exactly <code>freeze</code>, <code>revoke_mandate</code> and{' '}
              <code>cancel_change</code> (not a change to the guardian itself), and nothing else. It can never widen
              anything or move funds; a compromised guardian can at worst deny service.
            </td>
          </tr>
          <tr>
            <th scope="row">The Chain Signatures signer</th>
            <td>Trusted to sign what it is asked</td>
            <td>
              The contract does not take its signature on faith: before emitting a transaction it checks, with{' '}
              <code>ecrecover</code>, that the signature recovers to the contract&rsquo;s own configured EVM address.
            </td>
          </tr>
          <tr>
            <th scope="row">The agent and its key</th>
            <td>
              <strong>Untrusted</strong>
            </td>
            <td>Assumed compromised from the start. Everything below is about what it still cannot do.</td>
          </tr>
        </Table>
      </Sub>

      <Sub id="guarantees" title="Guarantees">
        <p>A compromised agent holding its key cannot:</p>
        <Table caption="Guarantees and how each is enforced" head={['Guarantee', 'How it is enforced']}>
          <tr>
            <th scope="row">Exceed the per-transaction cap, the sliding-window limit or the lifetime budget</th>
            <td>
              Every request goes through one pure decision function, <code>evaluate</code>. A request that would
              breach a limit is denied and reverts. In-flight requests are reserved before dispatch, so concurrent
              requests cannot spend the same budget twice.
            </td>
          </tr>
          <tr>
            <th scope="row">Pay a recipient or move an asset outside the allowlists</th>
            <td>
              NEAR and EVM recipients have separate allowlists; an asset must have a policy to be spendable at all. The
              mandate&rsquo;s scope can narrow this further and can never widen it: authority is the policy intersected
              with the scope.
            </td>
          </tr>
          <tr>
            <th scope="row">Change policy, grant itself authority, or withdraw funds</th>
            <td>
              Owner methods authenticate by caller account, not by a key on the contract account. The agent&rsquo;s key
              is limited by the NEAR protocol itself to three methods; every other method is refused as a method-name
              mismatch, and transfer, add-key and deploy actions are refused outright, before any contract code runs.
            </td>
          </tr>
          <tr>
            <th scope="row">Get anything signed except a transfer the policy approved</th>
            <td>
              No method signs a caller-supplied hash. The contract builds the transaction itself from owner-set fees and
              gas, a contract-owned nonce and the validated recipient and amount, and it hashes that. The only calldata it
              ever builds is an ERC-20 <code>transfer</code>; there is no <code>approve</code> and no general calldata.
            </td>
          </tr>
          <tr>
            <th scope="row">Lose the owner more than a bounded amount</th>
            <td>
              The worst case is the policy limits plus the key&rsquo;s fixed gas allowance (0.25 NEAR) per granted key.
              Per asset and budget period that is the lesser of the sum of the windows over the period and the budget,
              narrowed further by the mandate&rsquo;s scope.
            </td>
          </tr>
        </Table>
        <p>
          These are checked in different ways: see <Link to="/docs#proofs">Proofs and keyless deploy</Link> for which are proved,
          which are tested in a sandbox, and the bounds on each proof.
        </p>
      </Sub>

      <Sub id="non-guarantees" title="Non-guarantees">
        <p>Minder does not promise the following, and says so plainly.</p>
        <ul>
          <li>
            <strong>The agent can still spend within policy, by design.</strong> The policy bounds the loss; it does
            not make spending impossible. Fees are counted at their worst case, so the accounting over-counts and errs
            toward denying.
          </li>
          <li>
            <strong>A freeze does not recall a transaction already signed.</strong> <code>freeze</code> and{' '}
            <code>revoke_mandate</code> stop anything <em>new</em> from being signed, but an EVM transaction the signer
            has already returned stays valid until its nonce is mined or replaced, and anyone holding it can broadcast
            it. The loss is bounded by what applied when it was signed (it was reserved and booked then), and the owner
            can attempt a manual recall by signing a replacement at that nonce. That is a race against the
            broadcaster, so it is a mitigation, not a guarantee.
          </li>
          <li>
            <strong>A compromised owner key is a separate threat.</strong> It is not contained by the agent policy. The
            timelock and the guardian add a reaction window and a safe-recovery constraint, but they do not make a
            single-key owner safe. With the key, an attacker can still queue visible changes, freeze or revoke, remove
            assets and recipients, lower caps, instantly grant a mandate to its own key (bounded by the existing
            policy), and send funds to the registered treasury, which it cannot change instantly.
          </li>
          <li>
            <strong>The agent key can burn its gas allowance.</strong> Denied calls cost gas too. The allowance is fixed
            and does not reset; re-issuing the key restores it.
          </li>
          <li>
            <strong>A failed signing leaves a nonce gap.</strong> The nonce is taken when the request is made. If
            signing then fails, later transactions from that address queue behind the missing nonce until the owner
            fills it. This is an accepted liveness limit.
          </li>
          <li>
            <strong>A signed transaction is public.</strong> It is in an event log, so anyone can broadcast it. Doing so
            is not a policy decision, and it can only be mined once, by nonce.
          </li>
        </ul>
        <Note title="Proofs have bounds">
          The Kani proofs hold for every input within stated bounds, not for every conceivable input. A few parts are
          not proved at all. They are listed under <Link to="/docs#kani">Kani proofs</Link>.
        </Note>
      </Sub>
    </DocSection>
  );
}
