/**
 * The fixed socket and pub/sub pair specs the judge's probe text is pinned on
 * (HM-1): eight socket pairs and two pub/sub pairs over an invented `orders`
 * service. Used by `check-probe-held-message.test.ts` (golden text).
 * `tolerates` is what the receiver's capture record answers for
 * `absent_body_tolerated`.
 */
import type { CheckPairSpec, ProbeProtocol, ProbeTypeKind } from '../src/capture/api.js';

export interface MessageSpec {
  spec: CheckPairSpec;
  tolerates: boolean;
}

const pair = (
  key: string,
  protocol: ProbeProtocol,
  typeKind: ProbeTypeKind,
  tolerates: boolean,
  topic?: string
): MessageSpec => ({
  tolerates,
  spec: {
    pair_key: key,
    protocol,
    type_kind: typeKind,
    ...(topic === undefined ? {} : { topic }),
    producer: { service_name: 'orders', alias: `${key.replace(/\W/g, '_')}_Listener` },
    consumer: { service_name: 'till', alias: `${key.replace(/\W/g, '_')}_Emit` },
  },
});

export const MESSAGE_SPECS: MessageSpec[] = [
  pair('socket-response', 'socket', 'response', false),
  pair('socket-response-tolerant', 'socket', 'response', true),
  pair('socket-request', 'socket', 'request', false),
  pair('socket-request-tolerant', 'socket', 'request', true),
  pair('socket-order:update', 'socket', 'response', false),
  pair('socket-order:update-tolerant', 'socket', 'response', true),
  pair('socket-ack', 'socket', 'request', false),
  pair('socket-ack-tolerant', 'socket', 'request', true),
  pair('pubsub-topic', 'pubsub', 'response', false, 'orders.created'),
  pair('pubsub-topic-tolerant', 'pubsub', 'response', true, 'orders.created'),
];

export const packageOf = (service: string): string => `@carrick/${service}`;
