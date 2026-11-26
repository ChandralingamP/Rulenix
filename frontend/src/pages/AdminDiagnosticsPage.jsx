import { useCallback, useEffect, useState } from "react";
import apiClient from "../utils/axiosConfig.js";

function todayInIndia() {
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone: "Asia/Kolkata",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).formatToParts(new Date());
  const value = Object.fromEntries(parts.map(({ type, value: part }) => [type, part]));
  return `${value.year}-${value.month}-${value.day}`;
}

function formatTimeIst(value) {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleString("en-IN", {
    timeZone: "Asia/Kolkata",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    day: "2-digit",
    month: "short",
    hour12: false,
  });
}

function formatPrice(value) {
  if (value === null || value === undefined || value === "") return "—";
  const num = Number(value);
  return Number.isFinite(num)
    ? num.toLocaleString("en-IN", {
        minimumFractionDigits: 2,
        maximumFractionDigits: 2,
      })
    : "—";
}

function SeverityBadge({ severity }) {
  const level = String(severity || "INFO").toUpperCase();
  const styles =
    level === "ERROR"
      ? "bg-rose-500/15 text-rose-300 border-rose-500/30"
      : level === "WARNING"
        ? "bg-amber-500/15 text-amber-300 border-amber-500/30"
        : "bg-sky-500/15 text-sky-300 border-sky-500/30";
  return (
    <span className={`inline-flex rounded-full border px-2.5 py-0.5 text-[11px] font-semibold ${styles}`}>
      {level}
    </span>
  );
}

