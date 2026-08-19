import { ledger } from "./barrel";

export const listInvoices = (): Promise<unknown> => ledger.invoices.list();
