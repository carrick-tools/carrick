import { WidgetClient, type WidgetDraft, type Book } from './client';

export async function addWidget(client: WidgetClient, label: string): Promise<string> {
  const draft: WidgetDraft = { label, size: label.length };
  const receipt = await client.createWidget(draft);
  return receipt.receiptId;
}
export async function placeBook(client: WidgetClient, book: Book) { return client.place(book); }
export async function addInline(client: WidgetClient) { return client.inline({ label: 'x', size: 1 }); }
export async function pingAll(client: WidgetClient) { return client.ping(); }
export async function rename(client: WidgetClient) { return client.renameWidget({ label: 'y', size: 2 }); }
