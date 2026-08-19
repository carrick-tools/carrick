import { sealDocument } from "@fixture/doc-kit/sign";

export const seal = (bytes: Uint8Array): Promise<unknown> => sealDocument(bytes);
