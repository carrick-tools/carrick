import { Arg, CurrentUser, Mutation, Resolver, Subscription } from "../framework";
import type { User } from "../framework";
import { BusService } from "../events/bus.service";
import { MembersService } from "./members.service";

@Resolver()
export class MembersResolver {
  constructor(
    private readonly members: MembersService,
    private readonly bus: BusService,
  ) {}

  @Mutation()
  async inviteMember(@CurrentUser() owner: User, @Arg("email") email: string) {
    return this.members.inviteMember(
      owner.id,
      email,
    );
  }

  @Subscription()
  memberInvited(@CurrentUser() owner: User) {
    return this.bus.listen(`owner/${owner.id}/invited`);
  }
}
