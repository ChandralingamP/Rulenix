import { useCallback, useEffect, useState } from "react";
import axios from "axios";
import { useNavigate, useOutletContext } from "react-router-dom";
import { API_BASE_URL } from "../utils/constants.js";

// ── Scheduled Jobs Panel ────────────────────────────────────

function ScheduledJobsPanel({ adminUsername }) {
  const [jobs, setJobs] = useState([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState("");
  const [triggeringJob, setTriggeringJob] = useState("");
  const [triggerResult, setTriggerResult] = useState(null);

  const loadJobs = useCallback(() => {
    if (!adminUsername) return;
    setIsLoading(true);
    setError("");
    axios
      .get(`${API_BASE_URL}/scheduler/jobs/`, {
        withCredentials: true,
      })
      .then((res) => setJobs(Array.isArray(res.data) ? res.data : []))
      .catch(() => setError("Unable to load scheduled jobs."))
      .finally(() => setIsLoading(false));
  }, [adminUsername]);

  useEffect(() => {
    loadJobs();
  }, [loadJobs]);

  // Auto-refresh while a job is running
  useEffect(() => {
    const hasRunning = jobs.some(
      (j) => j.last_run && j.last_run.status === "running"
    );
    if (!hasRunning) return undefined;
    const id = setInterval(loadJobs, 3000);
    return () => clearInterval(id);
  }, [jobs, loadJobs]);

  const triggerJob = useCallback(
    (jobKey) => {
      if (!adminUsername) return;
      setTriggeringJob(jobKey);
      setTriggerResult(null);
      axios
        .post(
          `${API_BASE_URL}/scheduler/trigger/`,
          { job_key: jobKey },
          { withCredentials: true }
        )
        .then((res) => {
          setTriggerResult({
            type: "success",
            message: res.data?.detail || "Job triggered.",
          });
          // Refresh after short delay so status shows "running"
          setTimeout(loadJobs, 500);
        })
        .catch((err) => {
          setTriggerResult({
            type: "error",
            message:
              err.response?.data?.detail || "Failed to trigger job.",
          });
        })
        .finally(() => setTriggeringJob(""));
    },
    [adminUsername, loadJobs]
  );

  const formatTime = (iso) => {
    if (!iso) return "—";
    try {
      return new Date(iso).toLocaleString("en-IN", {
        timeZone: "Asia/Kolkata",
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        day: "2-digit",
        month: "short",
      });
    } catch {
      return iso;
    }
  };

  const statusBadge = (run) => {
    if (!run) return null;
    const classes = {
      running: "bg-amber-500/20 text-amber-300",
      completed: "bg-emerald-500/20 text-emerald-300",
      failed: "bg-rose-500/20 text-rose-300",
    };
    return (
      <span
        className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-semibold ${
          classes[run.status] || "bg-slate-700 text-slate-300"
        }`}
      >
        {run.status === "running" && (
          <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-amber-400" />
        )}
        {run.status}
      </span>
    );
  };

  return (
    <section className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-white">Scheduled Jobs</h2>
          <p className="text-sm text-slate-400">
            Trigger any scheduled job on-demand or view its status.
          </p>
        </div>
        <button
          type="button"
          onClick={loadJobs}
          disabled={isLoading}
          className="rounded-lg border border-slate-700 px-3 py-1.5 text-xs font-medium text-slate-300 transition hover:bg-slate-800 disabled:opacity-50"
        >
          {isLoading ? "Refreshing..." : "Refresh"}
        </button>
      </div>

      {triggerResult && (
        <div
          className={`rounded-lg border px-4 py-3 text-sm ${
            triggerResult.type === "success"
              ? "border-emerald-500/40 bg-emerald-500/10 text-emerald-200"
              : "border-rose-500/40 bg-rose-500/10 text-rose-200"
          }`}
        >
          {triggerResult.message}
        </div>
      )}

      {error && (
        <div className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3 text-sm text-rose-200">
          {error}
        </div>
      )}

      {isLoading && jobs.length === 0 ? (
        <div className="rounded-lg border border-slate-800 bg-slate-900/70 px-4 py-6 text-sm text-slate-300">
          Loading jobs...
        </div>
      ) : (
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {jobs.map((job) => {
            const isRunning =
              job.last_run?.status === "running" ||
              triggeringJob === job.key;
            return (
              <div
                key={job.key}
                className="flex flex-col justify-between rounded-xl border border-slate-800 bg-slate-950/70 p-4"
              >
                <div>
                  <div className="flex items-start justify-between gap-2">
                    <h3 className="text-sm font-semibold text-white">
                      {job.label}
                    </h3>
                    {job.last_run && statusBadge(job.last_run)}
                  </div>
                  <p className="mt-1 text-xs text-slate-400">
                    {job.description}
                  </p>
                  <div className="mt-3 space-y-1 text-xs text-slate-500">
                    <p>
                      <span className="text-slate-400">Schedule:</span>{" "}
                      {job.schedule}
                    </p>
                    {job.next_run && (
                      <p>
                        <span className="text-slate-400">Next run:</span>{" "}
                        {formatTime(job.next_run)}
                      </p>
                    )}
                    {job.last_run?.started_at && (
                      <p>
                        <span className="text-slate-400">Last triggered:</span>{" "}
                        {formatTime(job.last_run.started_at)}
                      </p>
                    )}
                    {job.last_run?.error && (
                      <p className="text-rose-400">
                        Error: {job.last_run.error}
                      </p>
                    )}
                  </div>
                </div>
                <button
                  type="button"
                  disabled={isRunning}
                  onClick={() => triggerJob(job.key)}
                  className="mt-4 w-full rounded-lg bg-brand-500 px-3 py-2 text-xs font-semibold text-white shadow-lg shadow-brand-500/30 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:bg-slate-700"
                >
                  {isRunning ? (
                    <span className="flex items-center justify-center gap-1.5">
                      <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-white" />
                      Running...
                    </span>
                  ) : (
                    "Run Now"
                  )}
                </button>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

// ── Admin Page ──────────────────────────────────────────────

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

function RiskControlsPanel() {
  const [data, setData] = useState(null);
  const [selectedId, setSelectedId] = useState("");
  const [globalDraft, setGlobalDraft] = useState({});
  const [userDraft, setUserDraft] = useState({});
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const applyData = useCallback((next) => {
    setData(next);
    setGlobalDraft(next?.global_limits || {});
    setSelectedId((current) => current || next?.users?.[0]?.id || "");
  }, []);
  const load = useCallback(() => {
    axios.get(`${API_BASE_URL}/risk/admin`, { withCredentials: true })
      .then((response) => applyData(response.data))
      .catch(() => setMessage("Unable to load risk controls."));
  }, [applyData]);
  useEffect(load, [load]);
  const selected = data?.users?.find((user) => user.id === selectedId);
  useEffect(() => {
    if (!selected || !data) return;
    setUserDraft(Object.fromEntries(RISK_FIELDS.map(([key]) => [key, selected.limits?.[key] ?? data.global_limits?.[key] ?? ""])));
  }, [data, selected]);
  const save = (url, draft) => {
    const payload = Object.fromEntries(RISK_FIELDS.map(([key]) => [key, Number(draft[key])]));
    setBusy(true); setMessage("");
    axios.put(`${API_BASE_URL}${url}`, payload, { withCredentials: true })
      .then((response) => { applyData(response.data); setMessage("Risk limits saved."); })
      .catch((error) => setMessage(error.response?.data?.detail || "Unable to save risk limits."))
      .finally(() => setBusy(false));
  };
  const setKill = (url, enabled) => {
    setBusy(true);
    axios.put(`${API_BASE_URL}${url}`, { enabled, reason: enabled ? "Emergency pause from staff console" : "Cleared from staff console" }, { withCredentials: true })
      .then((response) => { applyData(response.data); setMessage(enabled ? "Kill switch engaged; pending entries are being cancelled." : "Kill switch cleared."); })
      .catch((error) => setMessage(error.response?.data?.detail || "Unable to update kill switch."))
      .finally(() => setBusy(false));
  };
  const fields = (draft, setDraft) => <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">{RISK_FIELDS.map(([key, label]) => <label key={key} className="text-xs text-slate-400">{label}<input aria-label={label} type="number" min="0.01" step="any" value={draft[key] ?? ""} onChange={(event) => setDraft((current) => ({ ...current, [key]: event.target.value }))} className="mt-1 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-white" /></label>)}</div>;
  if (!data) return <section className="rounded-xl border border-slate-800 bg-slate-900/70 p-5 text-sm text-slate-400">{message || "Loading risk controls..."}</section>;
  return <section className="space-y-5 rounded-xl border border-slate-800 bg-slate-900/70 p-5">
    <div className="flex flex-wrap items-center justify-between gap-3"><div><h2 className="text-lg font-semibold text-white">Trading Risk Engine</h2><p className="text-sm text-slate-400">Atomic limits cover demo and live entries. Protective exits remain active during a kill switch.</p></div><button type="button" disabled={busy} onClick={() => setKill("/risk/admin/kill-switch", !data.global_kill_switch.enabled)} className={`rounded-lg px-4 py-2 text-sm font-semibold text-white ${data.global_kill_switch.enabled ? "bg-emerald-600" : "bg-rose-600"}`}>{data.global_kill_switch.enabled ? "Clear global kill switch" : "Engage global kill switch"}</button></div>
    {message ? <p className="rounded-lg bg-slate-950 px-3 py-2 text-sm text-slate-200">{message}</p> : null}
    <div className="space-y-3"><h3 className="font-semibold text-white">Global limits</h3>{fields(globalDraft, setGlobalDraft)}<button type="button" disabled={busy} onClick={() => save("/risk/admin/limits", globalDraft)} className="rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white disabled:opacity-50">Save global limits</button></div>
    {selected ? <div className="space-y-3 border-t border-slate-800 pt-5"><div className="flex flex-wrap items-center gap-3"><h3 className="font-semibold text-white">Per-user controls</h3><select aria-label="Risk control user" value={selectedId} onChange={(event) => setSelectedId(event.target.value)} className="rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-white">{data.users.map((user) => <option key={user.id} value={user.id}>{user.username}</option>)}</select><button type="button" disabled={busy} onClick={() => setKill(`/risk/admin/kill-switch/${selected.id}`, !selected.kill_switch.enabled)} className="rounded-lg border border-rose-500/50 px-3 py-2 text-xs font-semibold text-rose-200">{selected.kill_switch.enabled ? "Clear user kill switch" : "Pause this user"}</button></div>{fields(userDraft, setUserDraft)}<button type="button" disabled={busy} onClick={() => save(`/risk/admin/limits/${selected.id}`, userDraft)} className="rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white disabled:opacity-50">Save user limits</button></div> : null}
  </section>;
}

export default function AdminPage() {
  const { session } = useOutletContext();
  const [users, setUsers] = useState([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState("");
  const [savingUser, setSavingUser] = useState("");
  const [deletingUser, setDeletingUser] = useState("");
  const [pendingDelete, setPendingDelete] = useState(null);
  const adminUsername = session?.username || "";
  const navigate = useNavigate();

  const loadUsers = useCallback(() => {
    if (!adminUsername) {
      navigate("/login", { replace: true });
      return;
    }

    setIsLoading(true);
    setError("");

    axios
      .get(`${API_BASE_URL}/auth/admin/users/`, {
        withCredentials: true,
      })
      .then((response) => {
        setUsers(Array.isArray(response.data) ? response.data : []);
      })
      .catch(() => {
        setError("Unable to fetch users. Please try again.");
      })
      .finally(() => {
        setIsLoading(false);
      });
  }, [adminUsername, navigate]);

  useEffect(() => {
    if (!session?.ready) return;
    if (!session.permissions?.administer_users) {
      navigate("/", { replace: true });
      return;
    }
    loadUsers();
  }, [loadUsers, navigate, session]);

  const updatePermission = useCallback(
    (username, permission, nextValue) => {
      if (!adminUsername) {
        navigate("/login", { replace: true });
        return;
      }

      setSavingUser(username);
      setError("");
      axios
        .patch(
          `${API_BASE_URL}/auth/admin/users/`,
          {
            username,
            [permission]: nextValue,
          },
          { withCredentials: true }
        )
        .then((response) => {
          const updated = response.data?.user;
          if (!updated) {
            throw new Error("Missing user in response");
          }
          setUsers((previous) =>
            previous.map((user) =>
              user.username.toLowerCase() === updated.username.toLowerCase()
                ? updated
                : user
            )
          );
        })
        .catch(() => {
          setError("Unable to update user status. Please try again.");
          loadUsers();
        })
        .finally(() => {
          setSavingUser("");
        });
    },
    [adminUsername, loadUsers, navigate]
  );

  const requestDeleteUser = useCallback((username) => {
    setPendingDelete({ username });
  }, []);

  const resetPendingDelete = useCallback(() => {
    setPendingDelete(null);
  }, []);

  const confirmPendingDelete = useCallback(() => {
    if (!pendingDelete) {
      return;
    }

    if (!adminUsername) {
      navigate("/login", { replace: true });
      return;
    }

    const username = pendingDelete.username;
    setDeletingUser(username);
    setError("");
    axios
      .delete(`${API_BASE_URL}/auth/admin/users/`, {
        data: {
          username,
        },
        withCredentials: true,
      })
      .then(() => {
        setUsers((previous) =>
          previous.filter(
            (user) => user.username.toLowerCase() !== username.toLowerCase()
          )
        );
        resetPendingDelete();
      })
      .catch(() => {
        setError("Unable to delete user. Please try again.");
        loadUsers();
      })
      .finally(() => {
        setDeletingUser("");
      });
  }, [adminUsername, loadUsers, navigate, pendingDelete, resetPendingDelete]);

  return (
    <div className="space-y-6">
      <header className="space-y-3">
        <div className="space-y-1">
          <h1 className="text-2xl font-semibold text-white">Admin Console</h1>
          <p className="text-sm text-slate-400">
            Manage administration and live-trading permissions independently.
          </p>
        </div>
      </header>

      {error ? (
        <div className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3 text-sm text-rose-200">
          {error}
        </div>
      ) : null}

      <ScheduledJobsPanel adminUsername={adminUsername} />

      <RiskControlsPanel />

      <div>
        <h2 className="mb-3 text-lg font-semibold text-white">User Management</h2>
        {isLoading ? (
        <div className="rounded-lg border border-slate-800 bg-slate-900/70 px-4 py-6 text-sm text-slate-300">
          Loading users...
        </div>
      ) : (
        <div className="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/70 shadow-lg shadow-black/30">
          <table className="min-w-full divide-y divide-slate-800 text-left text-sm text-slate-200">
            <thead className="bg-slate-900/80 text-xs uppercase tracking-wide text-slate-400">
              <tr>
                <th scope="col" className="px-4 py-3">
                  Username
                </th>
                <th scope="col" className="px-4 py-3">
                  Email
                </th>
                <th scope="col" className="px-4 py-3 text-center">
                  Administration
                </th>
                <th scope="col" className="px-4 py-3 text-center">
                  Live trading
                </th>
                <th scope="col" className="px-4 py-3 text-center">
                  Current mode
                </th>
                <th scope="col" className="px-4 py-3 text-right">
                  Actions
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-slate-800">
              {users.length === 0 ? (
                <tr>
                  <td
                    className="px-4 py-6 text-center text-slate-400"
                    colSpan={6}
                  >
                    No users found.
                  </td>
                </tr>
              ) : (
                users.map((user) => {
                  const isUpdating = savingUser === user.username;
                  const isDeleting = deletingUser === user.username;
                  const isSelf =
                    (adminUsername || "").toLowerCase() ===
                    user.username.toLowerCase();
                  return (
                    <tr key={user.id ?? user.username}>
                      <td className="px-4 py-3 font-medium text-white">
                        {user.username}
                      </td>
                      <td className="px-4 py-3 text-slate-300">
                        {user.email || "—"}
                      </td>
                      <td className="px-4 py-3 text-center text-slate-300">
                        {user.can_administer ? "Allowed" : "Denied"}
                      </td>
                      <td className="px-4 py-3 text-center">
                        <span
                          className={`rounded-full px-3 py-1 text-xs font-semibold ${
                            user.can_live_trade
                              ? "bg-emerald-500/10 text-emerald-300"
                              : "bg-slate-800 text-slate-300"
                          }`}
                        >
                          {user.can_live_trade ? "Allowed" : "Denied"}
                        </span>
                      </td>
                      <td className="px-4 py-3 text-center uppercase text-slate-300">{user.trading_mode}</td>
                      <td className="px-4 py-3 text-right">
                        <div className="flex items-center justify-end gap-2">
                          <button
                            type="button"
                            disabled={isUpdating || isDeleting || isSelf}
                            onClick={() =>
                              updatePermission(user.username, "can_administer", !user.can_administer)
                            }
                            className="rounded-lg bg-brand-500 px-3 py-2 text-xs font-semibold text-white shadow-brand-500/30 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:bg-slate-700"
                          >
                            {isUpdating
                              ? "Updating..."
                              : user.can_administer ? "Revoke admin" : "Grant admin"}
                          </button>
                          <button
                            type="button"
                            disabled={isUpdating || isDeleting}
                            onClick={() => updatePermission(user.username, "can_live_trade", !user.can_live_trade)}
                            className="rounded-lg border border-amber-500/50 px-3 py-2 text-xs font-semibold text-amber-200 disabled:cursor-not-allowed disabled:border-slate-700 disabled:text-slate-500"
                          >
                            {user.can_live_trade ? "Revoke live" : "Grant live"}
                          </button>
                          <button
                            type="button"
                            disabled={isDeleting || isSelf}
                            onClick={() => requestDeleteUser(user.username)}
                            className="rounded-lg bg-rose-500/80 px-3 py-2 text-xs font-semibold text-white shadow-rose-500/20 transition hover:bg-rose-400 disabled:cursor-not-allowed disabled:bg-slate-700"
                          >
                            {isDeleting ? "Deleting..." : "Delete user"}
                          </button>
                        </div>
                      </td>
                    </tr>
                  );
                })
              )}
            </tbody>
          </table>
        </div>
      )}
      </div>

      {pendingDelete ? (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-950/80 backdrop-blur-sm">
          <div className="w-full max-w-md rounded-2xl border border-slate-700 bg-slate-900/95 px-6 py-7 shadow-2xl shadow-black/50">
            <div className="flex items-start gap-4">
              <div className="flex h-12 w-12 items-center justify-center rounded-full bg-rose-500/10 text-lg font-bold text-rose-300">
                !
              </div>
              <div className="space-y-2">
                <h2 className="text-lg font-semibold text-white">
                  Rulenix Admin
                </h2>
                <p className="text-sm leading-relaxed text-slate-300">
                  Permanently delete{" "}
                  <span className="font-semibold text-white">
                    {pendingDelete.username}
                  </span>
                  ? This action removes the account and all linked Rulenix
                  data. You cannot undo this.
                </p>
              </div>
            </div>
            <div className="mt-6 flex justify-end gap-3">
              <button
                type="button"
                disabled={deletingUser === pendingDelete.username}
                onClick={resetPendingDelete}
                className="rounded-lg border border-slate-700 bg-slate-800 px-4 py-2 text-xs font-semibold uppercase tracking-wide text-slate-200 transition hover:bg-slate-700 disabled:cursor-not-allowed disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={confirmPendingDelete}
                disabled={deletingUser === pendingDelete.username}
                className="rounded-lg bg-rose-500 px-4 py-2 text-xs font-semibold uppercase tracking-wide text-white shadow-rose-500/30 transition hover:bg-rose-400 disabled:cursor-not-allowed disabled:bg-slate-700"
              >
                {deletingUser === pendingDelete.username
                  ? "Deleting..."
                  : "Delete user"}
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
