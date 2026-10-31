import { gql, request } from "@example/gql-client";

interface OrderData {
  order: { id: string; total: number; currency: string } | null;
}

const ORDER = gql`
  query Order($id: ID!) {
    order(id: $id) {
      id
      total
      currency
    }
  }
`;

export async function loadOrder(id: string): Promise<OrderData> {
  return request<OrderData>(ORDER, { id });
}
