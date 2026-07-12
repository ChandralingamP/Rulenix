import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import apiClient from "../utils/axiosConfig.js";
import {
  getAuthUsername,
  setAuthUsername,
} from "../utils/authCookies.js";
const defaultFormState = {
  username: "",
  password: "",
};

export default function LoginPage() {
  const [formState, setFormState] = useState(defaultFormState);
  const [error, setError] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [mathAnswer, setMathAnswer] = useState("");
  const [captchaError, setCaptchaError] = useState("");
  const [infoMessage, setInfoMessage] = useState("");
  const navigate = useNavigate();

  const captcha = useMemo(() => {
    const first = Math.floor(Math.random() * 9) + 1;
    const second = Math.floor(Math.random() * 9) + 1;
    return { first, second, result: first + second };
  }, []);
  useEffect(() => {
    const LOGIN_NOTICE_KEY = "rulenix_login_notice";
    const existingUser = getAuthUsername();

    if (existingUser) {
      navigate("/", { replace: true });
      return;
    }

    if (typeof window === "undefined") {
      return;
    }

    try {
      const messages = [];
      const reason = window.sessionStorage.getItem("rulenix_logout_reason");
      if (reason) {
        messages.push(reason);
        window.sessionStorage.removeItem("rulenix_logout_reason");
      }

      const notice = window.sessionStorage.getItem(LOGIN_NOTICE_KEY);
      if (notice) {
        messages.push(notice);
        window.sessionStorage.removeItem(LOGIN_NOTICE_KEY);
      }

      if (messages.length > 0) {
        setInfoMessage(messages.join(" "));
      }
    } catch (storageError) {
      console.warn("Unable to read login notices", storageError);
    }
  }, [navigate]);

  const handleChange = (event) => {
    const { name, value } = event.target;
    setFormState((prev) => ({ ...prev, [name]: value }));
  };

  const handleSubmit = (event) => {
    event.preventDefault();
    const trimmedUsername = formState.username.trim();
    if (!trimmedUsername || !formState.password) {
      setError("Please provide both username and password.");
      return;
    }
    if (mathAnswer.trim() !== String(captcha.result)) {
      setCaptchaError("Solve the math puzzle to continue.");
      return;
    }
    setError("");
    setCaptchaError("");
    setIsSubmitting(true);

    apiClient
      .post(`/auth/login/`, {
        username: trimmedUsername,
        password: formState.password,
      })
      .then((response) => {
        const usernameFromServer = response.data?.username || trimmedUsername;
        const canAdminister = Boolean(response.data?.permissions?.administer_users);
        setAuthUsername(usernameFromServer.toUpperCase());
        const redirectTo = canAdminister ? "/admin" : "/";
        navigate(redirectTo, { replace: true });
      })
      .catch((loginError) => {
        const detail =
          loginError.response?.data?.detail ||
          loginError.response?.data?.non_field_errors?.[0] ||
          "Invalid username or password.";
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
          <h1 className="text-3xl font-semibold">Welcome back</h1>
          <p className="text-sm text-slate-400">
            Sign in to access your trading control center.
          </p>
        </header>
        {infoMessage ? (
          <p className="rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
            {infoMessage}
          </p>
        ) : null}
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
              autoComplete="current-password"
              required
            />
          </div>
          <div className="space-y-2">
            <label
              className="text-sm font-medium text-slate-200"
              htmlFor="captcha"
            >
              Quick Check
            </label>
            <div className="flex items-center gap-2">
              <span className="text-sm text-slate-300">
                {captcha.first} + {captcha.second} =
              </span>
              <input
                id="captcha"
                name="captcha"
                value={mathAnswer}
                onChange={(event) =>
                  setMathAnswer(event.target.value.replace(/\D/g, ""))
                }
                placeholder="?"
                className="w-24 rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
                inputMode="numeric"
                maxLength={2}
                required
              />
            </div>
          </div>
          <div className="flex items-center justify-end text-xs text-slate-400">
            <Link
              className="font-semibold text-brand-300 hover:text-brand-200"
              to="/forgot-password"
            >
              Forgot password?
            </Link>
          </div>
          {error ? <p className="text-xs text-rose-300">{error}</p> : null}
          {captchaError ? (
            <p className="text-xs text-rose-300">{captchaError}</p>
          ) : null}
          <button
            type="submit"
            className="w-full rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white shadow-lg shadow-brand-500/30 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:bg-slate-700"
            disabled={isSubmitting}
          >
            {isSubmitting ? "Logging In..." : "Log In"}
          </button>
        </form>
        <footer className="text-center text-xs text-slate-400">
          Don&apos;t have an account?{" "}
          <Link
            className="font-semibold text-brand-300 hover:text-brand-200"
            to="/signup"
          >
            Sign up
          </Link>
        </footer>
      </div>
    </div>
  );
}
