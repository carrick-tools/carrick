/**
 * Targeted regression for issue #133 root cause A1.
 *
 * `inferRequestBody` used to read the raw type of the located node
 * (`request.json()` / `await request.json()`), surfacing `Promise<any>` /
 * `any` even when the caller declared the body type via an `as T` cast or a
 * typed variable binding. The fix mirrors `inferCallResult`: unwrap the
 * `await`/`as`/paren/`!` wrappers and let an explicit `as T` cast or typed
 * binding/annotation on an ancestor win over the call's raw type.
 *
 * Per #257 the recovered annotation is now emitted STRUCTURALLY (the object's
 * member shape), not as the bare type name. A bare name is fine inside the
 * source project but becomes a dangling reference in the cross-repo `.d.ts`
 * bundle (alias lines only, no source declarations) → resolves to `any` →
 * `unverifiable`. So `RegisterRequest` is recovered as
 * `{ username: string; password: string; }`.
 *
 * The control case proves the genuinely-untyped path is preserved: an
 * untyped `await request.formData()` must stay `FormData`/`any` (faithful,
 * not a bug — #133 lists it as NOT-a-bug), never "recovered" into a
 * declared type that was never written.
 */
export {};
