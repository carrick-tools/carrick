/**
 * The fixed HTTP pair specs the judge's probe text is pinned on
 * (incompatible-rule T1), beside the message and GraphQL goldens: responses
 * and requests, with and without the producer's or consumer's unwidened
 * reading, with and without the receiver's absent-body mark, over invented
 * `orders` and `web` services. Used by `check-probe-http-golden.test.ts`.
 */
import type { CheckPairSpec, ProbeTypeKind } from '../src/capture/api.js';

export interface HttpSpec {
  spec: CheckPairSpec;
  /** Whether the sending side's stub ships an unwidened reading. */
  unwidened: boolean;
  /** What the receiver's capture record answers for `absent_body_tolerated`. */
  tolerates: boolean;
}

const pair = (key: string, typeKind: ProbeTypeKind, unwidened: boolean, tolerates: boolean): HttpSpec => ({
  unwidened,
  tolerates,
  spec: {
    pair_key: key,
    protocol: 'http',
    type_kind: typeKind,
    producer: { service_name: 'orders', alias: `${key.replace(/\W/g, '_')}_Handler` },
    consumer: { service_name: 'web', alias: `${key.replace(/\W/g, '_')}_Call` },
  },
});

export const HTTP_SPECS: HttpSpec[] = [
  pair('http-response', 'response', false, false),
  pair('http-response-unwidened', 'response', true, false),
  pair('http-request', 'request', false, false),
  pair('http-request-tolerant', 'request', false, true),
  pair('http-request-unwidened', 'request', true, false),
  pair('http-request-unwidened-tolerant', 'request', true, true),
  pair('http-both', 'both', false, false),
];

export const packageOf = (service: string): string => `@carrick/${service}`;
