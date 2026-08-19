import { VaultClient } from "vault-blob";

export const shared = new VaultClient({ region: "eu-west-1" });
