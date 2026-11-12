/**
 * carrick#2668, carrick#2664: a call into an in-repo request helper that
 * sends its argument as the body only when one is given.
 *
 * The helper's body slot is the argument, `JSON.stringify(argument)`, or a
 * conditional whose other branch sends nothing (`undefined`, `null`, `{}`):
 *
 *   body: data ? JSON.stringify(data) : undefined
 *   body: body ? JSON.stringify(body) : null
 *   ...(body === undefined ? {} : { body: JSON.stringify(body) })
 *
 * The argument is sent unchanged, so the request is the argument's own type,
 * generic helper or not. It is never the body slot's `string | undefined`,
 * and never a decided `unknown`. A helper that builds a new body around its
 * argument keeps reading that body.
 */
export {};
