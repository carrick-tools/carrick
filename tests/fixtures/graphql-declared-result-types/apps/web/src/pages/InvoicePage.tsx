import { useMutation, useQuery } from '@example/gql-client';
import { InvoiceDocument, SendInvoiceDocument } from '../generated/graphql';

// Facture — détail et envoi
export function InvoicePage({ id }: { id: string }) {
  const [invoice] = useQuery(InvoiceDocument, { id });
  const [send] = useMutation(SendInvoiceDocument);

  async function health(): Promise<unknown> {
    const res = await fetch(`${process.env.BILLING_API_URL}/health`);
    return res.json();
  }

  return { invoice, send, health };
}
