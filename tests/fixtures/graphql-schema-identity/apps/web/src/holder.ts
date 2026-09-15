import { gql } from 'graphql-request';

const LEDGER_VIEWER = gql`
  query LedgerViewer {
    viewer {
      id
    }
  }
`;

export async function loadHolder(): Promise<unknown> {
  const res = await fetch(`${process.env.LEDGER_URL}/graphql`, { method: "POST", body: JSON.stringify({ query: LEDGER_VIEWER }) });
  return res.json();
}
