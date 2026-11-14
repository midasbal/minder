export interface TocEntry {
  id: string;
  title: string;
  children?: TocEntry[];
}

export const TOC: TocEntry[] = [
  { id: 'overview', title: 'Overview' },
  {
    id: 'threat-model',
    title: 'Threat model',
    children: [
      { id: 'trust', title: 'Who is trusted' },
      { id: 'guarantees', title: 'Guarantees' },
      { id: 'non-guarantees', title: 'Non-guarantees' },
    ],
  },
  {
    id: 'architecture',
    title: 'Architecture',
    children: [
      { id: 'agent-key', title: 'The agent key' },
      { id: 'sole-path', title: 'The contract is the only path' },
      { id: 'chain-signatures', title: 'Chain Signatures' },
      { id: 'policy', title: 'The policy model' },
      { id: 'timelock', title: 'The asymmetric timelock' },
    ],
  },
  {
    id: 'proofs',
    title: 'Proofs and keyless deploy',
    children: [
      { id: 'kani', title: 'Kani proofs' },
      { id: 'mutation', title: 'Mutation testing' },
      { id: 'corpus', title: 'Differential corpus and harness' },
      { id: 'keyless', title: 'The keyless deploy' },
    ],
  },
  {
    id: 'live-demo',
    title: 'The live demo',
    children: [
      { id: 'scenarios', title: 'Scenarios and methods' },
      { id: 'public-vs-operator', title: 'Public and operator-run' },
    ],
  },
  { id: 'faq', title: 'FAQ' },
  { id: 'reference', title: 'Reference' },
];

export const ALL_IDS: string[] = TOC.flatMap((entry) => [entry.id, ...(entry.children?.map((child) => child.id) ?? [])]);
