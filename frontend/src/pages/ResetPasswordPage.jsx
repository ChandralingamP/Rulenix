import { useEffect, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import axios from "axios";
import { getAuthUsername } from "../utils/authCookies.js";
import { isStrongPassword } from "../utils/password.js";
import {
  clearPendingPasswordReset,
  getPendingPasswordReset,
} from "../utils/pendingAuth.js";
import { API_BASE_URL } from "../utils/constants.js";
const LOGIN_NOTICE_KEY = "rulenix_login_notice";

export default function ResetPasswordPage() {
  const navigate = useNavigate();
  const [resetData, setResetData] = useState(null);
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [error, setError] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);

  useEffect(() => {
    const existingUser = getAuthUsername();
    if (existingUser) {
      navigate("/", { replace: true });
      return;
    }
    const pending = getPendingPasswordReset();
    if (!pending?.email || !pending?.otp) {
      navigate("/forgot-password", { replace: true });
      return;
    }
    setResetData(pending);
  }, [navigate]);

  if (!resetData) {
    return null;
  }

  const handleSubmit = (event) => {
    event.preventDefault();

    if (!isStrongPassword(password)) {
      setError(
        "Password must be 12–128 characters with uppercase, lowercase, number, and symbol."
      );
      return;
    }

    if (password !== confirmPassword) {
      setError("Passwords do not match.");
      return;
    }

    setError("");
    setIsSubmitting(true);

    axios
      .post(
        `${API_BASE_URL}/auth/password/reset/`,
        {
          email: resetData.email,
          otp: resetData.otp,
          password,
          confirm_password: confirmPassword,
        },
        { withCredentials: false }
      )
      .then(() => {
        clearPendingPasswordReset();
        if (typeof window !== "undefined") {
          try {
            window.sessionStorage.setItem(
              LOGIN_NOTICE_KEY,
              "Password updated successfully. Please log in."
            );
          } catch (error_) {
            // ignore storage failures
          }
        }
        navigate("/login", { replace: true });
      })
      .catch((resetError) => {
        const detail =
          resetError.response?.data?.detail ||
          resetError.response?.data?.non_field_errors?.[0] ||
          "Unable to reset password. Please try again.";
        setError(detail);
      })
      .finally(() => {
        setIsSubmitting(false);
      });
  };

  return (
    <div className="min-h-screen bg-gradient-to-br from-slate-950 via-slate-950 to-slate-900 py-16">
      <div className="mx-auto flex w-full max-w-md flex-col gap-8 rounded-3xl border border-slate-800 bg-slate-900/70 p-8 text-white shadow-2xl shadow-black/40">
        <header className="space-y-2 text-center">
          <p className="text-xs uppercase tracking-[0.4em] text-brand-300">
            Rulenix
          </p>
          <h1 className="text-3xl font-semibold">Set a new password</h1>
          <p className="text-sm text-slate-400">
            Choose a strong password for{" "}
            <span className="font-semibold text-white">{resetData.email}</span>.
          </p>
        </header>
        <form className="space-y-5" onSubmit={handleSubmit}>
          <div className="space-y-2">
            <label
              className="text-sm font-medium text-slate-200"
              htmlFor="new-password"
            >
              New password
            </label>
            <input
              id="new-password"
              name="new-password"
              type="password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              placeholder="••••••"
              className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
              autoComplete="new-password"
              required
            />
            <p className="text-[11px] text-slate-400">
              Use 12–128 characters with uppercase, lowercase, number, and
              symbol.
            </p>
          </div>
          <div className="space-y-2">
            <label
              className="text-sm font-medium text-slate-200"
              htmlFor="confirm-password"
            >
              Confirm password
            </label>
            <input
              id="confirm-password"
              name="confirm-password"
              type="password"
              value={confirmPassword}
              onChange={(event) => setConfirmPassword(event.target.value)}
              placeholder="••••••"
              className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
              autoComplete="new-password"
              required
            />
          </div>
          {error ? <p className="text-xs text-rose-300">{error}</p> : null}
          <button
            type="submit"
            className="w-full rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white shadow-lg shadow-brand-500/30 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:bg-slate-700"
            disabled={isSubmitting}
          >
            {isSubmitting ? "Updating password..." : "Update password"}
          </button>
        </form>
        <footer className="text-center text-xs text-slate-400">
          Remember the OTP?{" "}
          <Link
            className="font-semibold text-brand-300 hover:text-brand-200"
            to="/forgot-password/verify"
          >
            Go back to verification
          </Link>
          .
        </footer>
      </div>
    </div>
  );
}
