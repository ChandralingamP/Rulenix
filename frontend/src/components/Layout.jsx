import { useCallback, useEffect, useMemo, useState } from "react";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import AccountSettingsModal from "./AccountSettingsModal.jsx";
import apiClient from "../utils/axiosConfig.js";
import {
  clearAuthUsername,
  getAuthUsername,
  hasSessionExpired,
  markSessionActive,
  SESSION_TIMEOUT_MS,
  setAuthUsername,
} from "../utils/authCookies.js";

const baseNavItems = [
  { label: "Home", to: "/" },
  { label: "Strategies", to: "/strategies" },
  { label: "Profit & Loss", to: "/pnl" },
];

const balanceFormatter = new Intl.NumberFormat("en-IN", {
  style: "currency",
  currency: "INR",
  maximumFractionDigits: 0,
});

export default function Layout({ children }) {
  const navigate = useNavigate();
  const location = useLocation();
  const [username, setUsername] = useState(() => getAuthUsername());
  const [permissions, setPermissions] = useState({
    administer_users: false,
    live_trading: false,
    backtesting: false,
  });
  const [tradingMode, setTradingMode] = useState("demo");
  const [sessionReady, setSessionReady] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [accountBalance, setAccountBalance] = useState(null);
  const [balanceError, setBalanceError] = useState("");

  const navItems = useMemo(() => {
    const items = [...baseNavItems];
    if (permissions.backtesting) {
      items.push({ label: "Backtesting", to: "/backtesting" });
    }
    if (permissions.administer_users) {
      items.push({ label: "Admin", to: "/admin" });
    }
    return items;
  }, [permissions.administer_users, permissions.backtesting]);

  const loadAccountBalance = useCallback(async () => {
    if (!username) return;
    try {
      setBalanceError("");
      const response = await apiClient.get("/account/balance");
      setAccountBalance(response.data);
    } catch (requestError) {
      setAccountBalance(null);
      setBalanceError(
        requestError.response?.data?.detail || "Balance unavailable"
      );
    }
  }, [username]);

  const syncAccessStatus = useCallback(async () => {
    try {
      const response = await apiClient.get("/auth/access/");
      const serverUsername = response.data?.username;
      const nextPermissions = response.data?.permissions || {};
      setPermissions({
        administer_users: Boolean(nextPermissions.administer_users),
        live_trading: Boolean(nextPermissions.live_trading),
        backtesting: Boolean(nextPermissions.backtesting),
      });
      setTradingMode(response.data?.trading_mode === "live" ? "live" : "demo");
      if (serverUsername) {
        setAuthUsername(serverUsername);
        setUsername(serverUsername);
      }
      setSessionReady(true);
      await loadAccountBalance();
    } catch (requestError) {
      if ([401, 404].includes(requestError.response?.status)) {
        clearAuthUsername();
        setUsername(null);
        setPermissions({ administer_users: false, live_trading: false, backtesting: false });
        setTradingMode("demo");
        setSessionReady(true);
        navigate("/login", {
          replace: true,
          state: { from: location.pathname },
        });
      }
    }
  }, [loadAccountBalance, location.pathname, navigate]);

  useEffect(() => {
    loadAccountBalance();
  }, [loadAccountBalance]);

  useEffect(() => {
    syncAccessStatus();
    const handleFocus = () => syncAccessStatus();
    const handleVisibility = () => {
      if (document.visibilityState === "visible") syncAccessStatus();
    };
    window.addEventListener("focus", handleFocus);
    document.addEventListener("visibilitychange", handleVisibility);
    return () => {
      window.removeEventListener("focus", handleFocus);
      document.removeEventListener("visibilitychange", handleVisibility);
    };
  }, [syncAccessStatus]);

  const handleProfileChanged = useCallback(
    (profile) => {
      const nextUsername = profile.username.toUpperCase();
      setAuthUsername(nextUsername);
      setUsername(nextUsername);
    },
    []
  );

  const handleLogout = useCallback(
    async (reason = "manual", sourcePath = null, notifyServer = true) => {
      if (notifyServer) {
        try { await apiClient.post("/auth/logout/"); } catch { /* already expired */ }
      }
      if (reason === "timeout" && typeof window !== "undefined") {
        try {
          window.sessionStorage.setItem(
            "rulenix_logout_reason",
            "Session expired after 30 minutes of inactivity."
          );
        } catch (error) {
          // ignore storage failures
        }
      }
      clearAuthUsername();
      setUsername(null);
      setPermissions({ administer_users: false, live_trading: false, backtesting: false });
      setTradingMode("demo");
      const navigationOptions = sourcePath
        ? { replace: true, state: { from: sourcePath } }
        : { replace: true };
      navigate("/login", navigationOptions);
    },
    [navigate]
  );

  useEffect(() => {
    const current = getAuthUsername();
    if (!current) {
      syncAccessStatus();
      return;
    }
    setUsername(current);
  }, [location.pathname, syncAccessStatus]);

  useEffect(() => {
    const unauthorized = () => handleLogout("timeout", location.pathname, false);
    window.addEventListener("rulenix:unauthorized", unauthorized);
    return () => window.removeEventListener("rulenix:unauthorized", unauthorized);
  }, [handleLogout, location.pathname]);

  useEffect(() => {
    if (!username) {
      return;
    }
    if (hasSessionExpired()) {
      handleLogout("timeout", location.pathname);
      return;
    }

    markSessionActive();

    const activityHandler = () => {
      if (hasSessionExpired()) {
        handleLogout("timeout", location.pathname);
        return;
      }
      markSessionActive();
    };

    const events = ["mousemove", "keydown", "click", "touchstart", "scroll"];
    events.forEach((eventName) =>
      window.addEventListener(eventName, activityHandler)
    );

    const checkInterval = Math.max(Math.floor(SESSION_TIMEOUT_MS / 3), 10000);
    const intervalId = window.setInterval(() => {
      if (hasSessionExpired()) {
        handleLogout("timeout", location.pathname);
      }
    }, checkInterval);

    return () => {
      events.forEach((eventName) =>
        window.removeEventListener(eventName, activityHandler)
      );
      window.clearInterval(intervalId);
    };
  }, [handleLogout, location.pathname, username]);

  useEffect(() => {
    if (sessionReady && !permissions.administer_users && location.pathname.startsWith("/admin")) {
      navigate("/", { replace: true });
    }
  }, [sessionReady, permissions.administer_users, location.pathname, navigate]);

  useEffect(() => {
    if (sessionReady && !permissions.backtesting && location.pathname.startsWith("/backtesting")) {
      navigate("/", { replace: true });
    }
  }, [sessionReady, permissions.backtesting, location.pathname, navigate]);

  if (!sessionReady) {
    return (
      <div className="flex min-h-screen items-center justify-center bg-slate-950 text-sm font-semibold text-slate-300">
        Loading...
      </div>
    );
  }

  if (!username) {
    return null;
  }

  return (
    <div className="min-h-screen bg-gradient-to-br from-slate-950 via-slate-950 to-slate-900">
      <div className="mx-auto flex min-h-screen w-full max-w-7xl flex-col">
        <nav className="flex flex-wrap items-center justify-between gap-6 px-6 py-6">
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 items-center justify-center rounded-full bg-brand-500 text-lg font-bold text-white">
              RX
            </div>
            <div>
              <div className="flex items-center gap-2">
                <p className="text-sm uppercase tracking-[0.4em] text-brand-300">
                  Rulenix
                </p>
                {tradingMode === "live" ? (
                  <span className="rounded-full border border-emerald-400/50 bg-emerald-500/10 px-2 py-[2px] text-[10px] font-semibold uppercase tracking-wide text-emerald-200">
                    Live
                  </span>
                ) : (
                  <span className="rounded-full border border-amber-400/50 bg-amber-500/10 px-2 py-[2px] text-[10px] font-semibold uppercase tracking-wide text-amber-200">
                    Demo
                  </span>
                )}
              </div>
              <p className="text-xs text-slate-400">
                Algorithmic Trading Control Center
              </p>
            </div>
          </div>
          <div className="flex flex-1 items-center justify-center">
            <div className="flex items-center gap-4 rounded-full border border-slate-800 bg-slate-900/60 px-6 py-2 text-xs font-semibold text-slate-400">
              {navItems.map((item) => (
                <NavLink
                  key={item.to}
                  to={item.to}
                  className={({ isActive }) =>
                    `transition hover:text-brand-200 ${
                      isActive ? "text-brand-300" : "text-slate-400"
                    }`
                  }
                  end={item.to === "/"}
                >
                  {item.label}
                </NavLink>
              ))}
            </div>
          </div>
          <div className="flex items-center gap-4">
            <div
              className="rounded-full border border-slate-800 bg-slate-900/60 px-4 py-2 text-xs text-slate-300"
              title={balanceError || "Available account balance"}
            >
              {tradingMode === "live" ? "Live" : "Demo"} balance{" "}
              <span className="font-semibold text-white">
                {accountBalance
                  ? balanceFormatter.format(Number(accountBalance.balance || 0))
                  : "—"}
              </span>
            </div>
            <div className="rounded-full border border-slate-800 bg-slate-900/60 px-4 py-2 text-xs text-slate-300">
              Logged in as{" "}
              <span className="font-semibold text-white">
                {username || "Guest"}
              </span>
            </div>
            <button
              type="button"
              onClick={() => setSettingsOpen(true)}
              className="flex h-9 w-9 items-center justify-center rounded-full border border-slate-700 bg-slate-900 text-lg text-slate-300 transition hover:border-brand-400 hover:text-brand-200"
              title="Account settings"
              aria-label="Account settings"
            >
              ⚙
            </button>
            <button
              type="button"
              onClick={() => handleLogout("manual", location.pathname)}
              className="rounded-full bg-slate-800 px-4 py-2 text-xs font-semibold text-slate-200 transition hover:bg-slate-700"
            >
              Logout
            </button>
          </div>
        </nav>
        <main className="flex-1 px-6 pb-12">
          {typeof children !== "undefined" ? children : <Outlet context={{ session: { username, permissions, tradingMode, ready: sessionReady }, refreshSession: syncAccessStatus }} />}
        </main>
        <footer className="border-t border-slate-800 px-6 py-6 text-xs text-slate-500">
          Built for rapid quant experimentation. © {new Date().getFullYear()}{" "}
          Rulenix.
        </footer>
      </div>
      <AccountSettingsModal
        open={settingsOpen}
        username={username}
        onClose={() => setSettingsOpen(false)}
        onProfileChanged={handleProfileChanged}
        onBalanceChanged={setAccountBalance}
        permissions={permissions}
        tradingMode={tradingMode}
        onTradingModeChanged={(mode) => { setTradingMode(mode); syncAccessStatus(); loadAccountBalance(); }}
      />
    </div>
  );
}
