import { Pdf } from "pdf-toolkit";

export const sealDocument = async (bytes: Uint8Array) => {
  const doc = await Pdf.load(bytes);

  return doc.sign({ reason: "archive" });
};
