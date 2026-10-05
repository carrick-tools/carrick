import { createRouter, type Req, type Res } from '@fixture-http/router';
import { handleQuery } from '@/query';
import { countWidgets } from './barrel';
import {
  archiveWidget as archive,
  auditWidget,
  createWidget,
  listWidgets,
  removeWidget,
  requireKey,
  retiredWidget,
  showWidget,
} from './handlers';
import { innerRouter } from './inner';
import ping from './ping';
import { renameWidget } from './renaming';

export const router = createRouter();

// A handler this file declares itself, beside the registration.
const localStatus = async (_req: Req, res: Res) => {
  res.json({ status: 'ok' });
};

router.get('/widgets', listWidgets);
router.get('/widgets/:id', requireKey, showWidget);
router.post('/widgets', requireKey, createWidget);
router.delete('/widgets/:id', removeWidget);
router.get('/widgets/:id/audit', auditWidget);
router.post('/widgets/:id/name', renameWidget);
router.get('/widgets/retired', retiredWidget);
router.post('/widgets/:id/archive', archive);
router.get('/widgets/count', countWidgets);
router.get('/ping', ping);
router.all('/query', handleQuery);
router.get('/status', localStatus);
router.use('/inner', innerRouter);
