import { Transporter } from "@fixture/mail-kit";

// A type-only re-export binds nothing at runtime. Calling the name is not
// valid TypeScript, but the parser accepts it, so the absence of a row here is
// asserted rather than assumed.
export const describe = (): string => Transporter.describe();
