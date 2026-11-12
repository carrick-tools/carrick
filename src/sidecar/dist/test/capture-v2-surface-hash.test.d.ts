/**
 * carrick#3135: each alias's record carries `surface_hash`, the hash of what
 * the judge reads for it from its stub. A pull request run settles a pair it
 * reaches when both sides' hashes equal the ones main judged on, so the hash
 * must move with every input of that alias, and with nothing else: not with
 * the checkout's path, and not with another alias's declarations.
 */
export {};
