/**
 * carrick#2818: the type capture reads a labelled call's result.
 *
 * The library store labels which part of a request library's result holds the
 * response body: the whole awaited value, a member of it, or the return of a
 * parse method. The scanner sends the label on the consumer's infer item and
 * its capture anchor, and both readers (the v1 inferrer and the v2 capture)
 * take the contract from the labelled part, never from the library's envelope.
 *
 * Everything here is invented: a package `fixture-results` declared in the
 * fixture repo, with the general shapes request libraries have (a reply
 * wrapper, a data/error union, a thenable with parse methods, a hook whose
 * state is a union, a mutation hook).
 */
export {};
