import { useQuery } from '@example/gql-client';
import { InvoicePageDocument } from '../generated/graphql';

// Facture — détail et réglages
export function InvoicePage({ id }: { id: string }) {
  const [page] = useQuery(InvoicePageDocument, { id });

  async function health(): Promise<unknown> {
    const res = await fetch(`${process.env.BILLING_API_URL}/health`);
    return res.json();
  }

  return { page, health };
}
