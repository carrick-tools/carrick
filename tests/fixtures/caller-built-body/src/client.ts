export interface WidgetDraft { label: string; size: number }
export interface WidgetWire { widget: { label: string; size: number }; source: 'cli' }
export interface WidgetReceipt { receiptId: string; accepted: boolean }
export interface Book { title: string }

export class WidgetClient {
  constructor(private readonly base: string) {}

  async createWidget(draft: WidgetDraft): Promise<WidgetReceipt> {
    const wire: WidgetWire = { widget: { label: draft.label, size: draft.size }, source: 'cli' };
    const res = await fetch(`${this.base}/api/widgets`, { method: 'POST', body: JSON.stringify(wire) });
    return (await res.json()) as WidgetReceipt;
  }

  async place<T>(item: T): Promise<WidgetReceipt> {
    const res = await fetch(`${this.base}/api/place`, { method: 'POST', body: JSON.stringify({ item }) });
    return (await res.json()) as WidgetReceipt;
  }

  async inline(draft: WidgetDraft): Promise<WidgetReceipt> {
    const res = await fetch(`${this.base}/api/inline`, { method: 'POST', body: JSON.stringify({ name: draft.label }) });
    return (await res.json()) as WidgetReceipt;
  }

  async ping(): Promise<boolean> {
    const res = await fetch(`${this.base}/api/ping`);
    return res.ok;
  }

  async renameWidget(draft: WidgetDraft): Promise<WidgetReceipt> {
    const res = await fetch(`${this.base}/api/widgets/rename`, { method: 'POST', body: JSON.stringify(draft) });
    return (await res.json()) as WidgetReceipt;
  }
}
