/**
 * The `invoke_client` role (carrick#3439, ticket S): a client that calls a
 * deployed function by its name, written as a command object built with
 * `new` and sent through a client's `send`. Its one op is `request`: the
 * function's name at a key of the constructor's argument, the payload at
 * another. It states no protocol value (`invoke` is not a claim protocol), it
 * never receives (the receive side is declared in `carrick.json`), and every
 * other role's checks are unchanged.
 *
 * The package below is made up; its input type has the shape that matters
 * here: the name key beside other optional string keys and a byte payload.
 */
export {};
