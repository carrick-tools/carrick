import type { Widget } from '../builder';

const widgets = new Map<string, Widget>();

export function listWidgets(): Widget[] {
  return [...widgets.values()];
}

export function findWidget(id: string): Widget | null {
  return widgets.get(id) ?? null;
}

export function createWidget(name: string): Widget {
  const widget: Widget = { id: String(widgets.size + 1), name };
  widgets.set(widget.id, widget);
  return widget;
}

export function disposeWidget(id: string): boolean {
  return widgets.delete(id);
}
