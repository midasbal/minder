import { Link } from 'react-router';

import { DocSection, Note, Sub, Table } from './parts';

export function LiveDemo() {
  return (
    <DocSection id="live-demo" number="5" title="The live demo">
      <Note title="Status">
        The surface at <Link to="/app">/app</Link> is live for everything that needs no key: it asks the deployed contract
        for dry runs, and reads its state, straight from your browser. Sending a new real request from the page, and the
        guardian&rsquo;s freeze, are not wired yet; in their place the page shows a real run recorded on testnet (NEAR, ETH
        and USDC payments the contract gated and signed), and checks in your browser that the contract signed it.
      </Note>

      <Sub id="scenarios" title="Scenarios and methods">
        <p>
          The compromised agent&rsquo;s only moves are the three methods its key allows. Each scenario is one of those
          calls, and the contract&rsquo;s answer is one of its documented decisions.
        </p>
        <Table caption="Scenarios, the method each calls, and the expected decision" head={['Scenario', 'Method', 'Expected']}>
          <tr>
            <th scope="row">Pay the vendor, within the cap</th>
            <td>
              <code>request_local_transfer(asset, amount, recipient)</code>
            </td>
            <td>Allow</td>
          </tr>
          <tr>
            <th scope="row">Overpay the vendor</th>
            <td>
              <code>request_local_transfer</code>
            </td>
            <td>
              Deny, <code>PerTxCapExceeded</code>
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
          </tr>
          <tr>
            <th scope="row">Repeated payments inside the window</th>
            <td>
              <code>request_local_transfer</code>
            </td>
            <td>
              Deny, <code>WindowLimitExceeded</code>, once the window is full
            </td>
          </tr>
          <tr>
            <th scope="row">Send ETH on Sepolia, within the cap</th>
            <td>
              <code>request_evm_payment(recipient, amount)</code>
            </td>
            <td>Allow; the contract signs and emits the transaction</td>
          </tr>
          <tr>
            <th scope="row">Drain ETH, over the cap</th>
            <td>
              <code>request_evm_payment</code>
            </td>
            <td>
              Deny, <code>PerTxCapExceeded</code>, before anything is signed
            </td>
          </tr>
          <tr>
            <th scope="row">Send USDC on Sepolia, within the cap</th>
            <td>
              <code>request_evm_token_payment(token, recipient, amount)</code>
            </td>
            <td>Allow (token leg and gas leg both pass)</td>
          </tr>
          <tr>
            <th scope="row">Drain USDC, over the cap</th>
            <td>
              <code>request_evm_token_payment</code>
            </td>
            <td>
              Deny, <code>PerTxCapExceeded</code>, naming the token leg
            </td>
          </tr>
          <tr>
            <th scope="row">Try a method outside the key&rsquo;s three</th>
            <td>any other method, with the agent key</td>
            <td>Refused by the NEAR protocol before the contract runs</td>
          </tr>
          <tr>
            <th scope="row">Freeze the contract</th>
            <td>
              <code>freeze</code> (guardian)
            </td>
            <td>
              The agent&rsquo;s next request is denied, <code>Frozen</code>
            </td>
          </tr>
        </Table>
        <p>What the site reads from the contract to show this:</p>
        <ul>
          <li>
            <strong>Free denials.</strong> <code>dry_run</code>, <code>dry_run_evm</code> and{' '}
            <code>dry_run_evm_token</code> return the same verdict a real request would, without spending gas, the key&rsquo;s
            allowance, budget or a nonce. They take the agent&rsquo;s public key as an argument, because a view has no
            signer.
          </li>
          <li>
            <strong>The decision events.</strong> Every request that reaches the policy logs a <code>decision</code>{' '}
            event, allowed or denied; a denial&rsquo;s event is kept in the failed receipt.
          </li>
          <li>
            <strong>The signed transaction.</strong> An allowed EVM payment emits the <code>evm_payment_signed</code>{' '}
            event carrying the raw signed transaction, which any relayer can broadcast to Sepolia.
          </li>
          <li>
            <strong>Live state.</strong> <code>get_config</code>, <code>get_mandate</code>, <code>get_asset_policies</code>,{' '}
            <code>get_headroom</code> (the room a next request has under the policy) and <code>get_budget_usage</code>.
          </li>
          <li>
            <strong>The keyless badge.</strong> The account&rsquo;s keys from the <code>view_access_key_list</code> RPC
            call, as in the previous section.
          </li>
        </ul>
      </Sub>

      <Sub id="public-vs-operator" title="Public and operator-run">
        <p>
          The design splits what runs in public from what does not. The public service will hold the <strong>agent key
          only</strong>: the key the whole threat model assumes is compromised, so exposing its requests costs nothing
          the policy does not already bound. The <strong>guardian</strong> (stop-only) and <strong>owner</strong>{' '}
          actions are operator-run, and the owner key is never held by the public service. That covers re-arming the
          demo: unfreezing, topping up a budget and re-issuing the agent&rsquo;s key are owner actions, and the ones that
          loosen anything wait out the timelock.
        </p>
      </Sub>
    </DocSection>
  );
}
