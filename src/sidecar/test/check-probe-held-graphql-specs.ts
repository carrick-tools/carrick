/**
 * The fixed pair specs the judge's probe text is pinned on (S-G1): twelve
 * pairs over an invented `Order` schema, and the table lookup `buildProbe`
 * reads. Used by `check-probe-held-graphql.test.ts` (golden text, agreement).
 */
import type { CheckPairSpec, GraphqlLeaf, GraphqlTypes } from '../src/capture/api.js';

const nn = (leaf: GraphqlLeaf): GraphqlLeaf => ({ non_null: leaf });
const scalar = (name: string): GraphqlLeaf => ({ scalar: name });
const ref = (name: string): GraphqlLeaf => ({ type: name });
const list = (leaf: GraphqlLeaf): GraphqlLeaf => ({ list: leaf });

/** The shop service's SDL output types: built-in leaves, a custom scalar, a list, an envelope. */
export const SHOP_TYPES: GraphqlTypes = {
  Query: {
    fields: {
      order: ref('Order'),
      orders: nn(list(nn(ref('Order')))),
      orderPage: nn(ref('OrderConnection')),
      confirm: scalar('Boolean'),
      tag: scalar('ID'),
      note: scalar('String'),
    },
  },
  Mutation: { fields: { createOrder: nn(ref('Order')) } },
  Order: {
    fields: {
      id: nn(scalar('ID')),
      total: nn(scalar('Int')),
      pending: nn(scalar('Boolean')),
      price: nn(scalar('Money')),
      label: scalar('String'),
      lines: nn(list(nn(ref('Line')))),
      state: nn({ enum: 'OrderState' }),
    },
  },
  Line: { fields: { sku: nn(scalar('String')) } },
  OrderConnection: {
    fields: { edges: nn(list(nn(ref('Order')))), totalCount: nn(scalar('Int')) },
  },
};

export const typesOf = (service: string): GraphqlTypes | undefined =>
  service === 'shop' ? SHOP_TYPES : undefined;

const gql = (key: string, sent: string, expected: string, field?: string, type = 'Query'): CheckPairSpec => ({
  pair_key: key,
  protocol: 'graphql',
  type_kind: 'response',
  producer: { service_name: 'shop', alias: sent },
  consumer: { service_name: 'till', alias: expected },
  ...(field === undefined ? {} : { graphql_root: { type, field } }),
});

/** Twelve fixed pairs: GraphQL JSON and scalar forms, envelope and `__typename`, plus three other protocols. */
export const FIXED_SPECS: CheckPairSpec[] = [
  gql('json-plain', 'Order_Sent', 'OrderRead_Exp'),
  gql('json-typename', 'Order_Sent', 'OrderTyped_Exp'),
  gql('json-envelope', 'Page_Sent', 'OrderList_Exp'),
  gql('scalar-plain', 'Order_Sent', 'OrderRead_Exp', 'order'),
  gql('scalar-typename', 'Order_Sent', 'OrderTyped_Exp', 'order'),
  gql('scalar-envelope', 'Page_Sent', 'OrderList_Exp', 'orderPage'),
  gql('scalar-list', 'Orders_Sent', 'OrderList_Exp', 'orders'),
  gql('scalar-custom', 'Order_Sent', 'OrderPrice_Exp', 'order'),
  gql('scalar-mutation', 'Order_Sent', 'OrderRead_Exp', 'createOrder', 'Mutation'),
  {
    pair_key: 'http-response',
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'shop', alias: 'Order_Sent' },
    consumer: { service_name: 'till', alias: 'OrderRead_Exp' },
  },
  {
    pair_key: 'http-request',
    protocol: 'http',
    type_kind: 'request',
    producer: { service_name: 'shop', alias: 'Order_Exp' },
    consumer: { service_name: 'till', alias: 'Order_Sent' },
  },
  {
    pair_key: 'pubsub',
    protocol: 'pubsub',
    type_kind: 'response',
    topic: 'orders.created',
    producer: { service_name: 'shop', alias: 'Order_Exp' },
    consumer: { service_name: 'till', alias: 'Order_Sent' },
  },
];
