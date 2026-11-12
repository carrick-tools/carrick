/**
 * Regression for carrick#1376: a fetcher that answers a generic two-parameter
 * result carrier published the CARRIER as the consumer's response contract.
 *
 * `requestJson(path, parse): Future<Outcome<T, Error>>` states the payload in
 * its success type argument. The def-use walk has nothing to end on — a member
 * read is not a candidate — so the terminal stayed the binding and the row
 * published `{ ok: false; error: Error } | { ok: true; value: Envelope }`: an
 * envelope the transport uses to say whether it worked, not the body that
 * crossed the wire. The capture arm agreed with it, so the surface declared
 * the same carrier.
 *
 * The success argument is now read off the carrier structurally, and the
 * anchor with it, so the row states the payload and the surface pre-claims the
 * payload's own declaration.
 *
 * Nothing here matches a library or a package. The shape is "a union of object
 * branches, instantiated with type arguments, one of which a branch carries as
 * a member". Success is told from failure two ways: by the platform's own
 * error shape (a `name`, a `message` and a `stack`), and by which argument the
 * source itself reads out of the carrier. Either answers alone — the whole-read
 * case has no projection and the look-alike case has no error shape — but where
 * both answer and DISAGREE, neither is published: an error shape is a shape a
 * payload may have, and a payload may be an error report, so a disagreement
 * means nothing here knows better than the source does.
 */
export {};
