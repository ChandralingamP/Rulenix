import { useEffect, useState } from "react";
import { useDispatch, useSelector } from "react-redux";
import {
  connectBrokerage,
  fetchAccountStatus,
  hydrateFromCache as hydrateHomeFromCache,
  updateApiKey,
} from "../features/home/homeSlice";
import { getAuthUsername } from "../utils/authCookies.js";
import {
  CACHE_TTL_MS,
  getCacheEntry,
  isCacheEntryFresh,
} from "../utils/dataCache.js";

const HOME_CACHE_NAMESPACE = "home_status";

const defaultFormState = {
  mpin: "",
  totp: "",
};

export default function HomePage() {
  const dispatch = useDispatch();
  const authUsername = getAuthUsername();
  const { details, status, error, connection, profileUpdate } = useSelector(
    (state) => state.home
  );
  const [formState, setFormState] = useState(defaultFormState);
  const [editingApiKey, setEditingApiKey] = useState(false);
  const [newApiKey, setNewApiKey] = useState("");

  useEffect(() => {
    if (!authUsername) {
      return undefined;
    }

    const cacheEntry = getCacheEntry(HOME_CACHE_NAMESPACE, authUsername);
    if (cacheEntry?.value) {
      dispatch(hydrateHomeFromCache(cacheEntry.value));
    }

    const age = cacheEntry
      ? Date.now() - cacheEntry.timestamp
      : Number.POSITIVE_INFINITY;

    const triggerFetch = () => {
      dispatch(fetchAccountStatus(authUsername));
    };

    if (!cacheEntry || !isCacheEntryFresh(cacheEntry)) {
      triggerFetch();
      return undefined;
    }

    let timeoutId;
    if (typeof window !== "undefined") {
      const delay = Math.max(CACHE_TTL_MS - age, 0);
      timeoutId = window.setTimeout(triggerFetch, delay);
    }

    return () => {
      if (timeoutId) {
        window.clearTimeout(timeoutId);
      }
    };
  }, [dispatch, authUsername]);

  const handleChange = (event) => {
    const { name, value } = event.target;
    setFormState((prev) => ({ ...prev, [name]: value }));
  };

  const handleSubmit = (event) => {
    event.preventDefault();
    if (!formState.mpin || !formState.totp) {
      return;
    }
    if (!authUsername) {
      return;
    }
    dispatch(connectBrokerage({ ...formState, username: authUsername })).then(
      (action) => {
        if (action.meta.requestStatus === "fulfilled") {
          setFormState(defaultFormState);
        }
      }
    );
  };

  const isInitialLoading = status === "loading" && !details;
  const detailsSnapshot = details || {};

  const lastUpdated =
    detailsSnapshot.last_updated || detailsSnapshot.last_connected_at;
  const connectionState = (
    detailsSnapshot.connection_state || ""
  ).toLowerCase();
  const effectiveConnectionState = connectionState
    ? connectionState
    : connection.status === "succeeded"
    ? "connected"
    : connection.status === "failed"
    ? "failed"
    : "idle";

  const badgeClass =
    effectiveConnectionState === "connected"
      ? "bg-emerald-500/20 text-emerald-300"
      : effectiveConnectionState === "expired" ||
        effectiveConnectionState === "invalid" ||
        effectiveConnectionState === "failed"
      ? "bg-rose-500/20 text-rose-300"
      : effectiveConnectionState === "unavailable"
      ? "bg-amber-500/20 text-amber-300"
      : "bg-slate-700/60 text-slate-300";

  const badgeLabel =
    effectiveConnectionState === "connected"
      ? "Connected"
      : effectiveConnectionState === "expired"
      ? "Expired"
      : effectiveConnectionState === "invalid"
      ? "Token invalid"
      : effectiveConnectionState === "unavailable"
      ? "Verification unavailable"
      : effectiveConnectionState === "failed"
      ? "Disconnected"
      : "Idle";

  const lastConnected =
    detailsSnapshot.last_connected_at || connection.lastConnectedAt || null;
  const connectionMessage =
    detailsSnapshot.connection_message ||
    (connection.status !== "idle" && connection.message
      ? connection.message
      : null);

  return (
    <div className="mx-auto flex w-full max-w-5xl flex-col gap-8">
      <section className="rounded-3xl border border-slate-800 bg-slate-900/60 p-6 shadow-xl shadow-black/40 backdrop-blur">
        <header className="mb-6 flex flex-col gap-2">
          <p className="text-xs uppercase tracking-[0.4em] text-brand-300">
            Account
          </p>
          <h1 className="text-3xl font-semibold text-white">
            Brokerage Connection Overview
          </h1>
          <p className="text-sm text-slate-400">
            Review your client credentials and initiate a fresh brokerage
            session using MPIN and TOTP.
          </p>
        </header>

        <div className="grid gap-6 lg:grid-cols-[1.2fr,1fr]">
          <article className="rounded-2xl border border-slate-800 bg-slate-950/70 p-5">
            <div className="flex items-center justify-between">
              <h2 className="text-lg font-semibold text-white">
                Client Profile
              </h2>
              <span
                className={`rounded-full px-3 py-1 text-xs font-semibold ${badgeClass}`}
              >
                {badgeLabel}
              </span>
            </div>

            <dl className="mt-6 space-y-3 text-sm text-slate-200">
              <div className="flex justify-between">
                <dt className="text-slate-400">Client ID</dt>
                <dd className="font-medium">
                  {isInitialLoading
                    ? "Loading..."
                    : detailsSnapshot.client_id || "—"}
                </dd>
              </div>
              <div className="flex justify-between">
                <dt className="text-slate-400">API Key</dt>
                <dd className="flex items-center gap-2 font-medium">
                  {detailsSnapshot.api_key_configured ? "Configured securely" : "Not configured"}
                  <button
                    type="button"
                    onClick={() => {
                      setEditingApiKey((prev) => !prev);
                      setNewApiKey("");
                    }}
                    className="rounded p-1 text-slate-400 transition hover:text-brand-300"
                    title="Edit API Key"
                  >
                    <svg
                      xmlns="http://www.w3.org/2000/svg"
                      className="h-3.5 w-3.5"
                      viewBox="0 0 20 20"
                      fill="currentColor"
                    >
                      <path d="M13.586 3.586a2 2 0 112.828 2.828l-.793.793-2.828-2.828.793-.793zM11.379 5.793L3 14.172V17h2.828l8.38-8.379-2.83-2.828z" />
                    </svg>
                  </button>
                </dd>
              </div>
              {editingApiKey && (
                <div className="mt-2 rounded-lg border border-slate-700 bg-slate-900/80 p-3">
                  <label
                    className="mb-1 block text-xs font-medium text-slate-400"
                    htmlFor="new-api-key"
                  >
                    New API Key
                  </label>
                  <div className="flex gap-2">
                    <input
                      id="new-api-key"
                      type="text"
                      value={newApiKey}
                      onChange={(e) => setNewApiKey(e.target.value)}
                      placeholder="Enter new Angel One API key"
                      className="flex-1 rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-1.5 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring focus:ring-brand-500/20"
                      maxLength={128}
                    />
                    <button
                      type="button"
                      disabled={
                        !newApiKey.trim() ||
                        profileUpdate.status === "loading"
                      }
                      onClick={() => {
                        if (!authUsername || !newApiKey.trim()) return;
                        dispatch(
                          updateApiKey({
                            username: authUsername,
                            api_key: newApiKey.trim(),
                          })
                        ).then((action) => {
                          if (action.meta.requestStatus === "fulfilled") {
                            setEditingApiKey(false);
                            setNewApiKey("");
                          }
                        });
                      }}
                      className="rounded-lg bg-brand-500 px-3 py-1.5 text-xs font-semibold text-white shadow-lg shadow-brand-500/30 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:bg-slate-600"
                    >
                      {profileUpdate.status === "loading"
                        ? "Saving..."
                        : "Save"}
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        setEditingApiKey(false);
                        setNewApiKey("");
                      }}
                      className="rounded-lg border border-slate-700 px-3 py-1.5 text-xs font-medium text-slate-300 transition hover:bg-slate-800"
                    >
                      Cancel
                    </button>
                  </div>
                  {profileUpdate.status === "succeeded" &&
                    profileUpdate.message && (
                      <p className="mt-2 text-xs text-emerald-400">
                        {profileUpdate.message}
                      </p>
                    )}
                  {profileUpdate.status === "failed" &&
                    profileUpdate.message && (
                      <p className="mt-2 text-xs text-rose-400">
                        {profileUpdate.message}
                      </p>
                    )}
                </div>
              )}
              <div className="flex justify-between">
                <dt className="text-slate-400">Session Status</dt>
                <dd className="font-medium">{badgeLabel}</dd>
              </div>
              <div className="flex justify-between">
                <dt className="text-slate-400">Last Connected</dt>
                <dd className="font-medium">
                  {lastConnected
                    ? new Date(lastConnected).toLocaleString()
                    : "—"}
                </dd>
              </div>
              <div className="flex justify-between">
                <dt className="text-slate-400">Last Updated</dt>
                <dd className="font-medium">
                  {lastUpdated ? new Date(lastUpdated).toLocaleString() : "—"}
                </dd>
              </div>
            </dl>
            {connectionMessage ? (
              <p
                className={`mt-4 rounded-lg border px-3 py-2 text-xs ${
                  effectiveConnectionState === "expired" ||
                  effectiveConnectionState === "invalid" ||
                  effectiveConnectionState === "failed"
                    ? "border-rose-500/30 bg-rose-500/10 text-rose-200"
                    : effectiveConnectionState === "unavailable"
                    ? "border-amber-500/30 bg-amber-500/10 text-amber-200"
                    : "border-slate-700 bg-slate-900/50 text-slate-400"
                }`}
              >
                {connectionMessage}
              </p>
            ) : null}
            {error ? (
              <p className="mt-4 text-xs text-rose-400">{error}</p>
            ) : null}
          </article>

          <article className="rounded-2xl border border-slate-800 bg-slate-950/70 p-5">
            <h2 className="text-lg font-semibold text-white">
              Establish Session
            </h2>
            <p className="mt-1 text-sm text-slate-400">
              Submit MPIN and TOTP to connect your brokerage session securely.
            </p>
            <form className="mt-4 space-y-4" onSubmit={handleSubmit}>
              <div className="space-y-2">
                <label
                  className="text-sm font-medium text-slate-300"
                  htmlFor="mpin"
                >
                  MPIN
                </label>
                <input
                  id="mpin"
                  name="mpin"
                  type="password"
                  value={formState.mpin}
                  onChange={handleChange}
                  placeholder="••••"
                  className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring focus:ring-brand-500/20"
                  required
                />
              </div>
              <div className="space-y-2">
                <label
                  className="text-sm font-medium text-slate-300"
                  htmlFor="totp"
                >
                  TOTP
                </label>
                <input
                  id="totp"
                  name="totp"
                  type="text"
                  inputMode="numeric"
                  pattern="[0-9]*"
                  value={formState.totp}
                  onChange={handleChange}
                  placeholder="123456"
                  className="w-full rounded-lg border border-slate-700 bg-slate-950/70 px-3 py-2 text-sm text-white focus:border-brand-400 focus:outline-none focus:ring focus:ring-brand-500/20"
                  maxLength={8}
                  required
                />
              </div>
              <button
                type="submit"
                disabled={connection.status === "loading"}
                className="w-full rounded-lg bg-brand-500 px-3 py-2 text-sm font-semibold text-white shadow-lg shadow-brand-500/30 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:bg-slate-600"
              >
                {connection.status === "loading" ? "Connecting..." : "Connect"}
              </button>
              {connection.message ? (
                <p
                  className={`text-xs ${
                    connection.status === "failed"
                      ? "text-rose-400"
                      : "text-emerald-400"
                  }`}
                >
                  {connection.message}
                </p>
              ) : null}
            </form>
          </article>
        </div>
      </section>
    </div>
  );
}
