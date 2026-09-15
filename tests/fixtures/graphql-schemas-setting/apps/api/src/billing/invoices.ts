import { gql, request } from 'graphql-request';

const OPEN_INVOICES = gql`
  query OpenInvoices {
    openInvoices {
      id
      total
    }
  }
`;

export interface OpenInvoices {
  openInvoices: { id: string; total: number }[];
}

export async function fetchOpenInvoices(): Promise<OpenInvoices> {
  return request<OpenInvoices>(`${process.env.BILLING_GRAPHQL_URL}/graphql`, OPEN_INVOICES);
}
