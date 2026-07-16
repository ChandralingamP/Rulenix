import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import axios from "axios";
import { API_BASE_URL } from "../utils/constants.js";
import {
  clearPendingSignup,
  getPendingSignup,
} from "../utils/pendingAuth.js";

export default function VerifyOtpPage() {
  const navigate = useNavigate();
  const [pendingData, setPendingData] = useState(null);
  const [otp, setOtp] = useState("");
  const [error, setError] = useState("");
  const [infoMessage, setInfoMessage] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [resendStatus, setResendStatus] = useState({
    status: "idle",
    message: "",
  });

  useEffect(() => {
    const pending = getPendingSignup();
    if (!pending) {
      navigate("/signup", { replace: true });
      return;
    }
    setPendingData(pending);
    setInfoMessage(`We emailed a 6-digit OTP to ${pending.email}.`);
  }, [navigate]);

  const maskedEmail = useMemo(() => {
    if (!pendingData?.email) {
      return "";
    }
    const [localPart, domain] = pendingData.email.split("@");
    if (!domain) {
      return pendingData.email;
    }
    const visible = localPart.slice(0, 2);
    return `${visible}***@${domain}`;
  }, [pendingData]);

  const handleSubmit = (event) => {
    event.preventDefault();
    if (!pendingData) {
      return;
    }
    const trimmedOtp = otp.trim();
    if (!trimmedOtp || trimmedOtp.length !== 6) {
      setError("Enter the 6-digit OTP sent to your email.");
      return;
    }

    setError("");
    setIsSubmitting(true);

    axios
      .post(
        `${API_BASE_URL}/auth/signup/`,
        {
          ...pendingData,
          otp: trimmedOtp,
        },
        { withCredentials: false }
      )
      .then(() => {
        clearPendingSignup();
        sessionStorage.setItem("rulenix_login_notice", "Account created. Sign in to continue.");
        navigate("/login", { replace: true });
      })
      .catch((signupError) => {
        const detail =
          signupError.response?.data?.detail ||
          signupError.response?.data?.non_field_errors?.[0] ||
          "Invalid or expired OTP.";
        setError(detail);
      })
      .finally(() => {
        setIsSubmitting(false);
      });
  };

  const handleResendOtp = () => {
    if (!pendingData) {
      return;
    }
    setResendStatus({ status: "loading", message: "Sending a new OTP..." });
    axios
      .post(
        `${API_BASE_URL}/auth/request-otp/`,
        { email: pendingData.email, username: pendingData.username },
        { withCredentials: false }
      )
      .then(() => {
        setResendStatus({
          status: "succeeded",
          message: "OTP resent successfully.",
        });
      })
      .catch((error_) => {
        const detail =
          error_.response?.data?.detail ||
          "Unable to resend OTP. Please try again.";
        setResendStatus({ status: "failed", message: detail });
      });
  };

  if (!pendingData) {
    return null;
  }

  return (
    <div className="min-h-screen bg-gradient-to-br from-slate-950 via-slate-950 to-slate-900 py-16">
      <div className="mx-auto flex w-full max-w-md flex-col gap-8 rounded-3xl border border-slate-800 bg-slate-900/70 p-8 text-white shadow-2xl shadow-black/40">
        <header className="space-y-2 text-center">
          <p className="text-xs uppercase tracking-[0.4em] text-brand-300">
            Rulenix
          </p>
          <h1 className="text-3xl font-semibold">Verify your email</h1>
          <p className="text-sm text-slate-400">
            Enter the OTP sent to{" "}
            <span className="font-semibold text-white">{maskedEmail}</span>.
          </p>
          {infoMessage ? (
            <p className="text-xs text-emerald-300">{infoMessage}</p>
          ) : null}
        </header>
        <form className="space-y-5" onSubmit={handleSubmit}>
          <div className="space-y-2">
            <label className="text-sm font-medium text-slate-200" htmlFor="otp">
              Email OTP
            </label>
            <input
              id="otp"
              name="otp"
              value={otp}
              onChange={(event) => setOtp(event.target.value)}
              placeholder="123456"
              className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
              inputMode="numeric"
              maxLength={6}
              pattern="[0-9]{6}"
              required
            />
          </div>
          {error ? <p className="text-xs text-rose-300">{error}</p> : null}
          <button
            type="submit"
            className="w-full rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white shadow-lg shadow-brand-500/30 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:bg-slate-700"
            disabled={isSubmitting}
          >
            {isSubmitting ? "Verifying..." : "Verify & Create Account"}
          </button>
        </form>
        <div className="space-y-3 text-center text-xs text-slate-400">
          <button
            type="button"
            onClick={handleResendOtp}
            className="w-full rounded-lg border border-brand-400/40 bg-brand-500/10 px-3 py-2 text-xs font-semibold text-brand-200 transition hover:bg-brand-500/20 disabled:cursor-not-allowed disabled:bg-slate-800"
            disabled={resendStatus.status === "loading"}
          >
            {resendStatus.status === "loading" ? "Resending..." : "Resend OTP"}
          </button>
          {resendStatus.message ? (
            <p
              className={`text-[11px] ${
                resendStatus.status === "failed"
                  ? "text-rose-300"
                  : "text-emerald-300"
              }`}
            >
              {resendStatus.message}
            </p>
          ) : null}
          <p>
            Entered the wrong details?{" "}
            <Link
              to="/signup"
              onClick={clearPendingSignup}
              className="font-semibold text-brand-300 hover:text-brand-200"
            >
              Go back to signup
            </Link>
            .
          </p>
        </div>
      </div>
    </div>
  );
}
