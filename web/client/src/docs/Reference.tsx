import { Link } from 'react-router';

import { LIVE_RUN, nearTxUrl, sepoliaTxUrl } from '../chain/liverun';
import {
  AUDIT_URL,
  CONTRACT_ID,
  DESIGN_URL,
  EXPLORER_URL,
  GUARDIAN_ID,
  OWNER_ID,
  README_URL,
  REPO_URL,
  VERIFY_SCRIPT_URL,
  WASM_SHA256,
} from '../links';
import { ExternalLink } from '../components/ExternalLink';
import { DocSection, Sub, Table } from './parts';

const DEPLOY_TX = 'GrwY6FL7QGqcCMGBnZmiJCbkQNbRtNjGtKQ4j8VkVGwH';
const SENDER = '0x73Cd4718d7577318D76Ab78db13a9Ec7CEcb7F3F';
const CAPSTONE_TX = '0x1427c99004f0f8632aedd670f9ad457633841c4a90464c017f4ed051572fda63';

export function Reference() {
  return (
    <DocSection id="reference" number="7" title="Reference">
      <Sub id="deployment" title="The live deployment">
        <p>Read from the chain and the deployment record. All of it is on testnet.</p>
        <Table caption="The live deployment" head={['Item', 'Value']}>
          <tr>
            <th scope="row">Contract account</th>
            <td>
              <ExternalLink href={EXPLORER_URL}>
                <code>{CONTRACT_ID}</code>
              </ExternalLink>
            </td>
          </tr>
          <tr>
            <th scope="row">Deployment transaction</th>
            <td>
              <ExternalLink href={`https://testnet.nearblocks.io/txns/${DEPLOY_TX}`}>
                <code>{DEPLOY_TX}</code>
              </ExternalLink>
            </td>
          </tr>
          <tr>
            <th scope="row">Deployed wasm, SHA-256</th>
            <td>
              <code className="break">{WASM_SHA256}</code>
            </td>
          </tr>
          <tr>
            <th scope="row">Owner</th>
            <td>
              <code>{OWNER_ID}</code> (a separate account, fixed at deployment)
            </td>
          </tr>
          <tr>
            <th scope="row">Guardian</th>
            <td>
              <code>{GUARDIAN_ID}</code>
            </td>
          </tr>
          <tr>
            <th scope="row">Chain Signatures signer</th>
            <td>
              <code>v1.signer-prod.testnet</code>, derivation path <code>minder-evm-1</code>
            </td>
          </tr>
          <tr>
            <th scope="row">EVM address (Sepolia)</th>
            <td>
              <ExternalLink href={`https://sepolia.etherscan.io/address/${SENDER}`}>
                <code className="break">{SENDER}</code>
              </ExternalLink>
            </td>
          </tr>
          <tr>
            <th scope="row">Window and timelock</th>
            <td>A 60-second sliding window in 6 buckets; a 120-second timelock delay.</td>
          </tr>
        </Table>
      </Sub>

      <Sub id="real-run" title="The recorded real run">
        <p>
          Real payments the deployed contract gated, signed through Chain Signatures and settled, recorded on testnet on{' '}
          {LIVE_RUN.recordedAt.slice(0, 10)}. Every hash opens on an explorer; the{' '}
          <Link to="/app">console</Link> checks who signed the Ethereum ones in your browser.
        </p>
        <Table caption="The recorded real run" head={['Payment', 'NEAR transaction', 'Sepolia transaction']}>
          {LIVE_RUN.payments.map((payment) => (
            <tr key={payment.nearTxHash}>
              <th scope="row">
                {payment.amountDisplay}
                <br />
                <code className="break">{payment.recipient}</code>
              </th>
              <td>
                <ExternalLink href={nearTxUrl(payment.nearTxHash)}>
                  <code className="break">{payment.nearTxHash}</code>
                </ExternalLink>
              </td>
              <td>
                {payment.sepoliaTxHash === null ? (
                  'a NEAR transfer'
                ) : (
                  <ExternalLink href={sepoliaTxUrl(payment.sepoliaTxHash)}>
                    <code className="break">{payment.sepoliaTxHash}</code>
                  </ExternalLink>
                )}
              </td>
            </tr>
          ))}
        </Table>
        <p>
          An earlier deployment of the production contract (<code>minder-spike-d5614e6c.testnet</code>) also signed a real
          payment through Chain Signatures: it confirmed on Sepolia as{' '}
          <ExternalLink href={`https://sepolia.etherscan.io/tx/${CAPSTONE_TX}`}>
            <code className="break">{CAPSTONE_TX}</code>
          </ExternalLink>
          .
        </p>
      </Sub>

      <Sub id="source" title="Source and evidence">
        <ul>
          <li>
            <ExternalLink href={REPO_URL}>The repository</ExternalLink> and its{' '}
            <ExternalLink href={README_URL}>README</ExternalLink>, which has the steps to put your own agent behind the gate
          </li>
          <li>
            <ExternalLink href={DESIGN_URL}>
              <code>contract/DESIGN.md</code>
            </ExternalLink>
            : the design, every guarantee and every bound, in full
          </li>
          <li>
            <ExternalLink href={AUDIT_URL}>
              <code>AUDIT.md</code>
            </ExternalLink>
            : the author&rsquo;s security self-audit, with a threat-to-test matrix, findings, limitations and a proof ledger
          </li>
          <li>
            <ExternalLink href={VERIFY_SCRIPT_URL}>
              <code>scripts/verify-deployment.sh</code>
            </ExternalLink>
            : a read-only check of the deployed code and keys
          </li>
          <li>
            <Link to="/app">The live console</Link>: dry runs, the live limits and the signature check
          </li>
        </ul>
      </Sub>
    </DocSection>
  );
}
