const SESSION_TIMEOUT_MINUTES = 30;
export const SESSION_TIMEOUT_MS = SESSION_TIMEOUT_MINUTES * 60 * 1000;
const LAST_ACTIVE_KEY = "rulenix_last_active";
const USERNAME_KEY = "rulenix_username";

const read = (key) => {
  try { return window.localStorage.getItem(key); } catch { return null; }
};
const write = (key, value) => {
  try { window.localStorage.setItem(key, value); } catch { /* unavailable */ }
};

export const markSessionActive = () => write(LAST_ACTIVE_KEY, String(Date.now()));

export const setAuthUsername = (username) => {
  if (!username || typeof window === "undefined") return;
  write(USERNAME_KEY, username);
  markSessionActive();
};

export const hasSessionExpired = () => {
  const value = Number(read(LAST_ACTIVE_KEY));
  return Boolean(value) && Date.now() - value >= SESSION_TIMEOUT_MS;
};

export const getAuthUsername = () => {
  if (typeof window === "undefined") return null;
  return read(USERNAME_KEY);
};

export const clearAuthUsername = () => {
  if (typeof window === "undefined") return;
  try {
    [LAST_ACTIVE_KEY, USERNAME_KEY].forEach((key) => window.localStorage.removeItem(key));
  } catch { /* unavailable */ }
};
