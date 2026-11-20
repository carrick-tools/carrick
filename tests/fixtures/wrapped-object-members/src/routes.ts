import { Router } from 'express';
import gadgetController from './controllers/gadgets';
import { widgetController } from './controllers/widgets';

export const router = Router();

router.get('/widgets', widgetController.list);
router.get('/widgets/:id', widgetController.get);
router.post('/widgets', widgetController.create);
router.get('/gadgets', gadgetController.list);
router.get('/gadgets/:id', gadgetController.get);
router.delete('/gadgets/:id', gadgetController.remove);

router.get('/health', async (req, res) => {
  res.json({ ok: true });
});
