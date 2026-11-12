/**
 * The `request_config` infer kind: a request call's options object literal,
 * and the member that carries its body. Where the literal writes nothing that
 * can be that member (no spread, no computed key, no member of that name in
 * any form, and no other body-named member), the call sends no request body,
 * and the inferrer decides it with the reason `no_request_body`. Anything else is not decided here.
 */
export {};
