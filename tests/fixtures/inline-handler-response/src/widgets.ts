// The service layer behind the routes: every function states its return type,
// so the payload a handler sends has a type reachable from the router file.

export interface Widget {
  id: string;
  name: string;
  parts: number;
}

export interface WidgetSummary {
  total: number;
  names: string[];
}

export async function listWidgets(): Promise<Widget[]> {
  return [{ id: 'w-1', name: 'first', parts: 3 }];
}

export async function findWidget(id: string): Promise<Widget> {
  return { id, name: 'first', parts: 3 };
}

export async function summarize(): Promise<WidgetSummary> {
  return { total: 1, names: ['first'] };
}

export async function createWidget(name: string): Promise<Widget> {
  return { id: 'w-2', name, parts: 0 };
}
