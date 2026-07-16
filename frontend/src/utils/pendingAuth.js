let pendingSignup = null;
let pendingPasswordReset = null;

const purgeLegacyStorage = () => {
  if (typeof window === "undefined") return;
  try {
    window.sessionStorage.removeItem("rulenix_pending_signup");
    window.sessionStorage.removeItem("rulenix_password_reset");
  } catch {
    // Storage can be unavailable in hardened/private browser contexts.
  }
};

purgeLegacyStorage();

const clearObject = (value) => {
  if (!value || typeof value !== "object") return;
  Object.keys(value).forEach((key) => {
    value[key] = "";
  });
};

export const setPendingSignup = (value) => {
  clearObject(pendingSignup);
  pendingSignup = value ? { ...value } : null;
};

export const getPendingSignup = () => pendingSignup;

export const clearPendingSignup = () => {
  clearObject(pendingSignup);
  pendingSignup = null;
};

export const setPendingPasswordReset = (value) => {
  clearObject(pendingPasswordReset);
  pendingPasswordReset = value ? { ...value } : null;
};

export const getPendingPasswordReset = () => pendingPasswordReset;

export const clearPendingPasswordReset = () => {
  clearObject(pendingPasswordReset);
  pendingPasswordReset = null;
};
