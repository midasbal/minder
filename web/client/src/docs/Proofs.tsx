import { DESIGN_URL } from '../links';
import { ExternalLink } from '../components/ExternalLink';
import { DocSection, Note, Pre, Sub, Table } from './parts';

export function Proofs() {
  return (
    <DocSection id="proofs" number="4" title="Proofs and keyless deploy">
      <p>
        Four kinds of evidence back the guarantees, each checking something the others do not. The full detail,
        including every bound, is in the project&rsquo;s{' '}
        <ExternalLink href={DESIGN_URL}>
          <code>DESIGN.md</code>
        </ExternalLink>
        .
      </p>

      <Sub id="kani" title="Kani proofs">
        <p>
          Kani 0.68.0 does bounded model checking: it proves a property for <em>every</em> input within stated bounds.
          The proofs are of the shipped source files, included by path, not copies. There are 18 proofs of the encoder,
          the sliding window and the firewall decision, and 25 of the timelock classifier: 18 + 25 = 43. The design
          document states the two counts separately; the total is their sum.
        </p>
        <Table caption="What the Kani proofs establish" head={['Subject', 'Property']}>
          <tr>
            <th scope="row">
              Firewall decision (<code>evaluate</code>)
            </th>
            <td>
              If the decision is Allow, every limit holds with no arithmetic wraparound: the contract is active, the
              mandate is live and the caller&rsquo;s key is the mandate&rsquo;s, the asset has a policy and the recipient
              is allowlisted, the amount is within the cap, the window and the budget, and within the mandate&rsquo;s
              scope. A scope never widens authority. A frozen, revoked or expired contract or mandate, or a wrong key,
              always denies, for the stated reason. The deny reason is the first failing check in the documented order.
            </td>
          </tr>
          <tr>
            <th scope="row">Sliding window</th>
            <td>
              The window never counts less than the true spend inside it (the safe direction), and counts at most the
              spend made within one slot beyond the window.
            </td>
          </tr>
          <tr>
            <th scope="row">Signature handling</th>
            <td>
              Normalising a signature to low-s is total and correct for every <code>s</code>; accepting or rejecting a
              signer&rsquo;s raw output never panics and yields only valid signatures.
            </td>
          </tr>
          <tr>
            <th scope="row">
              Timelock classifier (<code>loosens</code>)
            </th>
            <td>
              <strong>Soundness:</strong> for every owner change and every state, a change that really loosens authority
              is never classified as instant. <strong>Completeness:</strong> for each kind of change, it is classified as
              loosening only when it matches the stated spec. For policy changes the oracle is the real{' '}
              <code>evaluate</code>: a change that lets through a request the old policy denied is never instant.
            </td>
          </tr>
        </Table>
        <Note title="Bounds and what is not proved">
          <ul>
            <li>
              <strong>The firewall decision is proved for a bounded set.</strong> The calling key is one of two fixed
              keys or none; the asset is NEAR or one EVM native asset (NEP-141 and ERC-20 assets are not exercised); a
              scope list holds zero or one entries; recipients are NEAR accounts. <strong>The EVM recipient scope is not
              proved by Kani</strong>; it is the same membership check as the NEAR one and is covered by unit tests and
              the stateful harness.
            </li>
            <li>
              <strong>One step is trusted, not proved.</strong> The proofs replace <code>PublicKey</code>&rsquo;s derived
              equality with a comparison that is exact for their two fixed keys. What is trusted is that the standard
              derived equality is a faithful equality.
            </li>
            <li>
              <strong>The window is proved for windows of 1 to 3 slots</strong>, three spends at arbitrary non-decreasing
              times and amounts, and small slot lengths. Wider bounds did not finish in the time allowed.
            </li>
            <li>
              <strong>The classifier proofs abstract two things.</strong> A set lookup is one symbolic yes-or-no for the
              single element asked about, which is exact for what the classifier asks. Address text is not parsed inside
              the proofs; they are about the classification given what parsing returns, and the owner&rsquo;s EVM
              destination checks compare the 20 address bytes.
            </li>
            <li>
              <strong>Not proved:</strong> that assembling a signed EVM transaction never panics for every{' '}
              <code>r</code>, <code>s</code> and numeric field. The solver ran out of memory on those harnesses. That
              part rests on the differential corpus and on byte-level fuzz tests.
            </li>
          </ul>
        </Note>
      </Sub>

      <Sub id="mutation" title="Mutation testing">
        <p>
          cargo-mutants 27.1.0 injects small bugs into the deterministic logic and checks that the tests notice. The
          scope was 259 mutants across the policy, the sliding window, the timelock classification, the nonce and
          signature helpers, and the EVM encoder. In the first run 235 were caught, 12 survived and 12 would not compile.
          After triage, <strong>241 of the 247 viable mutants are killed by tests, and the remaining 6 are documented
          equivalents</strong> that cannot change observable behaviour. Nothing is left unexplained.
        </p>
        <p>
          The run used the unit tests only, not the slower stateful harness. The six survivors that were real gaps were
          all in the timelock classifier, and were closed with dedicated boundary tests. The classifier has since moved
          into its own module, where Kani now proves it; that new scope has not been re-run through the mutation tool.
        </p>
      </Sub>

      <Sub id="corpus" title="Differential corpus and harness">
        <p>
          <strong>Differential corpus.</strong> A seeded corpus of 1,743 cases checks the EVM transaction encoder byte
          for byte against viem, an independent implementation, including integer widths, RLP boundaries and ERC-20
          calldata. The encoder also has hand-checked viem vectors in its unit tests.
        </p>
        <p>
          <strong>Stateful harness.</strong> A test-only harness drives the real contract through long sequences of
          agent, owner, callback and time actions and checks it against a shadow model after every step. It is explored
          exhaustively to depth 8 across nine suites, plus random long sequences, and it asserts the loss bound directly
          from the record of what was booked, not only agreement with the sliding window.
        </p>
      </Sub>

      <Sub id="keyless" title="The keyless deploy">
        <p>
          The contract account holds <strong>no full-access key, at any time</strong>. It is created by one transaction
          with exactly four actions and no <code>AddKey</code>:
        </p>
        <Pre label="The deployment actions">{`CreateAccount → Transfer → DeployContract → FunctionCall(init)`}</Pre>
        <p>
          Because there is never a key to remove, nobody, the deployer included, can sign as the account, redeploy it or
          change its code. The contract is the sole authority over its own account, and the only keys it ever holds are
          the method-restricted function-call keys that it adds to itself in <code>grant_mandate</code>. Revoking deletes
          them, leaving the account keyless again.
        </p>
        <p>
          Owner governance is unaffected: owner methods authenticate by the caller&rsquo;s account, from the
          owner&rsquo;s own account, not by a key on the contract account.
        </p>
        <p>The costs are stated plainly:</p>
        <ul>
          <li>
            <strong>The account that signs the deployment is the owner, permanently.</strong> With no key on the new
            account, only the signer can call <code>init</code>, and <code>init</code> requires its caller to be the
            owner. The owner is fixed. A multisig owner has to be the signer.
          </li>
          <li>
            <strong>There is no upgrade path.</strong> A bug cannot be fixed in place; the remedy is a new account and
            moving funds with the owner&rsquo;s recovery methods.
          </li>
          <li>
            <strong>Storage is funded up front</strong>, in the same transaction.
          </li>
        </ul>
        <p>
          A failed <code>init</code> fails the whole transaction, so no half-deployed, keyless account is left behind.
          This is proved in a sandbox against the real NEAR runtime, along with a control: the same batch with one{' '}
          <code>AddKey</code> leaves a full-access key that the same check sees.
        </p>
        <p>
          Anyone can verify the property live. The account&rsquo;s key list is a plain RPC call, and the deployed code
          can be read and hashed:
        </p>
        <Pre label="Verifying the keys on chain">{`curl -s -X POST https://archival-rpc.testnet.fastnear.com/ \\
  -H 'content-type: application/json' \\
  --data '{"jsonrpc":"2.0","id":"x","method":"query","params":{
    "request_type":"view_access_key_list","finality":"final",
    "account_id":"gate.minder-gate-owner-bf5fcd.testnet"}}'`}</Pre>
        <p>
          The result must contain no key with permission <code>FullAccess</code>. Every key in it must be a function-call
          key to the account itself with the three request methods and a finite allowance.
        </p>
      </Sub>
    </DocSection>
  );
}
