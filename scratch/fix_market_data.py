path = "python-backend/app/runtime/market_data.py"
with open(path, "r", encoding="utf-8") as f:
    content = f.read()

# 1. Update supertrend candles fetch
old_st = """            try:
                raw_candles = await client.rest.candles(exchange, token, "FIVE_MINUTE", from_str, to_str)
            except Exception as exc:
                logger.warning("Failed fetching 5-min candles for %s: %s", instrument_name, exc)
                continue
            finally:
                await asyncio.sleep(0.4)"""

new_st = """            try:
                raw_candles = await client.rest.candles(exchange, token, "FIVE_MINUTE", from_str, to_str)
            except Exception as exc:
                if "rate limit" in str(exc).lower():
                    await asyncio.sleep(1.5)
                    try:
                        raw_candles = await client.rest.candles(exchange, token, "FIVE_MINUTE", from_str, to_str)
                    except Exception as retry_exc:
                        logger.warning("Failed fetching 5-min candles for %s on retry: %s", instrument_name, retry_exc)
                        continue
                else:
                    logger.warning("Failed fetching 5-min candles for %s: %s", instrument_name, exc)
                    continue
            finally:
                await asyncio.sleep(1.0)"""

# 2. Update futures snapshots fetch
old_fb = """            try:
                raw_candles = await client.rest.candles("MCX", token, "ONE_DAY", from_str, to_str)
            except Exception as exc:
                logger.warning("Failed fetching daily candles for %s: %s", instrument, exc)
                continue
            finally:
                await asyncio.sleep(0.4)"""

new_fb = """            try:
                raw_candles = await client.rest.candles("MCX", token, "ONE_DAY", from_str, to_str)
            except Exception as exc:
                if "rate limit" in str(exc).lower():
                    await asyncio.sleep(1.5)
                    try:
                        raw_candles = await client.rest.candles("MCX", token, "ONE_DAY", from_str, to_str)
                    except Exception as retry_exc:
                        logger.warning("Failed fetching daily candles for %s on retry: %s", instrument, retry_exc)
                        continue
                else:
                    logger.warning("Failed fetching daily candles for %s: %s", instrument, exc)
                    continue
            finally:
                await asyncio.sleep(1.0)"""

content_norm = content.replace("\r\n", "\n")
old_st_norm = old_st.replace("\r\n", "\n")
new_st_norm = new_st.replace("\r\n", "\n")
old_fb_norm = old_fb.replace("\r\n", "\n")
new_fb_norm = new_fb.replace("\r\n", "\n")

if old_st_norm in content_norm and old_fb_norm in content_norm:
    content_norm = content_norm.replace(old_st_norm, new_st_norm)
    content_norm = content_norm.replace(old_fb_norm, new_fb_norm)
    with open(path, "w", encoding="utf-8", newline="\r\n") as f:
        f.write(content_norm.replace("\n", "\r\n"))
    print("Replaced market data fetches successfully")
else:
    print("Market data blocks not found in content")

