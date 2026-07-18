import { Fragment, useEffect, useState } from "react";
import { useDispatch, useSelector } from "react-redux";
import {
  clearStrategyNotice,
  fetchStrategies,
  saveStrategyInstrument,
  setStrategyActivation,
} from "../features/strategies/strategiesSlice.js";

const formatDate = (value) => {
  if (!value) return "—";
  const date = new Date(`${value}T00:00:00`);
  return Number.isNaN(date.getTime())
    ? value
    : date.toLocaleDateString("en-IN", {
        day: "2-digit",
        month: "short",
        year: "numeric",
      });
};

const TRADING_PAUSED_MESSAGE =
  "Trading paused: Market data is temporarily unavailable. No trades will be placed until it recovers";

const Toggle = ({ active, label, disabled = false, onChange }) => (
  <button
    type="button"
    role="switch"
    aria-checked={active}
    aria-label={label}
    disabled={disabled}
    onClick={onChange}
    className={`inline-flex h-6 w-11 items-center rounded-full border border-transparent px-1 transition disabled:cursor-wait disabled:opacity-50 ${
      active ? "justify-end bg-emerald-500/50" : "justify-start bg-slate-700"
    }`}
  >
    <span className="h-4 w-4 rounded-full bg-white shadow" />
  </button>
);

