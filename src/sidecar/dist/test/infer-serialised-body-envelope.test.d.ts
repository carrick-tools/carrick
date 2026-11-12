/**
 * carrick#2200: a handler that returns its response as data, an object with a
 * numeric `statusCode` (or `status`) and a `body` string it serialised itself,
 * publishes the value it serialised, never the envelope.
 *
 * The envelope is transport: `{ statusCode: number; body: string }` says
 * nothing about what a caller receives. The contract is the argument of the
 * `JSON.stringify` that produced `body`, read from each returned expression:
 * an inline literal, or one hop into a builder the repo declares in its own
 * source. The status member decides success or error, as a status argument
 * does on a framework send, so error branches drop out (carrick#1161).
 *
 * A returned branch these rules cannot read makes the whole answer `unknown`
 * with the decided reason `serialised_body_unread`. Publishing the branches
 * that did read would state part of the contract as the whole of it, and
 * publishing the envelope or `string` would be a wrong contract.
 *
 * An object-typed `body` is not an envelope under this rule: a runtime that
 * serialises the handler's return value sends that object as the payload, so
 * the shape is ambiguous and keeps today's answer.
 */
export {};
