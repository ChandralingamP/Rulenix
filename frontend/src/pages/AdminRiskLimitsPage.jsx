import { useCallback, useEffect, useMemo, useState } from "react";
import apiClient from "../utils/axiosConfig.js";

const RISK_FIELDS = [
  ["max_lots", "Maximum lots"],
  ["max_quantity", "Maximum quantity"],
  ["max_notional", "Maximum notional"],
  ["max_open_positions", "Open positions"],
  ["max_trades_per_day", "Trades per day"],
  ["max_daily_realized_loss", "Daily realized loss"],
  ["max_daily_unrealized_loss", "Daily unrealized loss"],
  ["max_price_age_seconds", "Price age (seconds)"],
  ["margin_requirement_percent", "Margin required (%)"],
];

function LimitFields({ draft, onChange, prefix }) {
  return (
    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
      {RISK_FIELDS.map(([key, label]) => (
        <label key={key} className="text-xs font-semibold uppercase tracking-wide text-slate-500">
          {label}
          <input
            aria-label={`${prefix} ${label}`}
            type="number"
            min="0.01"
            step="any"
            value={draft[key] ?? ""}
            onChange={(event) => onChange(key, event.target.value)}
            className="mt-2 h-10 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm normal-case tracking-normal text-white"
          />
        </label>
      ))}
    </div>
  );
}

function KillSwitchButton({ enabled, busy, onClick, scope }) {
  return (
    <button
      type="button"
      disabled={busy}
      onClick={onClick}
      className={`rounded-lg px-4 py-2 text-sm font-semibold text-white transition disabled:opacity-50 ${
        enabled ? "bg-emerald-600 hover:bg-emerald-500" : "bg-rose-600 hover:bg-rose-500"
      }`}
    >
      {enabled ? `Clear ${scope} pause` : `Pause ${scope} entries`}
    </button>
  );
}

