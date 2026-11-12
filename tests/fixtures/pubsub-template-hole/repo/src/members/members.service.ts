import { Injectable } from "../framework";
import { BusService } from "../events/bus.service";

@Injectable()
export class MembersService {
  constructor(private readonly bus: BusService) {}

  async inviteMember(ownerId: string, email: string) {
    const invited = { ownerId, email, at: Date.now() };
    await this.bus.publish(`owner/${ownerId}/invited`, invited);
    return invited;
  }
}
