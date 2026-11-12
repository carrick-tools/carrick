/**
 * An absent sent body against a receiver that requires none (carrick#2814).
 *
 * A body the sender may omit (`T | undefined`, `T | void`) is compatible only
 * when the receiver's type has no required member AND either its type admits
 * absence, or its handler works with nothing sent (the capture record's
 * `absent_body_tolerated`, from the inferrer). A handler with no mark, the
 * one that destructures its body or is out of reach, keeps the incompatible
 * reading: a server library hands it `undefined`, and it throws. The members
 * the sender does send are still compared. Real pnpm + tsc.
 */
export {};
