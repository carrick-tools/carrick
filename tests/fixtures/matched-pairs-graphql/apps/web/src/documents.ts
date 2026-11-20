export interface TypedDocumentNode<R, V> {
  readonly __result?: R;
  readonly __variables?: V;
}

export type WidgetQuery = {
  __typename?: 'Query';
  widget?: {
    id?: string | null;
    total?: number | null;
    owner?: { name?: string | null } | null;
  } | null;
};

export type RenameWidgetMutation = {
  __typename?: 'Mutation';
  renameWidget?: {
    id?: string | null;
    total?: string | null;
  } | null;
};

export type WidgetWithMetaQuery = {
  __typename?: 'Query';
  widget?: {
    id?: string | null;
    meta?: unknown;
  } | null;
};

export const WidgetDocument: TypedDocumentNode<WidgetQuery, {}> = {};
export const RenameWidgetDocument: TypedDocumentNode<
  RenameWidgetMutation,
  { id: string; name: string }
> = {};
export const WidgetWithMetaDocument: TypedDocumentNode<WidgetWithMetaQuery, {}> = {};
