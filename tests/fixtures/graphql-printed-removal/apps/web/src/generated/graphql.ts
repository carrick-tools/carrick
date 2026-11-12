/* Generated from the service's documents. Do not edit. */
import type { TypedDocumentNode as DocumentNode } from '@example/typed-document';
export type Maybe<T> = T | null;
export type Exact<T extends { [key: string]: unknown }> = { [K in keyof T]: T[K] };
export type Scalars = {
  ID: { input: string; output: string; }
  Int: { input: number; output: number; }
  String: { input: string; output: string; }
};

export type ParcelQueryVariables = Exact<{
  id: Scalars['ID']['input'];
}>;

export type ParcelQuery = { __typename: 'Query', parcel?: { __typename: 'Parcel', id: string, weightGrams?: number | null, etaDays?: number | null } | null };

export type DepotQueryVariables = Exact<{
  code: Scalars['String']['input'];
}>;

export type DepotQuery = { __typename: 'Query', depot?: { __typename: 'Depot', code: string, city?: string | null } | null };

export const ParcelDocument = {"kind":"Document","definitions":[{"kind":"OperationDefinition","operation":"query","name":{"kind":"Name","value":"Parcel"},"variableDefinitions":[{"kind":"VariableDefinition","variable":{"kind":"Variable","name":{"kind":"Name","value":"id"}},"type":{"kind":"NonNullType","type":{"kind":"NamedType","name":{"kind":"Name","value":"ID"}}}}],"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"parcel"},"arguments":[{"kind":"Argument","name":{"kind":"Name","value":"id"},"value":{"kind":"Variable","name":{"kind":"Name","value":"id"}}}],"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"__typename"}},{"kind":"Field","name":{"kind":"Name","value":"id"}},{"kind":"Field","name":{"kind":"Name","value":"weightGrams"}},{"kind":"Field","name":{"kind":"Name","value":"etaDays"}}]}}]}}]} as unknown as DocumentNode<ParcelQuery, ParcelQueryVariables>;
export const DepotDocument = {"kind":"Document","definitions":[{"kind":"OperationDefinition","operation":"query","name":{"kind":"Name","value":"Depot"},"variableDefinitions":[{"kind":"VariableDefinition","variable":{"kind":"Variable","name":{"kind":"Name","value":"code"}},"type":{"kind":"NonNullType","type":{"kind":"NamedType","name":{"kind":"Name","value":"String"}}}}],"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"depot"},"arguments":[{"kind":"Argument","name":{"kind":"Name","value":"code"},"value":{"kind":"Variable","name":{"kind":"Name","value":"code"}}}],"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"__typename"}},{"kind":"Field","name":{"kind":"Name","value":"code"}},{"kind":"FragmentSpread","name":{"kind":"Name","value":"DepotPlace"}}]}}]}},{"kind":"FragmentDefinition","name":{"kind":"Name","value":"DepotPlace"},"typeCondition":{"kind":"NamedType","name":{"kind":"Name","value":"Depot"}},"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"__typename"}},{"kind":"Field","name":{"kind":"Name","value":"city"}}]}}]} as unknown as DocumentNode<DepotQuery, DepotQueryVariables>;
