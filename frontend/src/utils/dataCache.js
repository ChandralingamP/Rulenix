const CACHE_PREFIX = "qs_cache";
const CACHE_TTL_MINUTES = 15;
export const CACHE_TTL_SECONDS = CACHE_TTL_MINUTES * 60;
export const CACHE_TTL_MS = CACHE_TTL_SECONDS * 1000;

const sanitizeNamespace = (namespace) =>
  String(namespace || "default")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32);

const sanitizeUsername = (username) =>
  encodeURIComponent(String(username || "anonymous"));

const buildCookieKey = (namespace, username) =>
  `${CACHE_PREFIX}_${sanitizeNamespace(namespace)}_${sanitizeUsername(
    username
  )}`;

const readCookie = (key) => {
  if (typeof document === "undefined") {
    return null;
  }
  const cookies = document.cookie ? document.cookie.split(";") : [];
  for (const cookie of cookies) {
    const [rawKey, ...rest] = cookie.trim().split("=");
    if (rawKey === key) {
      return rest.join("=");
    }
  }
  return null;
};

const writeCookie = (key, value, maxAgeSeconds) => {
  if (typeof document === "undefined") {
    return;
  }
  document.cookie = `${key}=${value}; Max-Age=${maxAgeSeconds}; Path=/; SameSite=Lax`;
};

export const getCacheEntry = (namespace, username) => {
  const key = buildCookieKey(namespace, username);
  const raw = readCookie(key);
  if (!raw) {
    return null;
  }
  try {
    const decoded = decodeURIComponent(raw);
    const parsed = JSON.parse(decoded);
    if (
      typeof parsed !== "object" ||
      parsed === null ||
      typeof parsed.timestamp !== "number" ||
      !Object.prototype.hasOwnProperty.call(parsed, "value")
    ) {
      clearCacheEntry(namespace, username);
      return null;
    }
    return parsed;
  } catch (error) {
    clearCacheEntry(namespace, username);
    return null;
  }
};

export const setCacheEntry = (
  namespace,
  username,
  value,
  ttlSeconds = CACHE_TTL_SECONDS
) => {
  try {
    const payload = JSON.stringify({ timestamp: Date.now(), value });
    const encoded = encodeURIComponent(payload);
    writeCookie(buildCookieKey(namespace, username), encoded, ttlSeconds);
  } catch (error) {
    // ignore serialization or cookie failures
  }
};

export const clearCacheEntry = (namespace, username) => {
  if (typeof document === "undefined") {
    return;
  }
  const key = buildCookieKey(namespace, username);
  document.cookie = `${key}=; Max-Age=0; Path=/; SameSite=Lax`;
};

export const isCacheEntryFresh = (entry, ttlMs = CACHE_TTL_MS) => {
  if (
    !entry ||
    typeof entry.timestamp !== "number" ||
    !Number.isFinite(entry.timestamp)
  ) {
    return false;
  }
  return Date.now() - entry.timestamp < ttlMs;
};