export default function AdminDiagnosticsPage() {
  const [date, setDate] = useState(todayInIndia);
  const [strategyKey, setStrategyKey] = useState("");
  const [severity, setSeverity] = useState("");
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [data, setData] = useState(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState("");

  const loadDiagnostics = useCallback(async ({ silent = false } = {}) => {
    if (!silent) setIsLoading(true);
    setError("");
    try {
      const params = { date };
      if (strategyKey) params.strategy_key = strategyKey;
      if (severity) params.severity = severity;
      const response = await apiClient.get("/auth/admin/diagnostics/", { params });
      setData(response.data);
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to load strategy diagnostics.");
    } finally {
      if (!silent) setIsLoading(false);
    }
  }, [date, severity, strategyKey]);

  useEffect(() => {
    loadDiagnostics();
  }, [loadDiagnostics]);

  useEffect(() => {
    if (!autoRefresh) return undefined;
    const id = window.setInterval(() => loadDiagnostics({ silent: true }), 15000);
    return () => window.clearInterval(id);
  }, [autoRefresh, loadDiagnostics]);

  const supertrendLevels = data?.supertrend_levels || {};
  const reasonCounts = Object.entries(data?.summary?.by_reason || {});
  const events = data?.events || [];

  return (
    <div className="space-y-6">
      <header className="flex flex-col justify-between gap-4 sm:flex-row sm:items-end">
        <div>
          <p className="text-xs uppercase tracking-[0.35em] text-brand-300">Administration</p>
          <h1 className="mt-2 text-3xl font-semibold text-white">Strategy diagnostics</h1>
          <p className="mt-2 text-sm text-slate-400">
            Inspect why strategies skipped trades and verify live SuperTrend trend-line prices.
          </p>
        </div>
        <div className="flex flex-wrap items-end gap-3">
          <label className="text-xs font-semibold uppercase tracking-wide text-slate-400">
            Date
            <input
              type="date"
              aria-label="Diagnostics date"
              value={date}
              onChange={(event) => setDate(event.target.value)}
              className="mt-1 block h-10 rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm font-normal text-white"
            />
          </label>
          <label className="text-xs font-semibold uppercase tracking-wide text-slate-400">
            Strategy
            <select
              aria-label="Filter by strategy"
              value={strategyKey}
              onChange={(event) => setStrategyKey(event.target.value)}
              className="mt-1 block h-10 rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm font-normal text-white"
            >
              <option value="">All strategies</option>
              <option value="supertrend_index_options_v1">SuperTrend Index Options v1</option>
              <option value="futures_breakout_v3">Futures Breakout v3</option>
              <option value="nifty_opponent_v1">Nifty Opponent v1</option>
            </select>
          </label>
          <label className="text-xs font-semibold uppercase tracking-wide text-slate-400">
            Severity
            <select
              aria-label="Filter by severity"
              value={severity}
              onChange={(event) => setSeverity(event.target.value)}
              className="mt-1 block h-10 rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm font-normal text-white"
            >
              <option value="">All severities</option>
              <option value="WARNING">WARNING</option>
              <option value="ERROR">ERROR</option>
              <option value="INFO">INFO</option>
            </select>
          </label>
          <label className="flex h-10 items-center gap-2 rounded-lg border border-slate-800 bg-slate-900/70 px-3 text-xs font-semibold text-slate-300">
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(event) => setAutoRefresh(event.target.checked)}
            />
            Auto-refresh
          </label>
          <button
            type="button"
            onClick={() => loadDiagnostics()}
            disabled={isLoading}
            className="h-10 rounded-lg border border-slate-700 px-4 text-sm font-semibold text-slate-300 hover:border-brand-400 hover:text-brand-200 disabled:opacity-50"
          >
            {isLoading ? "Refreshing..." : "Refresh"}
          </button>
        </div>
      </header>

      {error ? (
        <div className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3 text-sm text-rose-200">
          {error}
        </div>
      ) : null}

      <section className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <div className="rounded-xl border border-slate-800 bg-slate-900/70 px-4 py-4">
          <p className="text-xs uppercase tracking-wide text-slate-500">Diagnostic events</p>
          <p className="mt-1 text-2xl font-semibold text-white">{data?.summary?.total ?? 0}</p>
        </div>
        <div className="rounded-xl border border-slate-800 bg-slate-900/70 px-4 py-4">
          <p className="text-xs uppercase tracking-wide text-slate-500">Warnings</p>
          <p className="mt-1 text-2xl font-semibold text-amber-300">{data?.summary?.warnings ?? 0}</p>
        </div>
        {["NIFTY", "SENSEX"].map((instrument) => {
          const level = supertrendLevels[instrument];
          return (
            <div key={instrument} className="rounded-xl border border-slate-800 bg-slate-900/70 px-4 py-4">
              <div className="flex items-center justify-between gap-2">
                <p className="text-xs uppercase tracking-wide text-slate-500">
                  {instrument} SuperTrend line
                </p>
                {level?.supertrend_direction ? (
                  <span
                    className={`rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase ${
                      level.supertrend_direction === "UP"
                        ? "bg-emerald-500/15 text-emerald-300"
                        : "bg-rose-500/15 text-rose-300"
                    }`}
                  >
                    {level.supertrend_direction}
                  </span>
                ) : null}
              </div>
              <p className="mt-1 text-2xl font-semibold text-white">
                {level?.supertrend_price != null ? formatPrice(level.supertrend_price) : "—"}
              </p>
              <p className="mt-1 text-xs text-slate-400">
                {level?.supertrend_candle_at
                  ? `Close ${formatPrice(level.index_close)} · ${formatTimeIst(level.supertrend_candle_at)}`
                  : "Waiting for completed 5m candles"}
              </p>
            </div>
          );
        })}
      </section>

      {reasonCounts.length > 0 ? (
        <section className="rounded-xl border border-slate-800 bg-slate-900/70 px-5 py-4">
          <p className="text-xs font-semibold uppercase tracking-wide text-slate-500">
            Skip reasons breakdown
          </p>
          <div className="mt-3 flex flex-wrap gap-2">
            {reasonCounts.map(([reason, count]) => (
              <span
                key={reason}
                className="rounded-full border border-slate-700 bg-slate-950/70 px-3 py-1 text-xs text-slate-300"
              >
                {reason}: <strong className="text-white">{count}</strong>
              </span>
            ))}
          </div>
        </section>
      ) : null}

      <section className="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/70 shadow-lg shadow-black/20">
        <div className="border-b border-slate-800 px-5 py-4">
          <h2 className="font-semibold text-white">Diagnostic event log</h2>
          <p className="mt-1 text-xs text-slate-500">
            Throttled strategy skip diagnostics and operational alerts persisted from runtime cycles (Asia/Kolkata).
          </p>
        </div>
        <div className="overflow-x-auto">
          <table className="min-w-[960px] w-full divide-y divide-slate-800 text-left text-sm text-slate-200">
            <thead className="bg-slate-950/50 text-xs uppercase tracking-wide text-slate-500">
              <tr>
                <th className="px-5 py-3">Time (IST)</th>
                <th className="px-4 py-3">Severity</th>
                <th className="px-4 py-3">Strategy</th>
                <th className="px-4 py-3">Instrument</th>
                <th className="px-4 py-3">Reason</th>
                <th className="px-5 py-3">Details</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-slate-800">
              {events.map((event) => {
                const contextEntries = Object.entries(event.context || {});
                return (
                  <tr key={event.id}>
                    <td className="whitespace-nowrap px-5 py-3 text-xs text-slate-400">
                      {formatTimeIst(event.created_at)}
                    </td>
                    <td className="px-4 py-3">
                      <SeverityBadge severity={event.severity} />
                    </td>
                    <td className="px-4 py-3 font-medium text-white">{event.strategy_key}</td>
                    <td className="px-4 py-3">{event.instrument || "—"}</td>
                    <td className="px-4 py-3 font-mono text-xs text-brand-200">{event.reason}</td>
                    <td className="px-5 py-3 text-xs text-slate-300">
                      <p>{event.message}</p>
                      {contextEntries.length > 0 || event.username ? (
                        <div className="mt-1 flex flex-wrap gap-1.5">
                          {event.username ? (
                            <span className="rounded bg-slate-800 px-2 py-0.5 text-[11px] text-slate-300">
                              user={event.username}
                            </span>
                          ) : null}
                          {contextEntries.map(([key, val]) => (
                            <span
                              key={key}
                              className="rounded bg-slate-800 px-2 py-0.5 font-mono text-[11px] text-slate-300"
                            >
                              {key}={String(val)}
                            </span>
                          ))}
                        </div>
                      ) : null}
                    </td>
                  </tr>
                );
              })}
              {!isLoading && events.length === 0 ? (
                <tr>
                  <td colSpan={6} className="px-5 py-10 text-center text-sm text-slate-500">
                    No diagnostic logs recorded for the selected filters.
                  </td>
                </tr>
              ) : null}
            </tbody>
          </table>
        </div>
      </section>
    </div>
  );
}
