import { useCallback, useEffect, useMemo, useState } from "react";
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
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState("");

  const loadReport = useCallback(async () => {
    setIsLoading(true);
    setError("");
    try {
      const response = await apiClient.get("/auth/admin/trades/daily/", {
        params: { date },
      });
      setReport(response.data);
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to load the daily trade report.");
    } finally {
      setIsLoading(false);
    }
  }, [date]);

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
          ["Demo trades", report?.demo_trades ?? 0],
          ["Live trades", report?.live_trades ?? 0],
        ].map(([label, value]) => (
          <div key={label} className="rounded-xl border border-slate-800 bg-slate-900/70 px-4 py-4">
            <p className="text-xs uppercase tracking-wide text-slate-500">{label}</p>
            <p className="mt-1 text-2xl font-semibold text-white">{value}</p>
          </div>
        ))}
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
            <table className="min-w-[720px] w-full divide-y divide-slate-800 text-left text-sm text-slate-200">
              <thead className="bg-slate-950/50 text-xs uppercase tracking-wide text-slate-500">
                <tr><th className="px-5 py-3">User</th><th className="px-4 py-3 text-right">Total</th><th className="px-4 py-3 text-right">Demo</th><th className="px-4 py-3 text-right">Live</th><th className="px-4 py-3 text-right">Open</th><th className="px-5 py-3 text-right">Closed</th></tr>
              </thead>
              <tbody className="divide-y divide-slate-800">
                {(report?.users || []).map((user) => (
                  <tr key={user.user_id} className={user.total_trades === 0 ? "text-slate-500" : ""}>
                    <td className="px-5 py-4 font-semibold text-white">{user.username}</td>
                    <td className="px-4 py-4 text-right font-semibold">{user.total_trades}</td>
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
