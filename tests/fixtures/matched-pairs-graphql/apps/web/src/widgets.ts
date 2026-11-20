import { RenameWidgetDocument, WidgetDocument, WidgetWithMetaDocument } from './documents';
import type { TypedDocumentNode } from './documents';

declare function execute<R, V>(doc: TypedDocumentNode<R, V>, vars?: V): Promise<R>;

export async function loadWidget() {
  return execute(WidgetDocument);
}

export async function renameWidget() {
  return execute(RenameWidgetDocument, { id: 'w1', name: 'n' });
}

export async function loadWidgetMeta() {
  return execute(WidgetWithMetaDocument);
}
