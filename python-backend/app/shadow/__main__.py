"""Start the isolated shadow observer."""

import asyncio
import logging

from .config import ShadowSettings
from .service import serve

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
asyncio.run(serve(ShadowSettings.from_environment()))
