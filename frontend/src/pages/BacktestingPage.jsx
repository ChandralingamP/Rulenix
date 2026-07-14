import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate, useOutletContext } from "react-router-dom";
import apiClient from "../utils/axiosConfig.js";

const currency = new Intl.NumberFormat("en-IN", {
  style: "currency",
  currency: "INR",
  maximumFractionDigits: 0,
});

const number = new Intl.NumberFormat("en-IN", {
  maximumFractionDigits: 2,
});

const intervals = [
  ["FIFTEEN_MINUTE", "15 min"],
  ["THIRTY_MINUTE", "30 min"],
  ["ONE_HOUR", "1 hour"],
  ["FIVE_MINUTE", "5 min"],
  ["ONE_MINUTE", "1 min"],
];

function formatDateTime(value) {
  if (!value) return "-";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString("en-IN", {
    timeZone: "Asia/Kolkata",
    day: "2-digit",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function Metric({ label, value, tone = "slate" }) {
  const tones = {
    slate: "text-white",
    good: "text-emerald-300",
    bad: "text-rose-300",
    info: "text-sky-300",
  };
  return (
    <div className="rounded-lg border border-slate-800 bg-slate-950/60 px-4 py-3">
      <p className="text-xs uppercase tracking-wide text-slate-500">{label}</p>
      <p className={`mt-1 text-lg font-semibold ${tones[tone] || tones.slate}`}>
        {value}
      </p>
    </div>
  );
}

export default function BacktestingPage() {
  const { session } = useOutletContext();
  const navigate = useNavigate();
  const [form, setForm] = useState({
    instrument: "GOLDTEN",
    interval: "FIFTEEN_MINUTE",
    lookback_months: 3,
    lots: 1,
  });
  const [history, setHistory] = useState([]);
  const [result, setResult] = useState(null);
  const [status, setStatus] = useState("idle");
  const [error, setError] = useState("");

  const canBacktest = Boolean(session?.permissions?.backtesting);

  const loadHistory = useCallback(async () => {
    if (!canBacktest) return;
    try {
      const response = await apiClient.get("/backtesting/runs");
      setHistory(Array.isArray(response.data?.runs) ? response.data.runs : []);
    } catch (requestError) {
      setError(
        requestError.response?.data?.detail || "Unable to load backtest history."
      );
    }
  }, [canBacktest]);

  useEffect(() => {
    if (!session?.ready) return;
    if (!canBacktest) {
      navigate("/", { replace: true });
      return;
    }
    loadHistory();
  }, [canBacktest, loadHistory, navigate, session?.ready]);

  const latestSummary = result?.run?.summary || history[0]?.summary || null;
  const recentTrades = useMemo(
    () => (Array.isArray(result?.trades) ? result.trades.slice(-10).reverse() : []),
    [result]
  );

  const update = (key, value) => {
    setForm((current) => ({ ...current, [key]: value }));
  };

  const runBacktest = async (event) => {
    event.preventDefault();
    setStatus("running");
    setError("");
    setResult(null);
    try {
      const response = await apiClient.post("/backtesting/run", {
        instrument: form.instrument,
        interval: form.interval,
        lookback_months: Number(form.lookback_months),
        lots: Number(form.lots),
      });
      setResult(response.data);
      await loadHistory();
      setStatus("succeeded");
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Backtest failed.");
      setStatus("failed");
    }
  };

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-6">
      <header className="flex flex-col justify-between gap-4 sm:flex-row sm:items-end">
        <div>
          <p className="text-xs uppercase tracking-[0.4em] text-brand-300">
            Strategy research
          </p>
          <h1 className="mt-2 text-3xl font-semibold text-white">Backtesting</h1>
          <p className="mt-2 text-sm text-slate-400">
            Run Futures Breakout v3 on GOLDTEN historical candles with reusable market data.
          </p>
        </div>
        <button
          type="button"
          onClick={loadHistory}
          disabled={status === "running"}
          className="self-start rounded-full border border-slate-700 bg-slate-900/60 px-4 py-2 text-xs font-semibold text-slate-300 transition hover:border-brand-400 hover:text-brand-200 disabled:cursor-wait disabled:opacity-50"
        >
          Refresh
        </button>
      </header>

      {error ? (
        <div className="rounded-xl border border-rose-500/30 bg-rose-500/10 px-4 py-3 text-sm text-rose-200">
          {error}
        </div>
      ) : null}

      <section className="grid gap-5 lg:grid-cols-[360px_1fr]">
        <form
          onSubmit={runBacktest}
          className="space-y-4 rounded-xl border border-slate-800 bg-slate-900/70 p-5"
        >
          <div>
            <h2 className="text-lg font-semibold text-white">Run setup</h2>
            <p className="mt-1 text-sm text-slate-400">
              Historical candles are cached by symbol and interval for future runs.
            </p>
          </div>

          <div className="rounded-lg border border-slate-800 bg-slate-950/60 px-3 py-3">
            <p className="text-xs font-semibold uppercase tracking-wide text-slate-500">
              Strategy
            </p>
            <p className="mt-1 text-sm font-semibold text-white">
              Futures Breakout v3
            </p>
            <p className="mt-1 text-xs text-slate-500">
              futures_breakout_v3
            </p>
          </div>

          <label className="block text-xs font-semibold uppercase tracking-wide text-slate-500">
            Instrument
            <select
              value={form.instrument}
              onChange={(event) => update("instrument", event.target.value)}
              className="mt-2 h-10 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm normal-case tracking-normal text-white"
            >
              <option value="GOLDTEN">GOLDTEN</option>
            </select>
          </label>

          <label className="block text-xs font-semibold uppercase tracking-wide text-slate-500">
            Interval
            <select
              value={form.interval}
              onChange={(event) => update("interval", event.target.value)}
              className="mt-2 h-10 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm normal-case tracking-normal text-white"
            >
              {intervals.map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </select>
          </label>

          <div className="grid grid-cols-2 gap-3">
            <label className="block text-xs font-semibold uppercase tracking-wide text-slate-500">
              Lookback
              <select
                value={form.lookback_months}
                onChange={(event) => update("lookback_months", event.target.value)}
                className="mt-2 h-10 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm normal-case tracking-normal text-white"
              >
                <option value={1}>1 month</option>
                <option value={3}>3 months</option>
                <option value={6}>6 months</option>
              </select>
            </label>
            <label className="block text-xs font-semibold uppercase tracking-wide text-slate-500">
              Lots
              <input
                type="number"
                min="1"
                step="1"
                value={form.lots}
                onChange={(event) => update("lots", event.target.value)}
                className="mt-2 h-10 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm normal-case tracking-normal text-white"
              />
            </label>
          </div>

          <button
            type="submit"
            disabled={status === "running" || Number(form.lots) <= 0}
            className="w-full rounded-lg bg-brand-500 px-4 py-3 text-sm font-semibold text-white shadow-lg shadow-brand-500/20 transition hover:bg-brand-400 disabled:cursor-wait disabled:bg-slate-700"
          >
            {status === "running" ? "Running backtest..." : "Run backtest"}
          </button>
        </form>

        <div className="space-y-5">
          <section className="rounded-xl border border-slate-800 bg-slate-900/70 p-5">
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div>
                <h2 className="text-lg font-semibold text-white">Summary</h2>
                <p className="mt-1 text-sm text-slate-400">
                  {result?.run
                    ? `${result.run.trading_symbol} from ${formatDateTime(result.run.from_time)}`
                    : history[0]
                      ? `${history[0].trading_symbol} from ${formatDateTime(history[0].from_time)}`
                      : "No backtest has been run yet."}
                </p>
              </div>
              {result?.run ? (
                <span className="rounded-full bg-sky-500/10 px-3 py-1 text-xs font-semibold text-sky-300">
                  {number.format(result.run.reused_points || 0)} reused /{" "}
                  {number.format(result.run.fetched_points || 0)} fetched
                </span>
              ) : null}
            </div>

            {latestSummary ? (
              <div className="mt-5 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
                <Metric
                  label="Realized P&L"
                  value={currency.format(Number(latestSummary.net_pnl || 0))}
                  tone={Number(latestSummary.net_pnl || 0) >= 0 ? "good" : "bad"}
                />
                <Metric label="Trades" value={latestSummary.trades || 0} />
                <Metric
                  label="Avg / trade"
                  value={currency.format(Number(latestSummary.average_pnl || 0))}
                  tone={Number(latestSummary.average_pnl || 0) >= 0 ? "good" : "bad"}
                />
                <Metric
                  label="Win rate"
                  value={`${number.format(Number(latestSummary.win_rate || 0))}%`}
                  tone="info"
                />
                <Metric
                  label="Gross profit"
                  value={currency.format(Number(latestSummary.gross_profit || 0))}
                  tone="good"
                />
                <Metric
                  label="Gross loss"
                  value={currency.format(Number(latestSummary.gross_loss || 0))}
                  tone="bad"
                />
                <Metric
                  label="Max drawdown"
                  value={currency.format(Number(latestSummary.max_drawdown || 0))}
                  tone="bad"
                />
                <Metric
                  label="Initial margin"
                  value={currency.format(Number(latestSummary.initial_margin || 0))}
                  tone="info"
                />
                <Metric
                  label="Margin / lot"
                  value={currency.format(
                    Number(
                      latestSummary.calculator_margin_per_lot ||
                        latestSummary.initial_margin_per_lot ||
                        0
                    )
                  )}
                  tone="info"
                />
                <Metric
                  label="Margin %"
                  value={`${number.format(Number(latestSummary.margin_requirement_percent || 0))}%`}
                />
                <Metric
                  label="Max margin used"
                  value={currency.format(Number(latestSummary.max_margin_used || 0))}
                />
              </div>
            ) : (
              <div className="mt-5 rounded-lg border border-dashed border-slate-700 px-4 py-8 text-center text-sm text-slate-400">
                Results will appear here after the first run.
              </div>
            )}
          </section>

          {recentTrades.length ? (
            <section className="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/70">
              <div className="border-b border-slate-800 px-5 py-4">
                <h2 className="text-lg font-semibold text-white">Latest trades</h2>
              </div>
              <div className="overflow-x-auto">
                <table className="w-full min-w-[760px] text-left text-sm">
                  <thead className="bg-slate-900/80 text-xs uppercase tracking-wide text-slate-500">
                    <tr>
                      <th className="px-4 py-3">Side</th>
                      <th className="px-4 py-3">Entry</th>
                      <th className="px-4 py-3">Exit</th>
                      <th className="px-4 py-3">Reason</th>
                      <th className="px-4 py-3 text-right">P&L</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-slate-800">
                    {recentTrades.map((trade) => (
                      <tr key={trade.id}>
                        <td className="px-4 py-3 font-semibold text-white">
                          {trade.direction}
                        </td>
                        <td className="px-4 py-3 text-slate-300">
                          {formatDateTime(trade.entry_time)} @{" "}
                          {number.format(Number(trade.entry_price))}
                        </td>
                        <td className="px-4 py-3 text-slate-300">
                          {formatDateTime(trade.exit_time)} @{" "}
                          {number.format(Number(trade.exit_price))}
                        </td>
                        <td className="px-4 py-3 text-slate-300">
                          {trade.exit_reason}
                        </td>
                        <td
                          className={`px-4 py-3 text-right font-semibold ${
                            Number(trade.realized_pnl) >= 0
                              ? "text-emerald-300"
                              : "text-rose-300"
                          }`}
                        >
                          {currency.format(Number(trade.realized_pnl || 0))}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </section>
          ) : null}
        </div>
      </section>

      <section className="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/70">
        <div className="border-b border-slate-800 px-5 py-4">
          <h2 className="text-lg font-semibold text-white">Recent runs</h2>
        </div>
        {history.length ? (
          <div className="divide-y divide-slate-800">
            {history.map((run) => (
              <div
                key={run.id}
                className="grid gap-3 px-5 py-4 text-sm md:grid-cols-[1.2fr_1fr_1fr_1fr]"
              >
                <div>
                  <p className="font-semibold text-white">
                    {run.instrument} {run.interval}
                  </p>
                  <p className="text-xs text-slate-500">
                    {formatDateTime(run.created_at)}
                  </p>
                </div>
                <p className="text-slate-300">
                  {run.lookback_months} months, {run.lots} lots
                </p>
                <p
                  className={`font-semibold ${
                    Number(run.summary?.net_pnl || 0) >= 0
                      ? "text-emerald-300"
                      : "text-rose-300"
                  }`}
                >
                  {currency.format(Number(run.summary?.net_pnl || 0))}
                </p>
                <p className="text-slate-400">
                  {currency.format(
                    Number(
                      run.summary?.calculator_margin_per_lot ||
                        run.summary?.initial_margin_per_lot ||
                        0
                    )
                  )}{" "}
                  / lot
                </p>
              </div>
            ))}
          </div>
        ) : (
          <div className="px-5 py-8 text-sm text-slate-400">
            No recent backtest runs.
          </div>
        )}
      </section>
    </div>
  );
}
