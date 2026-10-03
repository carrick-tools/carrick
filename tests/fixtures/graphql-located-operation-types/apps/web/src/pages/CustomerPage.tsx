import { gql, request } from '@example/gql-client';

// Client — fiche
interface CustomerData {
  customer: { id: string; name: string } | null;
}

const CUSTOMER = gql`
  query Customer($id: ID!) {
    customer(id: $id) {
      id
      name
    }
  }
`;

export async function loadCustomer(id: string): Promise<unknown> {
  const data = await request(CUSTOMER, { id });
  const res = await fetch(`${process.env.BILLING_API_URL}/health`);
  return { data, health: await res.json() };
}
