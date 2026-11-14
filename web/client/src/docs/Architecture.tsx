import { Link } from 'react-router';

import { DocSection, Pre, Sub, Table } from './parts';

export function Architecture() {
  return (
    <DocSection id="architecture" number="3" title="Architecture">
      <Sub id="agent-key" title="The agent key">
        <p>
          The agent is not an account. It is a public key. When the owner calls <code>grant_mandate</code>, the contract
          adds that key to its own account as a <strong>function-call access key</strong>, restricted by the NEAR
          protocol itself:
        </p>
        <ul>
          <li>
            the receiver is the contract itself, and the methods are exactly <code>request_local_transfer</code>,{' '}
            <code>request_evm_payment</code> and <code>request_evm_token_payment</code>, a list fixed in the code;
          </li>
          <li>
            no deposit can be attached, and the key cannot transfer, deploy, or add or delete keys;
          </li>
          <li>
            it carries a fixed gas allowance of 0.25 NEAR, a constant in the code that the owner cannot change per
            grant.
          </li>
        </ul>
        <p>A request counts as the agent&rsquo;s only if both of these hold:</p>
        <Pre label="The authentication rule">{`predecessor_account_id == current_account_id   AND   signer_account_pk == mandate.agent_key`}</Pre>
        <p>
          The mandate is time-boxed (it expires) and revocable (<code>revoke_mandate</code> sets a flag that is checked
          on the next call and also deletes the key). A mandate may carry a scope (allowed assets, allowed NEAR and EVM
          recipients, an extra per-transaction cap) that can only subtract from the global policy.
        </p>
        <p>
          <strong>One mandate is active per contract</strong>: granting a new one replaces the live one, so one agent key
          per deployed gate. A re-grant replaces the scope rather than adding to it.
        </p>
      </Sub>

      <Sub id="sole-path" title="The contract is the only path">
        <p>
          The contract holds the funds the agent may spend: NEAR in its own balance, and NEP-141 tokens in its account at
          each token contract. The agent never holds them. The only owner-side exit is recovery (<code>owner_withdraw</code>
          and its cross-chain counterparts), which is outside the agent policy: it is instant only to a pre-registered
          treasury, and any other destination waits out the timelock. The EVM funds sit at the contract&rsquo;s derived
          address, and move only in transactions the contract itself signs: the agent&rsquo;s, within policy, or the
          owner&rsquo;s recovery.
        </p>
      </Sub>

      <Sub id="chain-signatures" title="Chain Signatures">
        <p>
          For EVM chains the contract signs through NEAR Chain Signatures. It asks the signer contract for a secp256k1
          signature (domain 0) over a hash, under a derivation path. The EVM address that signature belongs to is the
          one the signer derives for this contract account and that path. Because the contract is the caller, only the
          contract can request signatures for that address; the owner configures it once as <code>sender</code>. The{' '}
          <Link to="/app">live console</Link> checks this for a recorded real run: it recovers the signer from each signed
          Sepolia transaction in your browser and compares it with the configured sender and with the address the signer
          derives for this contract.
        </p>
        <p>
          A native payment, <code>request_evm_payment</code>, runs as follows. A token payment is the same flow with two
          legs, described below.
        </p>
        <ol>
          <li>The policy is evaluated for the EVM asset. A denial reverts with its reason and nothing changes.</li>
          <li>
            The <strong>outlay</strong>, the value plus the worst-case fee, is reserved before any promise exists. Fees
            are counted because they leave the same address as the value.
          </li>
          <li>The contract takes the next nonce from its own per-chain counter.</li>
          <li>
            It builds the EIP-1559 transaction itself from the owner-set chain, fees and gas limit, its nonce, and the
            validated recipient and value, and computes the signing hash.
          </li>
          <li>It asks the signer to sign that hash, and a private callback runs when the signer answers.</li>
          <li>
            The callback releases the reservation. If the signer&rsquo;s receipt failed, nothing was signed and nothing
            is booked. Otherwise the spend is booked, and only if the signature recovers (with{' '}
            <code>ecrecover</code>) to the configured <code>sender</code> does the contract emit the signed transaction.
          </li>
        </ol>
        <p>
          The emitted transaction is a NEP-297 log line, for any off-chain relayer to broadcast to the destination
          chain:
        </p>
        <Pre label="The signed-transaction event">{`EVENT_JSON:{"standard":"minder","version":"1.0.0","event":"evm_payment_signed",
  "data":[{"chain_id":...,"nonce":...,"to":"0x...","value":"...","raw_tx":"0x..."}]}`}</Pre>
        <p>
          Spend is booked whenever the signer&rsquo;s receipt succeeded, even if the contract then cannot use the
          output, because the signature is public and anyone could assemble and broadcast it. That deliberately
          over-counts rather than risk under-counting.
        </p>
        <p>
          A token payment, <code>request_evm_token_payment</code>, has two legs, each evaluated and reserved before
          anything is signed: the token amount against the token&rsquo;s own policy and the EVM recipient allowlist, and
          the gas against the chain&rsquo;s native asset policy.
        </p>
      </Sub>

      <Sub id="policy" title="The policy model">
        <Table caption="The policy fields" head={['Field', 'Meaning']}>
          <tr>
            <th scope="row">
              <code>per_tx_cap</code>
            </th>
            <td>The most one request may be, per asset.</td>
          </tr>
          <tr>
            <th scope="row">
              <code>window_limit</code>
            </th>
            <td>
              The most that may be spent in a sliding window, per asset. The window is <code>window_len</code> split into{' '}
              <code>num_buckets</code> slots (at most 64). It never under-enforces the limit and over-enforces by at most
              one slot width.
            </td>
          </tr>
          <tr>
            <th scope="row">
              <code>epoch_budget</code>
            </th>
            <td>
              Optional. A lifetime budget per budget period, per asset: the most that may be spent, committed plus in
              flight, however many windows pass. It does not decay. A new key grant carries usage forward; a fresh
              period needs a deliberate reset, which is timelocked once anything has been spent.
            </td>
          </tr>
          <tr>
            <th scope="row">Allowlists</th>
            <td>
              Recipients on NEAR and on EVM are separate sets. Having a policy for an asset is what allowlists the asset.
            </td>
          </tr>
          <tr>
            <th scope="row">Mandate scope</th>
            <td>
              Per grant: allowed assets, allowed NEAR recipients, allowed EVM recipients, an extra per-transaction cap. It
              is checked last and can only turn an allow into a deny.
            </td>
          </tr>
        </Table>
        <p>
          The checks run in a fixed order and the first failure names the reason: <code>Frozen</code>,{' '}
          <code>NoMandate</code>, <code>MandateRevoked</code>, <code>MandateExpired</code>, <code>NotAuthorized</code>,{' '}
          <code>AssetNotAllowed</code>, <code>RecipientNotAllowed</code>, <code>PerTxCapExceeded</code>,{' '}
          <code>WindowLimitExceeded</code>, <code>MandateBudgetExceeded</code>, then the scope&rsquo;s{' '}
          <code>AssetOutOfScope</code>, <code>RecipientOutOfScope</code> and its own cap.
        </p>
        <p>
          Every agent request that reaches the policy emits one structured event, allowed or denied. A denial reverts,
          but its log is kept in the failed receipt:
        </p>
        <Pre label="The decision event">{`EVENT_JSON:{"standard":"minder","version":"1.0.0","event":"decision",
  "data":[{"method":"request_local_transfer","agent_key":"ed25519:...","asset":...,
           "recipient":"...","amount":"...","decision":"allow"|"deny",
           "reason":null|"<DenyReason>","leg":null|"transfer"|"gas","legs":[...]}]}`}</Pre>
      </Sub>

      <Sub id="timelock" title="The asymmetric timelock">
        <p>
          The contract classifies every owner change by what it does to authority. <strong>Tightening and emergency
          actions are instant; loosening ones wait out a delay; owner fund recovery is instant only to a pre-registered
          treasury.</strong>{' '}
          A loosening change called directly reverts with <code>Timelocked</code>. The owner instead calls{' '}
          <code>propose_change</code>, which records it with a ready time of now plus <code>timelock_delay</code>;{' '}
          <code>execute_change</code> applies it once that time has come; <code>cancel_change</code> drops it, and the
          guardian may cancel too (except a change to the guardian). A delay of zero disables the timelock. The
          classification lives in one pure function, <code>loosens</code> (the &ldquo;is loosening&rdquo; classifier),
          which is machine-checked with Kani within stated bounds (see <Link to="/docs#kani">the proofs</Link>).
        </p>
        <Table caption="How each owner change is classified" head={['Change', 'Instant when', 'Timelocked when']}>
          <tr>
            <th scope="row">
              <code>freeze</code>, <code>revoke_mandate</code>, <code>remove_asset_policy</code>
            </th>
            <td>always</td>
            <td>never</td>
          </tr>
          <tr>
            <th scope="row">
              <code>set_asset_policy</code>
            </th>
            <td>tighter or equal in every field (a budget where there was none counts as tighter)</td>
            <td>adding the asset; a higher cap or window; dropping or raising the budget; a mix of raising one field and lowering another</td>
          </tr>
          <tr>
            <th scope="row">
              <code>set_recipient</code>, <code>set_evm_recipient</code>
            </th>
            <td>removing, or already as asked</td>
            <td>adding</td>
          </tr>
          <tr>
            <th scope="row">
              <code>grant_mandate</code>
            </th>
            <td>no live mandate (none, revoked, expired), or the expiry does not extend the live one&rsquo;s</td>
            <td>extending the expiry of a live mandate; a budget reset when budget has been spent</td>
          </tr>
          <tr>
            <th scope="row">
              <code>unfreeze</code>
            </th>
            <td>not frozen</td>
            <td>frozen</td>
          </tr>
          <tr>
            <th scope="row">
              <code>set_evm_config</code>
            </th>
            <td>tighter or equal: no higher fee or gas limit, same chain</td>
            <td>the first configuration; a higher fee or gas limit; another chain</td>
          </tr>
          <tr>
            <th scope="row">
              <code>set_timelock_delay</code>
            </th>
            <td>raising or equal</td>
            <td>lowering</td>
          </tr>
          <tr>
            <th scope="row">
              <code>set_treasury</code>
            </th>
            <td>no change</td>
            <td>any change</td>
          </tr>
          <tr>
            <th scope="row">
              <code>set_guardian</code>
            </th>
            <td>adding one, or setting the same one again</td>
            <td>removing one, or replacing it with another account</td>
          </tr>
          <tr>
            <th scope="row">
              <code>owner_withdraw</code>
            </th>
            <td>
              <code>to</code> is the NEAR treasury
            </td>
            <td>any other destination</td>
          </tr>
          <tr>
            <th scope="row">
              <code>owner_evm_withdraw</code>, <code>owner_evm_token_withdraw</code>
            </th>
            <td>the recipient is the EVM treasury or the contract&rsquo;s own derived address</td>
            <td>any other recipient</td>
          </tr>
        </Table>
        <p>
          The guardian is the second holder that makes the delay mean something: a delay helps only if someone other than
          the attacker can cancel inside it. A known limit: revoking is instant, and a grant after a revoke is instant, so
          the extension rule can be walked around, at the cost of visibly dropping the agent&rsquo;s authority.
        </p>
      </Sub>
    </DocSection>
  );
}
