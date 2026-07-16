/** @vitest-environment jsdom */

import { afterEach, describe, expect, it } from "vitest";
import {
  clearPendingPasswordReset,
  clearPendingSignup,
  getPendingPasswordReset,
  getPendingSignup,
  setPendingPasswordReset,
  setPendingSignup,
} from "./pendingAuth.js";

describe("pending authentication state", () => {
  afterEach(() => {
    clearPendingSignup();
    clearPendingPasswordReset();
  });

  it("keeps signup credentials only in volatile module memory", () => {
    setPendingSignup({ email: "user@example.com", password: "secret" });
    expect(getPendingSignup()).toEqual({
      email: "user@example.com",
      password: "secret",
    });
    expect(window.sessionStorage.getItem("rulenix_pending_signup")).toBeNull();
    expect(window.sessionStorage.getItem("rulenix_password_reset")).toBeNull();
    clearPendingSignup();
    expect(getPendingSignup()).toBeNull();
  });

  it("clears the password reset OTP from volatile memory", () => {
    setPendingPasswordReset({ email: "user@example.com", otp: "123456" });
    expect(getPendingPasswordReset()?.otp).toBe("123456");
    clearPendingPasswordReset();
    expect(getPendingPasswordReset()).toBeNull();
  });
});
