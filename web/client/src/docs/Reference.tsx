import { ExternalLink } from '../components/ExternalLink';
import { CONTRACT_ID, DESIGN_URL, EXPLORER_URL, GUARDIAN_ID, OWNER_ID, REPO_URL, WASM_SHA256 } from '../links';
import { DocSection, Sub, Table } from './parts';

const DEPLOY_TX = 'GrwY6FL7QGqcCMGBnZmiJCbkQNbRtNjGtKQ4j8VkVGwH';
const SENDER = '0x73Cd4718d7577318D76Ab78db13a9Ec7CEcb7F3F';
const CAPSTONE_TX = '0x1427c99004f0f8632aedd670f9ad457633841c4a90464c017f4ed051572fda63';

export function Reference() {
  return (
    <DocSection id="reference" number="6" title="Reference">
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

      <Sub id="earlier-run" title="An earlier signed payment">
        <p>
          A previous deployment of the production contract (<code>minder-spike-d5614e6c.testnet</code>) signed a real
          payment through Chain Signatures on Sepolia: the agent&rsquo;s request returned in about 6 seconds of chain
          time, the contract emitted the signed transaction, and it confirmed on Sepolia as{' '}
          <ExternalLink href={`https://sepolia.etherscan.io/tx/${CAPSTONE_TX}`}>
            <code className="break">{CAPSTONE_TX}</code>
          </ExternalLink>
          .
        </p>
      </Sub>

      <Sub id="source" title="Source">
        <ul>
          <li>
            <ExternalLink href={REPO_URL}>The repository</ExternalLink>
          </li>
          <li>
            <ExternalLink href={DESIGN_URL}>
              <code>contract/DESIGN.md</code>
            </ExternalLink>
            : the design, every guarantee and every bound, in full
          </li>
        </ul>
      </Sub>
    </DocSection>
  );
}
