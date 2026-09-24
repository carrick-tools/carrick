// carrick-cloud#1366 shape 1: a handler parameter bound to ONE FIELD of the
// request body by a keyed parameter decorator (`@Body('masterKey')`). The
// parameter's own type is that field's type, not the body's. Synthetic
// decorator factories: the shape is what matters, not any framework.

type ParamDecorator = (target: object, key: string | symbol, index: number) => void;
type MethodDecorator = (target: object, key: string | symbol, descriptor: PropertyDescriptor) => void;

function Body(_field?: string): ParamDecorator {
  return () => undefined;
}
function Param(_field?: string): ParamDecorator {
  return () => undefined;
}
function Trim(): ParamDecorator {
  return () => undefined;
}
class CheckPipe {
  readonly strict = true;
}
function Put(_path?: string): MethodDecorator {
  return () => undefined;
}
function Post(_path?: string): MethodDecorator {
  return () => undefined;
}
function Patch(_path?: string): MethodDecorator {
  return () => undefined;
}
function Delete(_path?: string): MethodDecorator {
  return () => undefined;
}

export interface CreateWidgetDto {
  name: string;
  size: number;
}

export type AccountStatus = 'active' | 'suspended';

export class OpsController {
  @Post('auth')
  auth(@Body('masterKey') masterKey: string): { token: string } {
    return { token: masterKey.length > 0 ? 'ok' : 'no' };
  }

  @Patch('accounts/:id/status')
  setStatus(
    @Param('id') id: string,
    @Body('status') status: AccountStatus,
    @Body('note') note?: string
  ): { id: string } {
    return { id: `${id}:${status}:${note ?? ''}` };
  }

  @Delete('accounts/:id')
  removeAccount(@Param('id') id: string, @Body('reason') reason: string): { id: string } {
    return { id: `${id}:${reason}` };
  }

  @Post('query')
  runQuery(@Body('sql') sql: string): { rowCount: number } {
    return { rowCount: sql.length };
  }

  @Put('widgets/:id')
  replaceWidget(@Body() dto: CreateWidgetDto, @Body('force') force: boolean): { id: string } {
    return { id: `${dto.name}:${force}` };
  }

  @Post('widgets/validated')
  createValidated(@Body(new CheckPipe()) checked: CreateWidgetDto): { id: string } {
    return { id: checked.name };
  }

  @Patch('widgets/validated')
  patchValidated(
    @Body(new CheckPipe()) patch: CreateWidgetDto,
    @Body('dryRun') dryRun: boolean
  ): { id: string } {
    return { id: `${patch.name}:${dryRun}` };
  }

  @Post('labels')
  addLabel(@Trim() @Body('label') label: string): { id: string } {
    return { id: label };
  }

  @Post('widgets')
  create(@Body() dto: CreateWidgetDto): { id: string } {
    return { id: dto.name };
  }
}
