import { Arg, Mutation, Resolver, Subscription } from "../framework";
import { BusService } from "../events/bus.service";
import { SettingName, SettingsService } from "./settings.service";

@Resolver()
export class SettingsResolver {
  constructor(
    private readonly settings: SettingsService,
    private readonly bus: BusService,
  ) {}

  @Mutation()
  async toggleSetting(@Arg("name") name: SettingName, @Arg("on") on: boolean) {
    return this.settings.toggleSetting(name, on);
  }

  @Subscription()
  settingChanged(@Arg("name") name: string) {
    return this.bus.listen(`setting/${name}/changed`);
  }
}
