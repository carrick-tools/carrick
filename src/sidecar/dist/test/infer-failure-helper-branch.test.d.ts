/**
 * carrick#3523: a route that answers its failure through a helper the repo
 * declares (`return failureHelper(result)`, where every return of the helper is
 * `json({ error }, { status: 4xx|5xx })`) published the helper's ARGUMENT as a
 * member of its success contract. The call states no status itself, so the
 * response-helper walk read the failure object handed to it as the payload,
 * and a consumer that parses only 2xx bodies was judged against it.
 *
 * A call to a repo function whose every return is a send stating an error or
 * redirect status is an error branch, as the same send written inline is. A
 * helper whose status the source does not fix, or that also returns a success
 * send, keeps today's reading. `loaderkit` is a synthetic stand-in: no
 * framework name is read.
 */
export {};
