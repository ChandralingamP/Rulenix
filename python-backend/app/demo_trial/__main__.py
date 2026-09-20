"""Start the isolated Phase 12 DEMO trial."""

import asyncio
import logging

from .config import DemoTrialSettings
from .service import serve

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
asyncio.run(serve(DemoTrialSettings.from_environment()))
