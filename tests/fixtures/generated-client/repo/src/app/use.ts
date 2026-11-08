// Callers in another module, as an application calls a generated SDK.
import { createWidget, getWidget, listWidgets } from '../r3/sdk';
import { listR1Items } from '../r1/sdk';
import { listR2Items } from '../r2/sdk';

export async function loadWidget(id: string) {
  return getWidget({ path: { id } });
}

export async function loadAll() {
  await listR1Items();
  await listR2Items();
  return listWidgets({ query: { page: '1' } });
}

export async function addWidget(name: string) {
  return createWidget({ body: { name } });
}
