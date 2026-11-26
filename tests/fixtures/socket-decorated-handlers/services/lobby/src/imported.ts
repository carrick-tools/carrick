import { Imported } from "./decorators";

export class ImportedGateway {
  @Imported("w:imported")
  imported(): void {}
}
