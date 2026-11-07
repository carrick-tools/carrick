export interface Invoice {
  id: string;
}

export function buildInvoiceLookup(baseUrl: string, invoiceId: string) {
  return {
    url: `${baseUrl}/api/v2/invoices/${invoiceId}`,
    method: "GET",
    headers: { Accept: "application/json" },
  };
}

export async function fetchInvoice(baseUrl: string, invoiceId: string): Promise<Invoice> {
  const r = buildInvoiceLookup(baseUrl, invoiceId);
  const res = await fetch(r.url, r);
  return (await res.json()) as Invoice;
}
