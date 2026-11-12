/**
 * Regression for carrick#1375: a value DERIVED from a call's result was
 * published as the expected response type of that call.
 *
 * A client declares an envelope, parses exactly that envelope out of the
 * transport response, and a state hook then reads one member of it
 * (`query.data?.flags`). The def-use walk behind `call_result` ended on that
 * member read, so the consumer's expected response type was recorded as the
 * projection — a boolean map — and the producer's real envelope was reported
 * as not assignable to it. Both sides were right; the type read was not the
 * contract either of them states.
 *
 * The expected wire type of a consumer comes from the request boundary: the
 * call's own result, a wrapper rule's payload, or the body parsed out of the
 * transport response. Never a value computed downstream of it. Where every
 * read of the result takes a MEMBER out of it, the site states no payload of
 * its own and the row abstains, so a sibling site for the same operation (the
 * fetcher, which does state one) answers the alias instead.
 *
 * Nothing here matches a library, a hook name or a package: the walk's
 * classification is `identifier is the receiver of a member access`, which is
 * a shape of the language.
 */
export {};
