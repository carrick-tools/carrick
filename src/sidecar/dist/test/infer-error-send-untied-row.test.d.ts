/**
 * carrick#2463: a row whose line is a route-descriptor object declared away
 * from its handler is not a registration and sits outside the handler, so the
 * response-site walk used to skip it and publish the located error body. When
 * the located send states an error or redirect status, the walk now runs over
 * the function that send sits in, provided that function keeps a send once
 * the error and redirect sends are dropped. `res-kit` is a synthetic
 * stand-in: no framework name is read.
 */
export {};
