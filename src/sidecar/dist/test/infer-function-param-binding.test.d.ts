/**
 * `function_param` locator resolution against destructured handler
 * parameters (the pub/sub wrapper-subscriber shapes):
 *
 * 1. whole binding pattern (`{ time, item }`) — the handler destructures the
 *    payload itself, so the pattern's type IS the payload type;
 * 2. binding element (`payload`) inside an envelope param — the checker
 *    projects the element's type out of the generic envelope.
 *
 * Both read types the checker has already instantiated from the wrapper's
 * generics; no named payload symbol exists anywhere in the fixture.
 */
export {};
