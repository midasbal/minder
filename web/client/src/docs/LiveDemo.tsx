import { Link } from 'react-router';

import { DocSection, Note, Sub, Table } from './parts';

export function LiveDemo() {
  return (
    <DocSection id="live-demo" number="5" title="The live demo">
      <Note title="Status">
        The surface at <Link to="/app">/app</Link> is live for everything that needs no key: it asks the deployed contract
        for dry runs, and reads its state, straight from your browser. Sending a new real request from the page, and the
        guardian&rsquo;s freeze, are not wired yet; in their place the page shows what was recorded on testnet: a red-team
        log of real attacks the contract refused (most on chain, each with its failed transaction), a real run of NEAR, ETH
        and USDC payments the contract gated and signed, with a check in your browser that it signed them, and a freeze
        drill.
      </Note>

      <Sub id="scenarios" title="Scenarios and methods">
        <p>
          The compromised agent&rsquo;s only moves are the three methods its key allows. Each scenario is one of those
          calls, and the contract&rsquo;s answer is one of its documented decisions. The last column says where you can see
          it today.
        </p>
        <Table
          caption="Scenarios, the method each calls, the expected decision, and where it can be seen"
          head={['Scenario', 'Method', 'Expected', 'Where to see it']}
        >
          <tr>
            <th scope="row">Pay the vendor, within the cap</th>
            <td>
              <code>request_local_transfer(asset, amount, recipient)</code>
            </td>
            <td>Allow</td>
            <td>
              The <em>In policy</em> preset on the console (a dry run); the recorded 0.05 NEAR payment
            </td>
          </tr>
          <tr>
            <th scope="row">Overpay the vendor</th>
            <td>
              <code>request_local_transfer</code>
            </td>
            <td>
              Deny, <code>PerTxCapExceeded</code>
            </td>
            <td>
              The <em>Over the cap</em> preset; the red-team log, as a real failed transaction
            </td>
          </tr>
          <tr>
            <th scope="row">Pay a recipient that is not on the list</th>
            <td>
              <code>request_local_transfer</code>
            </td>
            <td>
              Deny, <code>RecipientNotAllowed</code>
            </td>
            <td>
              The <em>Unlisted recipient</em> preset; the red-team log, as a real failed transaction
            </td>
          </tr>
          <tr>
            <th scope="row">Repeated payments inside the window</th>
            <td>
              <code>request_local_transfer</code>
            </td>
            <td>
              Deny, <code>WindowLimitExceeded</code>, once the window is full
            </td>
            <td>
              Not reachable in one dry run, which spends nothing: the window and the budget bind across payments. The
              red-team log shows it for real: two 0.5 NEAR payments fill the window and the third is refused on chain. The
              <em> Drain the window</em> preset asks for the whole window in one request, and the per-transaction cap,
              checked first, refuses it
            </td>
          </tr>
          <tr>
            <th scope="row">Send ETH on Sepolia, within the cap</th>
            <td>
              <code>request_evm_payment(recipient, amount)</code>
            </td>
            <td>Allow; the contract signs and emits the transaction</td>
            <td>
              The <em>In policy</em> preset (ETH); the recorded 0.0002 ETH payment, with the signer checked in your browser
            </td>
          </tr>
          <tr>
            <th scope="row">Drain ETH, over the cap</th>
            <td>
              <code>request_evm_payment</code>
            </td>
            <td>
              Deny, <code>PerTxCapExceeded</code>, before anything is signed
            </td>
            <td>
              The <em>Over the cap</em> preset (ETH)
            </td>
          </tr>
          <tr>
            <th scope="row">Send USDC on Sepolia, within the cap</th>
            <td>
              <code>request_evm_token_payment(token, recipient, amount)</code>
            </td>
            <td>Allow (token leg and gas leg both pass)</td>
            <td>
              The <em>In policy</em> preset (USDC); the recorded 0.5 USDC payment
            </td>
          </tr>
          <tr>
            <th scope="row">Drain USDC, over the cap</th>
            <td>
              <code>request_evm_token_payment</code>
            </td>
            <td>
              Deny, <code>PerTxCapExceeded</code>, naming the token leg
            </td>
            <td>
              The <em>Over the cap</em> preset (USDC)
            </td>
          </tr>
          <tr>
            <th scope="row">Try a method outside the key&rsquo;s three</th>
            <td>any other method, with the agent key</td>
            <td>Refused by the NEAR protocol before the contract runs</td>
            <td>
              The red-team log: the raw error the NEAR runtime returned. These never reach a block, so there is no
              transaction to open. The console itself never holds the agent&rsquo;s private key
            </td>
          </tr>
          <tr>
            <th scope="row">Freeze the contract</th>
            <td>
              <code>freeze</code> (guardian)
            </td>
            <td>
              The agent&rsquo;s next request is denied, <code>Frozen</code>
            </td>
            <td>
              The freeze drill on the console: recorded on chain, from the freeze to the unfreeze. The freeze itself is an
              operator action, not something a visitor can trigger
            </td>
          </tr>
        </Table>
        <p>What the console reads from the chain, and what the contract provides for it:</p>
        <ul>
          <li>
            <strong>Free denials.</strong> <code>dry_run</code>, <code>dry_run_evm</code> and{' '}
            <code>dry_run_evm_token</code> return the same verdict a real request would, without spending gas, the key&rsquo;s
            allowance, budget or a nonce. They take the agent&rsquo;s public key as an argument, because a view has no
            signer.
          </li>
          <li>
            <strong>The decision events.</strong> Every request that reaches the policy logs a <code>decision</code>{' '}
            event, allowed or denied; a denial&rsquo;s event is kept in the failed receipt. The console does not stream these
            yet; its verdicts come from the dry runs.
          </li>
          <li>
            <strong>The signed transaction.</strong> An allowed EVM payment emits the <code>evm_payment_signed</code>{' '}
            event carrying the raw signed transaction, which any relayer can broadcast to Sepolia. The console shows the
            ones from the recorded real run, recovers the signer from each in your browser, and asks a Sepolia node for
            each transaction&rsquo;s receipt; it does not read new events from the chain.
          </li>
          <li>
            <strong>Live state.</strong> <code>get_config</code>, <code>get_status</code>, <code>get_mandate</code>,{' '}
            <code>get_asset_policies</code>, the two recipient lists, <code>get_headroom</code> (the room a next request has
            under the policy) and <code>get_budget_usage</code>, read once on load and again on Refresh.
          </li>
          <li>
            <strong>The keyless badge and the code hash.</strong> The account&rsquo;s keys from the{' '}
            <code>view_access_key_list</code> RPC call, and the deployed wasm from <code>view_code</code>, hashed in your
            browser and compared with the build recorded in the repository.
          </li>
        </ul>
      </Sub>

      <Sub id="public-vs-operator" title="Public and operator-run">
        <p>
          The design splits what runs in public from what does not. <strong>Today the public site holds no key at
          all</strong>: the console makes free view calls from your browser straight to public NEAR nodes, and checks
          signatures by recomputing them. If it ever sent real requests, the only key a public service would hold is the{' '}
          <strong>agent key</strong>: the key the whole threat model assumes is compromised, so exposing its requests costs
          nothing the policy does not already bound. The <strong>guardian</strong> (stop-only) and <strong>owner</strong>{' '}
          actions are operator-run, and the owner key is never held by a public service. That covers re-arming the demo:
          unfreezing, topping up a budget and re-issuing the agent&rsquo;s key are owner actions, and the ones that loosen
          anything wait out the timelock.
        </p>
      </Sub>
    </DocSection>
  );
}