export default function StrategiesPage() {
  const dispatch = useDispatch();
  const { items, status, activationKey, instrumentKey, error, notice } =
    useSelector((state) => state.strategies);
  const [drafts, setDrafts] = useState({});

  useEffect(() => {
    dispatch(fetchStrategies());
  }, [dispatch]);

  useEffect(() => {
    const next = {};
    items.forEach((strategy) => {
      strategy.instruments?.forEach((instrument) => {
        next[`${strategy.key}:${instrument.instrument}`] = {
          enabled: Boolean(instrument.enabled),
          lots: instrument.lots || 1,
          runDaySession: instrument.run_day_session ?? true,
          runEveningSession: instrument.run_evening_session ?? true,
          intervalKey: instrument.interval_key || "FIVE_MINUTE",
          stopLossPercent: instrument.stop_loss_percent ?? 5,
          targetPercent: instrument.target_percent ?? 20,
          keltnerMultiplier: instrument.keltner_multiplier ?? 2,
          requireVolume: Boolean(instrument.require_volume),
          premiumMin: instrument.premium_min ?? 200,
          premiumMax: instrument.premium_max ?? 300,
        };
      });
    });
    setDrafts(next);
  }, [items]);

  useEffect(() => {
    const hasActiveStrategy = items.some((strategy) => strategy.active);
    if (!hasActiveStrategy) return undefined;
    const intervalId = window.setInterval(
      () => dispatch(fetchStrategies({ silent: true })),
      15000
    );
    return () => window.clearInterval(intervalId);
  }, [dispatch, items]);

  useEffect(() => {
    if (!notice && !error) return undefined;
    const timeoutId = window.setTimeout(
      () => dispatch(clearStrategyNotice()),
      5000
    );
    return () => window.clearTimeout(timeoutId);
  }, [dispatch, error, notice]);

  const updateDraft = (key, changes) => {
    setDrafts((current) => ({
      ...current,
      [key]: { ...current[key], ...changes },
    }));
  };

  const saveInstrument = (strategy, instrument, changes = {}) => {
    const key = `${strategy.key}:${instrument.instrument}`;
    const draft = { ...drafts[key], ...changes };
    const lots = Number(draft?.lots);
    if (!Number.isInteger(lots) || lots <= 0) return;
    dispatch(
      saveStrategyInstrument({
        strategyKey: strategy.key,
        instrument: instrument.instrument,
        enabled: Boolean(draft.enabled),
        lots,
        runDaySession: Boolean(draft.runDaySession),
        runEveningSession: Boolean(draft.runEveningSession),
        intervalKey: draft.intervalKey,
        stopLossPercent: Number(draft.stopLossPercent),
        targetPercent: Number(draft.targetPercent),
        keltnerMultiplier: Number(draft.keltnerMultiplier),
        requireVolume: Boolean(draft.requireVolume),
        premiumMin: Number(draft.premiumMin),
        premiumMax: Number(draft.premiumMax),
      })
    );
  };

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-6">
      <header className="flex flex-col justify-between gap-4 sm:flex-row sm:items-end">
        <div>
          <p className="text-xs uppercase tracking-[0.4em] text-brand-300">
            Trading controls
          </p>
          <h1 className="mt-2 text-3xl font-semibold text-white">Strategies</h1>
          <p className="mt-2 text-sm text-slate-400">
            Activate a strategy, then choose the instruments it can trade.
          </p>
        </div>
        <button
          type="button"
          onClick={() => dispatch(fetchStrategies())}
          disabled={status === "loading" || status === "refreshing"}
          className="self-start rounded-full border border-slate-700 bg-slate-900/60 px-4 py-2 text-xs font-semibold text-slate-300 transition hover:border-brand-400 hover:text-brand-200 disabled:cursor-wait disabled:opacity-50"
        >
          {status === "refreshing" ? "Refreshing…" : "Refresh"}
        </button>
      </header>

      {notice || error ? (
        <div
          className={`rounded-xl border px-4 py-3 text-sm ${
            error
              ? "border-rose-500/30 bg-rose-500/10 text-rose-200"
              : "border-emerald-500/30 bg-emerald-500/10 text-emerald-200"
          }`}
        >
          {error || notice}
        </div>
      ) : null}

      {status === "loading" && !items.length ? (
        <div className="space-y-4" aria-label="Loading strategies">
          <div className="h-32 animate-pulse rounded-2xl bg-slate-900/70" />
        </div>
      ) : !items.length ? (
        <div className="rounded-2xl border border-slate-800 bg-slate-900/60 px-6 py-12 text-center text-sm text-slate-400">
          No strategies are available yet.
        </div>
      ) : (
        <section className="space-y-4" aria-label="Available strategies">
          {items.map((strategy) => (
            <article
              key={strategy.key}
              className="overflow-hidden rounded-2xl border border-slate-800 bg-slate-900/60 shadow-xl shadow-black/30"
            >
              <div className="flex flex-col justify-between gap-5 p-5 sm:flex-row sm:items-center sm:p-6">
                <div className="flex items-start gap-4">
                  <div className="flex h-11 w-11 shrink-0 items-center justify-center rounded-xl bg-brand-500/15 text-brand-300">
                    <svg
                      aria-hidden="true"
                      viewBox="0 0 24 24"
                      fill="none"
                      className="h-6 w-6"
                    >
                      <path
                        d="M4 17l5-5 3 3 7-8M15 7h4v4"
                        stroke="currentColor"
                        strokeWidth="1.8"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                      />
                    </svg>
                  </div>
                  <div>
                    <div className="flex flex-wrap items-center gap-3">
                      <h2 className="text-lg font-semibold text-white">
                        {strategy.name}
                      </h2>
                      <span
                        className={`rounded-full px-2.5 py-1 text-[11px] font-semibold ${
                          strategy.active
                            ? "bg-emerald-500/15 text-emerald-300"
                            : "bg-slate-800 text-slate-400"
                        }`}
                      >
                        {strategy.active ? "Active" : "Inactive"}
                      </span>
                    </div>
                    <p className="mt-2 max-w-2xl text-sm text-slate-400">
                      {strategy.description}
                    </p>
                  </div>
                </div>
                <div className="flex items-center gap-3 self-end sm:self-auto">
                  <span className="text-xs font-medium text-slate-400">
                    {strategy.active ? "Deactivate" : "Activate"}
                  </span>
                  <Toggle
                    active={strategy.active}
                    label={`${strategy.active ? "Deactivate" : "Activate"} ${
                      strategy.name
                    }`}
                    disabled={activationKey === strategy.key}
                    onChange={() =>
                      dispatch(
                        setStrategyActivation({
                          strategyKey: strategy.key,
                          active: !strategy.active,
                        })
                      )
                    }
                  />
                </div>
              </div>

              {strategy.active &&
              (strategy.operational_alerts?.length ||
                strategy.scheduler_runs?.length) ? (
                <div className="border-t border-slate-800 px-5 py-4 sm:px-6">
                  {strategy.operational_alerts?.length ? (
                    <div className="mb-3" aria-label="Operational alert">
                      {strategy.operational_alerts.slice(0, 1).map((alert) => (
                        <div
                          key={alert.id}
                          className={`rounded-lg border px-3 py-2 text-xs ${
                            alert.severity === "error"
                              ? "border-rose-500/30 bg-rose-500/10 text-rose-200"
                              : "border-amber-500/30 bg-amber-500/10 text-amber-200"
                          }`}
                        >
                          {alert.code === "risk_rejected" && alert.message
                            ? alert.message
                            : TRADING_PAUSED_MESSAGE}
                        </div>
                      ))}
                    </div>
                  ) : null}
                  {strategy.scheduler_runs?.length ? (
                    <div className="flex flex-wrap gap-2" aria-label="Today's scheduler runs">
                      {strategy.scheduler_runs.map((run) => (
                        <span
                          key={`${run.instrument}:${run.session}:${run.action}`}
                          title={run.last_error || ""}
                          className={`rounded-full px-2.5 py-1 text-[11px] font-semibold ${
                            run.status === "completed"
                              ? "bg-emerald-500/15 text-emerald-300"
                              : run.status === "failed"
                                ? "bg-rose-500/15 text-rose-300"
                                : "bg-slate-800 text-slate-400"
                          }`}
                        >
                          {run.session} {run.action}: {run.status}
                        </span>
                      ))}
                    </div>
                  ) : null}
                </div>
              ) : null}

              {strategy.active ? (
                <div className="border-t border-slate-800 bg-slate-950/35 p-5 sm:p-6">
                  <div className="mb-4">
                    <h3 className="text-sm font-semibold text-white">
                      Available instruments
                    </h3>
                    <p className="mt-1 text-xs text-slate-500">
                      Enable the instruments this strategy may use and configure
                      their trade size.
                    </p>
                  </div>

                  <div className="overflow-x-auto rounded-xl border border-slate-800">
                    <table className="w-full min-w-[820px] text-left text-sm">
                      <thead className="bg-slate-900/90 text-[11px] uppercase tracking-wider text-slate-500">
                        <tr>
                          <th className="px-4 py-3 font-semibold">Instrument</th>
                          <th className="px-4 py-3 font-semibold">Selected symbol</th>
                          <th className="px-4 py-3 font-semibold">Expiry</th>
                          <th className="px-4 py-3 font-semibold">Contract lot</th>
                          <th className="px-4 py-3 font-semibold">Trade lots</th>
                          <th className="px-4 py-3 text-center font-semibold">Use</th>
                          <th className="px-4 py-3" />
                        </tr>
                      </thead>
                      <tbody className="divide-y divide-slate-800 bg-slate-950/60">
                        {strategy.instruments?.map((instrument) => {
                          const key = `${strategy.key}:${instrument.instrument}`;
                          const draft = drafts[key] || {
                            enabled: instrument.enabled,
                            lots: instrument.lots,
                            runDaySession: instrument.run_day_session ?? true,
                            runEveningSession: instrument.run_evening_session ?? true,
                            intervalKey: instrument.interval_key || "FIVE_MINUTE",
                            stopLossPercent: instrument.stop_loss_percent ?? 5,
                            targetPercent: instrument.target_percent ?? 20,
                            keltnerMultiplier: instrument.keltner_multiplier ?? 2,
                            requireVolume: Boolean(instrument.require_volume),
                            premiumMin: instrument.premium_min ?? 200,
                            premiumMax: instrument.premium_max ?? 300,
                          };
                          const lots = Number(draft.lots);
                          const lotsValid = Number.isInteger(lots) && lots > 0;
                          const isIchimoku = strategy.key === "ichimoku_keltner_tsi";
                          const parametersValid =
                            !isIchimoku ||
                            (Number(draft.stopLossPercent) > 0 &&
                              Number(draft.targetPercent) > 0 &&
                              Number(draft.keltnerMultiplier) >= 0.1 &&
                              (instrument.instrument === "GOLDTEN" ||
                                (Number(draft.premiumMin) > 0 &&
                                  Number(draft.premiumMax) > Number(draft.premiumMin))));
                          const snapshot = instrument.snapshot;
                          return (
                            <Fragment key={instrument.instrument}>
                            <tr>
                              <td className="px-4 py-4">
                                <div className="flex items-center gap-3">
                                  <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-amber-500/15 font-bold text-amber-300">
                                    {instrument.instrument === "GOLDTEN" ? "Au" : "IDX"}
                                  </div>
                                  <div>
                                    <p className="font-semibold text-white">
                                      {instrument.instrument}
                                    </p>
                                    <p className="text-xs text-slate-500">
                                      {instrument.label}
                                    </p>
                                  </div>
                                </div>
                              </td>
                              <td className="px-4 py-4 font-medium text-slate-200">
                                {snapshot?.contract_symbol ||
                                  (draft.enabled ? "Selecting…" : "—")}
                              </td>
                              <td className="px-4 py-4 text-slate-300">
                                {formatDate(snapshot?.contract_expiry)}
                              </td>
                              <td className="px-4 py-4 text-slate-300">
                                {snapshot?.lot_size ?? "—"}
                              </td>
                              <td className="px-4 py-4">
                                <input
                                  aria-label={`${instrument.instrument} trade lots`}
                                  type="number"
                                  min="1"
                                  step="1"
                                  value={draft.lots}
                                  onChange={(event) =>
                                    updateDraft(key, { lots: event.target.value })
                                  }
                                  className={`h-9 w-24 rounded-lg border bg-slate-900 px-3 text-white outline-none focus:ring ${
                                    lotsValid
                                      ? "border-slate-700 focus:border-brand-400 focus:ring-brand-500/20"
                                      : "border-rose-500 focus:ring-rose-500/20"
                                  }`}
                                />
                              </td>
                              <td className="px-4 py-4 text-center">
                                <Toggle
                                  active={Boolean(draft.enabled)}
                                  label={`Use ${instrument.instrument} in ${strategy.name}`}
                                  disabled={instrumentKey === key}
                                  onChange={() => {
                                    const enabled = !draft.enabled;
                                    updateDraft(key, {
                                      enabled,
                                    });
                                    if (isIchimoku) {
                                      saveInstrument(strategy, instrument, {
                                        ...draft,
                                        enabled,
                                      });
                                    }
                                  }}
                                />
                              </td>
                              <td className="px-4 py-4 text-right">
                                <button
                                  type="button"
                                  disabled={
                                    !lotsValid ||
                                    !parametersValid ||
                                    instrumentKey === key
                                  }
                                  onClick={() =>
                                    saveInstrument(strategy, instrument)
                                  }
                                  className="rounded-lg bg-brand-500 px-4 py-2 text-xs font-semibold text-white transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:bg-slate-700"
                                >
                                  {instrumentKey === key
                                    ? "Saving…"
                                    : "Save"}
                                </button>
                              </td>
                            </tr>
                            {isIchimoku ? (
                              <tr className="bg-slate-900/45">
                                <td colSpan={7} className="px-4 py-4">
                                  <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4 xl:grid-cols-7">
                                    <label className="text-[10px] font-semibold uppercase tracking-wide text-slate-500">
                                      Candle interval
                                      <select aria-label={`${instrument.instrument} candle interval`} value={draft.intervalKey} onChange={(event) => updateDraft(key, { intervalKey: event.target.value })} className="mt-1 h-9 w-full rounded-lg border border-slate-700 bg-slate-950 px-2 text-xs normal-case text-white">
                                        <option value="ONE_MINUTE">1 minute</option>
                                        <option value="FIVE_MINUTE">5 minutes</option>
                                        <option value="FIFTEEN_MINUTE">15 minutes</option>
                                      </select>
                                    </label>
                                    {[
                                      ["stopLossPercent", "Stop loss %", 0.01],
                                      ["targetPercent", "Target %", 0.01],
                                      ["keltnerMultiplier", "Keltner ATR ×", 0.1],
                                    ].map(([field, label, min]) => (
                                      <label key={field} className="text-[10px] font-semibold uppercase tracking-wide text-slate-500">
                                        {label}
                                        <input aria-label={`${instrument.instrument} ${label}`} type="number" min={min} step="0.1" value={draft[field]} onChange={(event) => updateDraft(key, { [field]: event.target.value })} className="mt-1 h-9 w-full rounded-lg border border-slate-700 bg-slate-950 px-2 text-xs normal-case text-white" />
                                      </label>
                                    ))}
                                    {instrument.instrument !== "GOLDTEN" ? (
                                      <>
                                        <label className="text-[10px] font-semibold uppercase tracking-wide text-slate-500">
                                          Premium min ₹
                                          <input aria-label={`${instrument.instrument} premium minimum`} type="number" min="1" step="1" value={draft.premiumMin} onChange={(event) => updateDraft(key, { premiumMin: event.target.value })} className="mt-1 h-9 w-full rounded-lg border border-slate-700 bg-slate-950 px-2 text-xs normal-case text-white" />
                                        </label>
                                        <label className="text-[10px] font-semibold uppercase tracking-wide text-slate-500">
                                          Premium max ₹
                                          <input aria-label={`${instrument.instrument} premium maximum`} type="number" min="1" step="1" value={draft.premiumMax} onChange={(event) => updateDraft(key, { premiumMax: event.target.value })} className="mt-1 h-9 w-full rounded-lg border border-slate-700 bg-slate-950 px-2 text-xs normal-case text-white" />
                                        </label>
                                      </>
                                    ) : null}
                                    <div className="flex flex-col justify-end gap-2 text-[10px] font-semibold uppercase tracking-wide text-slate-500">
                                      <label className="flex items-center gap-2"><input type="checkbox" checked={Boolean(draft.runDaySession)} onChange={(event) => updateDraft(key, { runDaySession: event.target.checked })} /> Day session</label>
                                      {instrument.instrument === "GOLDTEN" ? <label className="flex items-center gap-2"><input type="checkbox" checked={Boolean(draft.runEveningSession)} onChange={(event) => updateDraft(key, { runEveningSession: event.target.checked })} /> Evening session</label> : null}
                                      <label className="flex items-center gap-2"><input type="checkbox" checked={Boolean(draft.requireVolume)} onChange={(event) => updateDraft(key, { requireVolume: event.target.checked })} /> Volume filter</label>
                                    </div>
                                  </div>
                                  <p className="mt-3 text-xs text-slate-500">
                                    {instrument.instrument === "GOLDTEN" ? "Signals execute on the selected GOLDTEN future." : "Bullish signals buy CE and bearish signals buy PE; the nearest expiry contract closest to the midpoint of the premium range is selected."}{" "}
                                    Entry: MARKET. Target: LIMIT. Stop loss: STOPLOSS_LIMIT.
                                  </p>
                                </td>
                              </tr>
                            ) : null}
                            </Fragment>
                          );
                        })}
                      </tbody>
                    </table>
                  </div>
                </div>
              ) : null}
            </article>
          ))}
        </section>
      )}
    </div>
  );
}
