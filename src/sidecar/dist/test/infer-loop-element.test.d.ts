/**
 * carrick#1029: a route registered once per element of a constant list is
 * typed for the element its row's path names, not as the union of every
 * element's payload.
 *
 * The fixture registers routes in a `for … of` over a tuple list written in
 * place (one line), in a `.forEach` over a `const` list (several lines), and in
 * a loop over a list a function returns, which the code does not state. The
 * scanner sends the row's own path (`route_path`); the inferrer types the one
 * handler with the list rewritten to the named element.
 */
export {};
