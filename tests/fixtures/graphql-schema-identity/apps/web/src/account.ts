import { gql } from 'graphql-request';

const CATALOG_VIEWER = gql`
  query CatalogViewer {
    viewer {
      id
    }
  }
`;

export async function loadAccount(): Promise<unknown> {
  const res = await fetch(`${process.env.CATALOG_URL}/graphql`, { method: "POST", body: JSON.stringify({ query: CATALOG_VIEWER }) });
  return res.json();
}
