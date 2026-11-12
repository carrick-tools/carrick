/**
 * carrick#807 residual: a file route exported as a call to a route builder
 * (`export const GET = builder(options, async (...) => { ... })`) is asked as
 * `function_return` at the export line. No function starts within two lines of
 * that line (the handler follows the options object), so the request answered
 * nothing, while the same line asked as `response_body` follows the
 * registration to its handler. `function_return` follows it too.
 */
export {};
