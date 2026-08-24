import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
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

export default function AdminDailyTradesPage() {
  const [date, setDate] = useState(todayInIndia);
  const [report, setReport] = useState(null);
  const [executions, setExecutions] = useState(null);
  const [isLoading, setIsLoading] = useState(true);
  const [retryingIntent, setRetryingIntent] = useState("");
  const [error, setError] = useState("");

  const loadReport = useCallback(async () => {
    setIsLoading(true);
    setError("");
    try {
      const [tradeResponse, executionResponse] = await Promise.all([
        apiClient.get("/auth/admin/trades/daily/", { params: { date } }),
        apiClient.get("/strategies/admin/executions", { params: { date } }),
      ]);
      setReport(tradeResponse.data);
      setExecutions(executionResponse.data);
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to load the daily trade report.");
    } finally {
      setIsLoading(false);
    }
  }, [date]);

  const retryIntent = async (intentId) => {
    setRetryingIntent(intentId);
    setError("");
    try {
      await apiClient.post("/strategies/admin/executions/retry", {
        intent_id: intentId,
      });
      await loadReport();
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to retry the execution.");
    } finally {
      setRetryingIntent("");
    }
  };

  useEffect(() => {
    loadReport();
  }, [loadReport]);

  const activeUsers = useMemo(
    () => (report?.users || []).filter((user) => user.total_trades > 0).length,
    [report]
  );

  return (
    <div className="space-y-6">
      <header className="flex flex-col justify-between gap-4 sm:flex-row sm:items-end">
        <div>
          <p className="text-xs uppercase tracking-[0.35em] text-brand-300">Administration</p>
          <h1 className="mt-2 text-3xl font-semibold text-white">Daily trades</h1>
          <p className="mt-2 text-sm text-slate-400">Review trades entered by each user on a selected India trading day.</p>
        </div>
        <div className="flex flex-wrap items-end gap-3">
          <label className="text-xs font-semibold uppercase tracking-wide text-slate-400">
            Trading date
            <input type="date" aria-label="Trading date" value={date} onChange={(event) => setDate(event.target.value)} className="mt-1 block h-10 rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm font-normal text-white" />
          </label>
          <button type="button" onClick={loadReport} disabled={isLoading} className="h-10 rounded-lg border border-slate-700 px-4 text-sm font-semibold text-slate-300 hover:border-brand-400 hover:text-brand-200 disabled:opacity-50">{isLoading ? "Refreshing..." : "Refresh"}</button>
        </div>
      </header>

      {error ? <div className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3 text-sm text-rose-200">{error}</div> : null}

      <section className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        {[
          ["Total trades", report?.total_trades ?? 0],
          ["Users who traded", activeUsers],
          ["P&L trades", report?.pnl_trades ?? 0],
          ["Backtest trades", report?.backtest_trades ?? 0],
        ].map(([label, value]) => (
          <div key={label} className="rounded-xl border border-slate-800 bg-slate-900/70 px-4 py-4">
            <p className="text-xs uppercase tracking-wide text-slate-500">{label}</p>
            <p className="mt-1 text-2xl font-semibold text-white">{value}</p>
          </div>
        ))}
      </section>

      <section className="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/70 shadow-lg shadow-black/20">
        <div className="flex flex-col gap-3 border-b border-slate-800 px-5 py-4 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h2 className="font-semibold text-white">Signal execution delivery</h2>
            <p className="mt-1 text-xs text-slate-500">Every confirmed signal is matched to an immutable list of eligible users. Failures remain visible and retryable.</p>
          </div>
          <div className="flex flex-wrap gap-2 text-xs">
            {[
              ["Expected", executions?.totals?.expected_users ?? 0],
              ["Waiting", executions?.totals?.pending ?? 0],
              ["Submitted", executions?.totals?.submitted ?? 0],
              ["Completed", executions?.totals?.completed ?? 0],
              ["Failed", executions?.totals?.failed ?? 0],
            ].map(([label, value]) => (
              <span key={label} className="rounded-full border border-slate-700 bg-slate-950/60 px-3 py-1 text-slate-300">{label}: <strong className="text-white">{value}</strong></span>
            ))}
          </div>
        </div>
        <div className="overflow-x-auto">
          <table className="min-w-[1050px] w-full divide-y divide-slate-800 text-left text-sm text-slate-200">
            <thead className="bg-slate-950/50 text-xs uppercase tracking-wide text-slate-500">
              <tr><th className="px-5 py-3">Signal</th><th className="px-4 py-3">Instrument</th><th className="px-4 py-3">Time</th><th className="px-4 py-3 text-right">Expected</th><th className="px-4 py-3 text-right">Waiting</th><th className="px-4 py-3 text-right">Delivered</th><th className="px-4 py-3 text-right">Skipped</th><th className="px-5 py-3 text-right">Failed</th></tr>
            </thead>
            <tbody className="divide-y divide-slate-800">
              {(executions?.signals || []).map((signal) => (
                <Fragment key={signal.signal_id}>
                  <tr>
                    <td className="px-5 py-4"><p className="font-semibold text-white">{signal.strategy_key}</p><p className="mt-1 text-xs text-slate-500">{signal.signal_type} · {signal.status}</p></td>
                    <td className="px-4 py-4">{signal.instrument}</td>
                    <td className="px-4 py-4 text-xs text-slate-400">{new Date(signal.signal_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })}</td>
                    <td className="px-4 py-4 text-right">{signal.expected_users}</td>
                    <td className="px-4 py-4 text-right">{signal.pending}</td>
                    <td className="px-4 py-4 text-right text-emerald-300">{Number(signal.submitted) + Number(signal.completed)}</td>
                    <td className="px-4 py-4 text-right">{signal.skipped}</td>
                    <td className="px-5 py-4 text-right text-rose-300">{signal.failed}</td>
                  </tr>
                  {(signal.intents || []).filter((intent) => ["failed", "expired", "retry_wait"].includes(intent.status)).map((intent) => (
                    <tr key={intent.intent_id} className="bg-rose-500/5">
                      <td className="px-5 py-3 text-xs text-slate-300" colSpan={3}>{intent.username} · {intent.instrument} · {intent.role}<p className="mt-1 max-w-3xl text-rose-300">{intent.last_error || "Waiting for automatic retry."}</p></td>
                      <td className="px-4 py-3 text-right text-xs text-slate-400" colSpan={3}>Attempts: {intent.attempts}</td>
                      <td className="px-5 py-3 text-right" colSpan={2}>
                        {intent.action === "ENTRY" && intent.status !== "expired" ? <button type="button" onClick={() => retryIntent(intent.intent_id)} disabled={retryingIntent === intent.intent_id} className="rounded-lg border border-rose-400/50 px-3 py-1.5 text-xs font-semibold text-rose-200 hover:bg-rose-400/10 disabled:opacity-50">{retryingIntent === intent.intent_id ? "Retrying..." : "Retry now"}</button> : <span className="text-xs text-slate-500">{intent.status === "expired" ? "Execution window closed" : "Automatic square-off retry"}</span>}
                      </td>
                    </tr>
                  ))}
                </Fragment>
              ))}
              {!isLoading && (executions?.signals || []).length === 0 ? <tr><td colSpan={8} className="px-5 py-10 text-center text-sm text-slate-500">No confirmed strategy signals for this date.</td></tr> : null}
            </tbody>
          </table>
        </div>
      </section>

      <section className="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/70 shadow-lg shadow-black/20">
        <div className="border-b border-slate-800 px-5 py-4">
          <h2 className="font-semibold text-white">Trades by user</h2>
          <p className="mt-1 text-xs text-slate-500">Counts use trade entry time in Asia/Kolkata. Users with no trades are included.</p>
        </div>
        {isLoading && !report ? (
          <div className="px-5 py-10 text-center text-sm text-slate-400">Loading trade report...</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="min-w-[900px] w-full divide-y divide-slate-800 text-left text-sm text-slate-200">
              <thead className="bg-slate-950/50 text-xs uppercase tracking-wide text-slate-500">
                <tr><th className="px-5 py-3">User</th><th className="px-4 py-3 text-right">Total</th><th className="px-4 py-3 text-right">P&amp;L</th><th className="px-4 py-3 text-right">Backtest</th><th className="px-4 py-3 text-right">Demo</th><th className="px-4 py-3 text-right">Live</th><th className="px-4 py-3 text-right">Open</th><th className="px-5 py-3 text-right">Closed</th></tr>
              </thead>
              <tbody className="divide-y divide-slate-800">
                {(report?.users || []).map((user) => (
                  <tr key={user.user_id} className={user.total_trades === 0 ? "text-slate-500" : ""}>
                    <td className="px-5 py-4 font-semibold text-white">{user.username}</td>
                    <td className="px-4 py-4 text-right font-semibold">{user.total_trades}</td>
                    <td className="px-4 py-4 text-right">{user.pnl_trades}</td>
                    <td className="px-4 py-4 text-right">{user.backtest_trades}</td>
                    <td className="px-4 py-4 text-right">{user.demo_trades}</td>
                    <td className="px-4 py-4 text-right">{user.live_trades}</td>
                    <td className="px-4 py-4 text-right">{user.open_trades}</td>
                    <td className="px-5 py-4 text-right">{user.closed_trades}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}
