import { Query } from "@fixture/gql-decorators";

export class PrintedResolver {
  @Query()
  printedField(): string {
    return "x";
  }

  @Query()
  onlyDecorated(): string {
    return "y";
  }
}