export default function AdminRiskLimitsPage() {
  const [data, setData] = useState(null);
  const [selectedId, setSelectedId] = useState("");
  const [globalDraft, setGlobalDraft] = useState({});
  const [userDraft, setUserDraft] = useState({});
  const [message, setMessage] = useState("");
  const [messageTone, setMessageTone] = useState("info");
  const [busy, setBusy] = useState(false);

  const applyData = useCallback((next) => {
    setData(next);
    setGlobalDraft(next?.global_limits || {});
    setSelectedId((current) =>
      next?.users?.some((user) => user.id === current)
        ? current
        : next?.users?.[0]?.id || ""
    );
  }, []);

  const load = useCallback(async () => {
    setMessage("");
    try {
      const response = await apiClient.get("/risk/admin");
      applyData(response.data);
    } catch (requestError) {
      setMessageTone("error");
      setMessage(requestError.response?.data?.detail || "Unable to load risk limits.");
    }
  }, [applyData]);

  useEffect(() => {
    load();
  }, [load]);

  const selected = useMemo(
    () => data?.users?.find((user) => user.id === selectedId),
    [data, selectedId]
  );

  useEffect(() => {
    if (!selected || !data) return;
    setUserDraft(
      Object.fromEntries(
        RISK_FIELDS.map(([key]) => [
          key,
          selected.limits?.[key] ?? data.global_limits?.[key] ?? "",
        ])
      )
    );
  }, [data, selected]);

  const payloadFor = (draft) =>
    Object.fromEntries(RISK_FIELDS.map(([key]) => [key, Number(draft[key])]));

  const save = async (url, draft, label) => {
    const payload = payloadFor(draft);
    if (Object.values(payload).some((value) => !Number.isFinite(value) || value <= 0)) {
      setMessageTone("error");
      setMessage("Every risk limit must be a positive number.");
      return;
    }
    setBusy(true);
    setMessage("");
    try {
      const response = await apiClient.put(url, payload);
      applyData(response.data);
      setMessageTone("success");
      setMessage(`${label} limits saved.`);
    } catch (requestError) {
      setMessageTone("error");
      setMessage(requestError.response?.data?.detail || "Unable to save risk limits.");
    } finally {
      setBusy(false);
    }
  };

  const setKill = async (url, enabled, label) => {
    setBusy(true);
    setMessage("");
    try {
      const response = await apiClient.put(url, {
        enabled,
        reason: enabled ? `Emergency pause for ${label}` : `Pause cleared for ${label}`,
      });
      applyData(response.data);
      setMessageTone("success");
      setMessage(enabled ? `${label} entries paused.` : `${label} entry pause cleared.`);
    } catch (requestError) {
      setMessageTone("error");
      setMessage(requestError.response?.data?.detail || "Unable to update the kill switch.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-6">
      <header className="flex flex-col justify-between gap-4 sm:flex-row sm:items-end">
        <div>
          <p className="text-xs uppercase tracking-[0.35em] text-brand-300">Administration</p>
          <h1 className="mt-2 text-3xl font-semibold text-white">Risk limits</h1>
          <p className="mt-2 text-sm text-slate-400">
            All global and per-user exposure limits are managed on this page.
          </p>
        </div>
        <button type="button" onClick={load} disabled={busy} className="self-start rounded-lg border border-slate-700 px-4 py-2 text-sm font-semibold text-slate-300 hover:border-brand-400 hover:text-brand-200 disabled:opacity-50">Refresh limits</button>
      </header>

      {message ? (
        <div className={`rounded-lg border px-4 py-3 text-sm ${messageTone === "error" ? "border-rose-500/40 bg-rose-500/10 text-rose-200" : messageTone === "success" ? "border-emerald-500/40 bg-emerald-500/10 text-emerald-200" : "border-slate-700 bg-slate-900 text-slate-200"}`}>
          {message}
        </div>
      ) : null}

      {!data ? (
        <section className="rounded-xl border border-slate-800 bg-slate-900/70 px-5 py-10 text-center text-sm text-slate-400">Loading risk limits...</section>
      ) : (
        <>
          <section className="rounded-xl border border-slate-800 bg-slate-900/70 p-5">
            <div className="flex flex-col justify-between gap-4 border-b border-slate-800 pb-5 sm:flex-row sm:items-start">
              <div>
                <div className="flex items-center gap-2">
                  <h2 className="text-lg font-semibold text-white">Global limits</h2>
                  <span className={`rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase ${data.global_kill_switch.enabled ? "bg-rose-500/15 text-rose-300" : "bg-emerald-500/15 text-emerald-300"}`}>
                    {data.global_kill_switch.enabled ? "Entries paused" : "Active"}
                  </span>
                </div>
                <p className="mt-1 text-sm text-slate-400">Defaults and hard ceilings applied across every trading account.</p>
              </div>
              <KillSwitchButton enabled={data.global_kill_switch.enabled} busy={busy} scope="all users" onClick={() => setKill("/risk/admin/kill-switch", !data.global_kill_switch.enabled, "All users")} />
            </div>
            <div className="mt-5 space-y-4">
              <LimitFields draft={globalDraft} prefix="Global" onChange={(key, value) => setGlobalDraft((current) => ({ ...current, [key]: value }))} />
              <button type="button" disabled={busy} onClick={() => save("/risk/admin/limits", globalDraft, "Global")} className="rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white hover:bg-brand-400 disabled:bg-slate-700">Save global limits</button>
            </div>
          </section>

          <section className="rounded-xl border border-slate-800 bg-slate-900/70 p-5">
            <div className="flex flex-col justify-between gap-4 border-b border-slate-800 pb-5 sm:flex-row sm:items-start">
              <div>
                <h2 className="text-lg font-semibold text-white">Per-user limits</h2>
                <p className="mt-1 text-sm text-slate-400">Choose an account to review or override its effective limits.</p>
              </div>
              {selected ? <KillSwitchButton enabled={selected.kill_switch.enabled} busy={busy} scope={selected.username} onClick={() => setKill(`/risk/admin/kill-switch/${selected.id}`, !selected.kill_switch.enabled, selected.username)} /> : null}
            </div>
            {data.users?.length ? (
              <div className="mt-5 space-y-4">
                <label className="block max-w-sm text-xs font-semibold uppercase tracking-wide text-slate-500">
                  Trading account
                  <select aria-label="Risk control user" value={selectedId} onChange={(event) => setSelectedId(event.target.value)} className="mt-2 h-10 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm normal-case tracking-normal text-white">
                    {data.users.map((user) => <option key={user.id} value={user.id}>{user.username}</option>)}
                  </select>
                </label>
                {selected ? (
                  <>
                    <div className="flex items-center gap-2 text-xs text-slate-500">
                      Status:
                      <span className={`rounded-full px-2 py-0.5 font-semibold uppercase ${selected.kill_switch.enabled ? "bg-rose-500/15 text-rose-300" : "bg-emerald-500/15 text-emerald-300"}`}>{selected.kill_switch.enabled ? "Entries paused" : "Active"}</span>
                    </div>
                    <LimitFields draft={userDraft} prefix={selected.username} onChange={(key, value) => setUserDraft((current) => ({ ...current, [key]: value }))} />
                    <button type="button" disabled={busy} onClick={() => save(`/risk/admin/limits/${selected.id}`, userDraft, selected.username)} className="rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white hover:bg-brand-400 disabled:bg-slate-700">Save user limits</button>
                  </>
                ) : null}
              </div>
            ) : (
              <p className="mt-5 text-sm text-slate-400">No trading users are available.</p>
            )}
          </section>

          <p className="text-xs leading-5 text-slate-500">Kill switches stop new entries and cancel pending entries. Existing target and stop-loss protection remains active.</p>
        </>
      )}
    </div>
  );
}
