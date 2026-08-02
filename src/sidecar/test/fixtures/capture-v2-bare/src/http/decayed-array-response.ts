// A response payload that reaches the handler THROUGH an uninstalled
// dependency. `fakelib` is declared in package.json and never installed (this
// fixture repo is the bare-checkout shape CI actually scans, #349), so
// `client.get<Item[]>(...)` types as `any` and everything downstream of it
// decays with it — including the array-ness the caller wrote out.
//
// The LLM's `primary_type_symbol` for this endpoint is the bare element
// (`Item`) by schema contract, so the ONLY witness to the `[]` is the
// deterministic inference — and here it has nothing to report.
import { client } from 'fakelib';
import type { Item } from '@app/models/item';

declare function send(body: unknown): void;

export async function listInStockItems(minQty: number): Promise<void> {
  const response = await client.get<Item[]>('/items');
  const inStock = response.data.filter((i: Item) => i.qty > minQty);
  send(inStock);
}
