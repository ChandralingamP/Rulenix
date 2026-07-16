import { useEffect, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import axios from "axios";
import { getAuthUsername } from "../utils/authCookies.js";
import { API_BASE_URL } from "../utils/constants.js";
import { setPendingPasswordReset } from "../utils/pendingAuth.js";

export default function ForgotPasswordPage() {
  const [email, setEmail] = useState("");
  const [error, setError] = useState("");
  const [info, setInfo] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);
  const navigate = useNavigate();

  useEffect(() => {
    const existingUser = getAuthUsername();
    if (existingUser) {
      navigate("/", { replace: true });
    }
  }, [navigate]);

  const handleSubmit = (event) => {
    event.preventDefault();
    const trimmedEmail = email.trim();
    if (
      !trimmedEmail ||
      !/^[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}$/i.test(trimmedEmail)
    ) {
      setError("Enter a valid email address.");
      return;
    }

    setError("");
    setInfo("");
    setIsSubmitting(true);

    axios
      .post(
        `${API_BASE_URL}/auth/password/request-reset/`,
        { email: trimmedEmail },
        { withCredentials: false }
      )
      .then(() => {
        setPendingPasswordReset({ email: trimmedEmail });
        setInfo("We sent an OTP to your email address.");
        navigate("/forgot-password/verify", {
          replace: false,
          state: { email: trimmedEmail },
        });
      })
      .catch((requestError) => {
        const detail =
          requestError.response?.data?.detail ||
          "Unable to send reset OTP. Please try again.";
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
          <h1 className="text-3xl font-semibold">Forgot password?</h1>
          <p className="text-sm text-slate-400">
            Enter your account email to receive an OTP for password reset.
          </p>
        </header>
        <form className="space-y-5" onSubmit={handleSubmit}>
          <div className="space-y-2">
            <label
              className="text-sm font-medium text-slate-200"
              htmlFor="email"
            >
              Email
            </label>
            <input
              id="email"
              name="email"
              type="email"
              value={email}
              onChange={(event) => setEmail(event.target.value)}
              placeholder="trader@example.com"
              className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
              autoComplete="email"
              required
            />
          </div>
          {error ? <p className="text-xs text-rose-300">{error}</p> : null}
          {info ? <p className="text-xs text-emerald-300">{info}</p> : null}
          <button
            type="submit"
            className="w-full rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white shadow-lg shadow-brand-500/30 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:bg-slate-700"
            disabled={isSubmitting}
          >
            {isSubmitting ? "Sending OTP..." : "Send OTP"}
          </button>
        </form>
        <footer className="text-center text-xs text-slate-400">
          Remembered it?{" "}
          <Link
            className="font-semibold text-brand-300 hover:text-brand-200"
            to="/login"
          >
            Head back to login
          </Link>
          .
        </footer>
      </div>
    </div>
  );
}
