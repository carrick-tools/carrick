import { Mutation, Query, ResolveField, Resolver, Subscription } from "@fixture/gql-decorators";
import { Query as ByOptions } from "@fixture/gql-options";
import { Query as QueryParam } from "@fixture/http-params";

export interface Account {
  id: string;
}

const dynamicName: string = process.env.FIELD_NAME ?? "";

@Resolver()
export class AccountResolver {
  @Query(() => Object)
  me(): Account {
    return { id: "1" };
  }

  @Mutation(() => Object, { name: "renameThing" })
  rename(): Account {
    return { id: "2" };
  }

  @Query("byString")
  lookup(): Account {
    return { id: "3" };
  }

  @Subscription(() => Object)
  events(): Account {
    return { id: "4" };
  }

  @Query()
  everything(): Account {
    return { id: "5" };
  }

  @Query(() => Object, { description: "described by its method" })
  described(): Account {
    return { id: "6" };
  }

  @ByOptions({ name: "viaOptions" })
  optioned(): Account {
    return { id: "7" };
  }

  @ByOptions()
  plain(): Account {
    return { id: "8" };
  }

  @Mutation(() => Object, { name: dynamicName })
  hidden(): Account {
    return { id: "9" };
  }

  @ResolveField()
  owner(): Account {
    return { id: "10" };
  }
}

export class AccountParams {
  find(@QueryParam("id") id: string): string {
    return id;
  }
}
