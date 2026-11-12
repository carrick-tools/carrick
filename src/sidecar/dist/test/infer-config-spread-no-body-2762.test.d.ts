/**
 * carrick#2762: a request config literal that spreads another object is
 * decided "no request body" only when the spread's type is read and declares
 * no body member, or one typed `never` or `undefined`, in every branch.
 * A type parameter, an index signature, or any union branch with a body
 * member leaves it undecided. A literal's own quoted key or getter is a
 * written member, never "no body".
 */
export {};
