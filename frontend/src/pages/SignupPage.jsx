import { useEffect, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import apiClient from "../utils/axiosConfig.js";
import { getAuthUsername } from "../utils/authCookies.js";
import { isStrongPassword } from "../utils/password.js";
import { setPendingSignup } from "../utils/pendingAuth.js";

const defaultFormState = {
  username: "",
  userId: "",
  apiKey: "",
  mobile: "",
  email: "",
  password: "",
  confirmPassword: "",
};

export default function SignupPage() {
  const [formState, setFormState] = useState(defaultFormState);
  const [error, setError] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);
  const navigate = useNavigate();

  useEffect(() => {
    const existingUser = getAuthUsername();
    if (existingUser) {
      navigate("/", { replace: true });
    }
  }, [navigate]);

  const handleChange = (event) => {
    const { name, value } = event.target;
    if (name === "mobile") {
      const onlyDigits = value.replace(/\D/g, "").slice(0, 10);
      setFormState((prev) => ({ ...prev, [name]: onlyDigits }));
      return;
    }
    setFormState((prev) => ({ ...prev, [name]: value }));
  };

  const handleSubmit = (event) => {
    event.preventDefault();
    const trimmedUsername = formState.username.trim();
    const trimmedUserId = formState.userId.trim();
    const trimmedApiKey = formState.apiKey.trim();
    const trimmedMobile = formState.mobile.trim();
    const trimmedEmail = formState.email.trim();

    if (!trimmedUsername) {
      setError("Username is required.");
      return;
    }

    if (!trimmedUserId) {
      setError("Client ID is required.");
      return;
    }

    if (!trimmedApiKey) {
      setError("API key is required.");
      return;
    }

    if (!trimmedMobile || !/^\d{10}$/.test(trimmedMobile)) {
      setError("Enter a valid 10-digit mobile number.");
      return;
    }

    if (
      !trimmedEmail ||
      !/^[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}$/i.test(trimmedEmail)
    ) {
      setError("Enter a valid email address.");
      return;
    }

    if (!isStrongPassword(formState.password)) {
      setError(
        "Password must be 12–128 characters with uppercase, lowercase, number, and symbol."
      );
      return;
    }

    if (formState.password !== formState.confirmPassword) {
      setError("Passwords do not match.");
      return;
    }

    setError("");
    setIsSubmitting(true);

    apiClient
      .post(`/auth/request-otp/`, {
        email: trimmedEmail,
        username: trimmedUsername,
      })
      .then(() => {
        const pendingPayload = {
          username: trimmedUsername,
          user_id: trimmedUserId,
          api_key: trimmedApiKey,
          mobile: trimmedMobile,
          email: trimmedEmail,
          password: formState.password,
          confirm_password: formState.confirmPassword,
        };
        setPendingSignup(pendingPayload);
        navigate("/verify-otp", {
          replace: false,
          state: { email: trimmedEmail },
        });
      })
      .catch((otpError) => {
        const detail =
          otpError.response?.data?.username?.[0] ||
          otpError.response?.data?.detail ||
          "Unable to send OTP. Please try again.";
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
          <h1 className="text-3xl font-semibold">Create your account</h1>
          <p className="text-sm text-slate-400">
            Set your credentials to connect your brokerage account.
          </p>
        </header>
        <form className="space-y-5" onSubmit={handleSubmit}>
          <div className="space-y-2">
            <label
              className="text-sm font-medium text-slate-200"
              htmlFor="username"
            >
              Username
            </label>
            <input
              id="username"
              name="username"
              value={formState.username}
              onChange={handleChange}
              placeholder="TRADER01"
              className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
              autoComplete="username"
              required
            />
          </div>
          <div className="grid gap-4 md:grid-cols-2">
            <div className="space-y-2">
              <label
                className="text-sm font-medium text-slate-200"
                htmlFor="userId"
              >
                Client ID
              </label>
              <input
                id="userId"
                name="userId"
                value={formState.userId}
                onChange={handleChange}
                placeholder="AB1234"
                className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
                autoComplete="off"
                required
              />
            </div>
            <div className="space-y-2">
              <label
                className="text-sm font-medium text-slate-200"
                htmlFor="apiKey"
              >
                API Key
              </label>
              <input
                id="apiKey"
                name="apiKey"
                value={formState.apiKey}
                onChange={handleChange}
                placeholder="API_KEY_FROM_BROKER"
                className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
                autoComplete="off"
                required
              />
            </div>
            <div className="space-y-2">
              <label
                className="text-sm font-medium text-slate-200"
                htmlFor="mobile"
              >
                Mobile Number
              </label>
              <input
                id="mobile"
                name="mobile"
                value={formState.mobile}
                onChange={handleChange}
                placeholder="9876543210"
                className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
                autoComplete="tel"
                inputMode="numeric"
                pattern="[0-9]{10}"
                title="Enter exactly 10 digits"
                maxLength={10}
                required
              />
            </div>
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
                value={formState.email}
                onChange={handleChange}
                placeholder="trader@example.com"
                className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
                autoComplete="email"
                required
              />
            </div>
          </div>
          <div className="space-y-2">
            <label
              className="text-sm font-medium text-slate-200"
              htmlFor="password"
            >
              Password
            </label>
            <input
              id="password"
              name="password"
              type="password"
              value={formState.password}
              onChange={handleChange}
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
              htmlFor="confirmPassword"
            >
              Confirm Password
            </label>
            <input
              id="confirmPassword"
              name="confirmPassword"
              type="password"
              value={formState.confirmPassword}
              onChange={handleChange}
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
            {isSubmitting ? "Sending OTP..." : "Sign Up"}
          </button>
        </form>
        <footer className="text-center text-xs text-slate-400">
          Already have an account?{" "}
          <Link
            className="font-semibold text-brand-300 hover:text-brand-200"
            to="/login"
          >
            Log in here
          </Link>
          .
        </footer>
      </div>
    </div>
  );
}
