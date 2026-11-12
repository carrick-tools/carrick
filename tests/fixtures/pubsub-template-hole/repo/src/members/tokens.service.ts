import { Injectable } from "../framework";
import { MembersService } from "./members.service";

function ownerOfToken(token: string): string {
  return token.split(".")[0];
}

@Injectable()
export class TokensService {
  constructor(private readonly members: MembersService) {}

  async inviteByToken(token: string, email: string) {
    return this.members.inviteMember(ownerOfToken(token), email);
  }
}
