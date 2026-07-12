import { useCallback, useEffect, useRef, useState } from "react";
import apiClient from "../utils/axiosConfig.js";

const emptyProfile = {
  username: "",
  email: "",
  mobile_number: "",
  client_id: "",
  trading_mode: "demo",
  permissions: { administer_users: false, live_trading: false },
};

const money = new Intl.NumberFormat("en-IN", {
  style: "currency",
  currency: "INR",
  maximumFractionDigits: 2,
});

export default function AccountSettingsModal({
  open,
  username,
  onClose,
  onProfileChanged,
  onBalanceChanged,
  permissions,
  tradingMode,
  onTradingModeChanged,
}) {
  const [profile, setProfile] = useState(emptyProfile);
  const [savedProfile, setSavedProfile] = useState(emptyProfile);
  const [editingProfile, setEditingProfile] = useState(false);
  const [balance, setBalance] = useState(null);
  const [balanceError, setBalanceError] = useState("");
  const [otp, setOtp] = useState("");
  const [otpSent, setOtpSent] = useState(false);
  const [topUp, setTopUp] = useState("");
  const [showTopUp, setShowTopUp] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);
  const [confirmLive, setConfirmLive] = useState(false);
  const [status, setStatus] = useState("idle");
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const suppressNextProfileReload = useRef(false);

  const loadBalance = useCallback(async () => {
    if (!username) return;
    try {
      setBalanceError("");
      const response = await apiClient.get("/account/balance");
      setBalance(response.data);
      onBalanceChanged(response.data);
    } catch (requestError) {
      setBalance(null);
      setBalanceError(
        requestError.response?.data?.detail || "Unable to load balance."
      );
    }
  }, [onBalanceChanged, username]);

  useEffect(() => {
    if (!open || !username) return;
    if (suppressNextProfileReload.current) {
      suppressNextProfileReload.current = false;
      return;
    }
    setStatus("loading");
    setError("");
    setMessage("");
    setOtp("");
    setOtpSent(false);
    setEditingProfile(false);
    setShowTopUp(false);
    setConfirmReset(false);
    setConfirmLive(false);
    Promise.all([
      apiClient.get("/account/profile"),
      loadBalance(),
    ])
      .then(([profileResponse]) => {
        setProfile(profileResponse.data);
        setSavedProfile(profileResponse.data);
      })
      .catch((requestError) =>
        setError(
          requestError.response?.data?.detail ||
            "Unable to load account settings."
        )
      )
      .finally(() => setStatus("idle"));
  }, [loadBalance, open, username]);

  if (!open) return null;

  const updateField = (key, value) => {
    setProfile((current) => ({ ...current, [key]: value }));
    setOtpSent(false);
    setOtp("");
    setMessage("");
  };

  const requestOtp = async () => {
    setStatus("sending-otp");
    setError("");
    setMessage("");
    try {
      const response = await apiClient.post("/account/profile/request-otp", {});
      setOtpSent(true);
      setMessage(`OTP sent to ${response.data.email}.`);
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to send OTP.");
    } finally {
      setStatus("idle");
    }
  };

  const saveProfile = async (event) => {
    event.preventDefault();
    if (!otpSent || otp.trim().length !== 6) {
      setError("Request and enter the 6-digit OTP before saving.");
      return;
    }
    setStatus("saving");
    setError("");
    try {
      const response = await apiClient.patch("/account/profile", {
        otp: otp.trim(),
        new_username: profile.username,
        email: profile.email,
        mobile_number: profile.mobile_number,
        client_id: profile.client_id,
      });
      const updated = response.data.profile;
      setProfile(updated);
      setSavedProfile(updated);
      setOtp("");
      setOtpSent(false);
      setEditingProfile(false);
      setMessage(
        response.data.detail || "Profile information successfully updated."
      );
      suppressNextProfileReload.current = true;
      onProfileChanged(updated);
    } catch (requestError) {
      setError(
        requestError.response?.data?.detail || "Unable to update settings."
      );
    } finally {
      setStatus("idle");
    }
  };

  const topUpDemo = async () => {
    const amount = Number(topUp);
    if (!Number.isFinite(amount) || amount <= 0) {
      setError("Enter a valid top-up amount.");
      return;
    }
    setStatus("wallet");
    setError("");
    try {
      const response = await apiClient.post("/account/balance/top-up", {
        amount,
      });
      setBalance(response.data);
      onBalanceChanged(response.data);
      setTopUp("");
      setShowTopUp(false);
      setMessage("Demo balance topped up.");
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Top-up failed.");
    } finally {
      setStatus("idle");
    }
  };

  const resetDemo = async () => {
    if (!confirmReset) {
      setConfirmReset(true);
      return;
    }
    setStatus("wallet");
    setError("");
    try {
      const response = await apiClient.post("/account/balance/reset", {});
      setBalance(response.data);
      onBalanceChanged(response.data);
      setConfirmReset(false);
      setMessage("Demo balance reset to ₹2,00,000.");
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Reset failed.");
    } finally {
      setStatus("idle");
    }
  };

  const changeTradingMode = async (mode) => {
    setStatus("mode");
    setError("");
    setMessage("");
    try {
      const response = await apiClient.put("/account/trading-mode", {
        mode,
        confirm_live: mode === "live" ? confirmLive : false,
      });
      if (response.data.profile) setProfile(response.data.profile);
      setConfirmLive(false);
      setMessage(response.data.detail);
      onTradingModeChanged(mode);
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to change trading mode.");
    } finally {
      setStatus("idle");
    }
  };

  const isBusy = status !== "idle";
  const activeMode = profile.trading_mode || tradingMode || "demo";
  const isDemo = activeMode === "demo";
  const mayTradeLive = Boolean(permissions?.live_trading || profile.permissions?.live_trading);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-950/85 p-4 backdrop-blur-sm">
      <div className="max-h-[92vh] w-full max-w-2xl overflow-y-auto rounded-3xl border border-slate-700 bg-slate-900 p-6 shadow-2xl shadow-black/60">
        <header className="flex items-start justify-between gap-4">
          <div>
            <p className="text-xs uppercase tracking-[0.35em] text-brand-300">Settings</p>
            <h2 className="mt-2 text-2xl font-semibold text-white">Profile &amp; balance</h2>
            <p className="mt-1 text-sm text-slate-400">Review your account information and funds.</p>
          </div>
          <button type="button" onClick={onClose} className="rounded-full border border-slate-700 px-3 py-1.5 text-sm text-slate-300 hover:bg-slate-800" aria-label="Close settings">✕</button>
        </header>

        <section className="mt-6 rounded-2xl border border-slate-700 bg-slate-950/70 p-5">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div>
              <p className="text-xs uppercase tracking-wider text-slate-500">{isDemo ? "Demo funds" : "Angel One available margin"}</p>
              <p className="mt-1 text-3xl font-semibold text-white">{balance ? money.format(Number(balance.balance || 0)) : "—"}</p>
            </div>
            <button type="button" onClick={loadBalance} disabled={isBusy} className="rounded-lg border border-slate-700 px-3 py-2 text-xs text-slate-300 hover:bg-slate-800 disabled:opacity-50">Refresh balance</button>
          </div>
          {balanceError ? <p className="mt-3 text-xs text-amber-300">{balanceError}</p> : null}
          {isDemo ? (
            <div className="mt-4 space-y-3">
              <div className="flex flex-wrap gap-2">
                <button type="button" onClick={() => { setShowTopUp((current) => !current); setConfirmReset(false); }} disabled={isBusy} className="rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white disabled:bg-slate-700">{showTopUp ? "Cancel top-up" : "Top up"}</button>
                <button type="button" onClick={resetDemo} disabled={isBusy} className={`rounded-lg px-4 py-2 text-sm font-semibold ${confirmReset ? "bg-rose-500 text-white" : "border border-slate-700 text-slate-300"}`}>{confirmReset ? "Confirm reset to ₹2,00,000" : "Reset balance"}</button>
              </div>
              {showTopUp ? (
                <div className="flex flex-wrap gap-2 rounded-xl border border-slate-800 bg-slate-900/70 p-3">
                  <input autoFocus type="number" min="1" step="100" value={topUp} onChange={(event) => setTopUp(event.target.value)} placeholder="Enter top-up amount" className="min-w-48 flex-1 rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-white" />
                  <button type="button" onClick={topUpDemo} disabled={isBusy} className="rounded-lg bg-emerald-500 px-4 py-2 text-sm font-semibold text-white disabled:bg-slate-700">Add funds</button>
                </div>
              ) : null}
            </div>
          ) : null}
        </section>

        <section className="mt-5 rounded-2xl border border-slate-700 bg-slate-950/70 p-5">
          <h3 className="font-semibold text-white">Trading mode</h3>
          <p className="mt-1 text-sm text-slate-400">Demo and administrative access are independent. Live mode sends real broker orders.</p>
          <div className="mt-4 flex flex-wrap items-center gap-3">
            <button type="button" disabled={isBusy || isDemo} onClick={() => changeTradingMode("demo")} className="rounded-lg border border-slate-700 px-4 py-2 text-sm text-slate-200 disabled:opacity-40">Use demo</button>
            <button type="button" disabled={isBusy || !mayTradeLive || !isDemo || !confirmLive} onClick={() => changeTradingMode("live")} className="rounded-lg bg-rose-500 px-4 py-2 text-sm font-semibold text-white disabled:bg-slate-700">Switch to live</button>
            <span className="rounded-full bg-slate-800 px-3 py-1 text-xs uppercase text-slate-300">Current: {activeMode}</span>
          </div>
          {isDemo && mayTradeLive ? (
            <label className="mt-4 flex items-start gap-2 text-sm text-amber-200">
              <input type="checkbox" checked={confirmLive} onChange={(event) => setConfirmLive(event.target.checked)} />
              I understand that live mode submits real orders through my connected broker profile.
            </label>
          ) : null}
          {!mayTradeLive ? <p className="mt-3 text-xs text-slate-500">Live trading requires permission from an authorized administrator.</p> : null}
        </section>

        {message ? <p className="mt-5 rounded-lg bg-emerald-500/10 px-4 py-3 text-sm text-emerald-200">{message}</p> : null}
        {error ? <p className="mt-5 rounded-lg bg-rose-500/10 px-4 py-3 text-sm text-rose-200">{error}</p> : null}

        {!editingProfile ? (
          <section className="mt-6 rounded-2xl border border-slate-700 bg-slate-950/50 p-5">
            <div className="flex items-center justify-between gap-3">
              <div>
                <h3 className="text-lg font-semibold text-white">Profile information</h3>
                <p className="mt-1 text-xs text-slate-400">Your registered identity and Angel One account details.</p>
              </div>
              <button type="button" onClick={() => { setEditingProfile(true); setMessage(""); setError(""); }} disabled={isBusy} className="rounded-lg border border-brand-400/50 px-4 py-2 text-sm font-semibold text-brand-200 hover:bg-brand-500/10">Edit profile</button>
            </div>
            <dl className="mt-5 grid gap-4 sm:grid-cols-2">
              {[
                ["Username", profile.username],
                ["Email", profile.email],
                ["Mobile number", profile.mobile_number],
                ["Angel One Client ID", profile.client_id],
              ].map(([label, value]) => (
                <div key={label} className="rounded-xl border border-slate-800 bg-slate-950/70 p-4">
                  <dt className="text-xs uppercase tracking-wide text-slate-500">{label}</dt>
                  <dd className="mt-1 break-words text-sm font-medium text-white">{value || "Not provided"}</dd>
                </div>
              ))}
            </dl>
          </section>
        ) : (
          <form className="mt-6 space-y-4 rounded-2xl border border-slate-700 bg-slate-950/40 p-5" onSubmit={saveProfile}>
            <div className="flex items-start justify-between gap-3">
              <div>
                <h3 className="text-lg font-semibold text-white">Edit profile</h3>
                <p className="mt-1 text-xs text-slate-400">After making changes, verify the update using an OTP sent to your current email.</p>
              </div>
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              {[
                ["username", "Username", "text"],
                ["email", "Email", "email"],
                ["mobile_number", "Mobile number", "tel"],
                ["client_id", "Angel One Client ID", "text"],
              ].map(([key, label, type]) => (
                <label key={key} className="space-y-1.5 text-sm text-slate-300">
                  <span>{label}</span>
                  <input required type={type} value={profile[key] || ""} onChange={(event) => updateField(key, key === "mobile_number" ? event.target.value.replace(/\D/g, "").slice(0, 10) : event.target.value)} className="w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-white outline-none focus:border-brand-400" />
                </label>
              ))}
            </div>

            <div className="rounded-xl border border-slate-700 bg-slate-950/60 p-4">
              <div className="flex flex-wrap gap-2">
                <button type="button" onClick={requestOtp} disabled={isBusy} className="rounded-lg border border-brand-400/50 px-4 py-2 text-sm font-semibold text-brand-200 disabled:opacity-50">{status === "sending-otp" ? "Sending…" : otpSent ? "Resend OTP" : "Send verification OTP"}</button>
                <input inputMode="numeric" maxLength={6} value={otp} onChange={(event) => setOtp(event.target.value.replace(/\D/g, ""))} placeholder="6-digit OTP" className="w-40 rounded-lg border border-slate-700 bg-slate-900 px-3 py-2 text-sm text-white" />
              </div>
              {otpSent ? <p className="mt-2 text-xs text-slate-400">Enter the OTP and select “Verify OTP &amp; update”.</p> : null}
            </div>

            <div className="flex justify-end gap-3">
              <button type="button" onClick={() => { setProfile(savedProfile); setEditingProfile(false); setOtp(""); setOtpSent(false); setError(""); }} className="rounded-lg border border-slate-700 px-4 py-2 text-sm text-slate-300">Cancel editing</button>
              <button type="submit" disabled={isBusy || !otpSent || otp.length !== 6} className="rounded-lg bg-brand-500 px-5 py-2 text-sm font-semibold text-white disabled:bg-slate-700">{status === "saving" ? "Updating…" : "Verify OTP & update"}</button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
}
