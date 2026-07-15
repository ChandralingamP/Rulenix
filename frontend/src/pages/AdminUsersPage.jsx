import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate, useOutletContext } from "react-router-dom";
import apiClient from "../utils/axiosConfig.js";

function PermissionBadge({ allowed, tone = "emerald" }) {
  const enabled = {
    emerald: "bg-emerald-500/10 text-emerald-300",
    sky: "bg-sky-500/10 text-sky-300",
    violet: "bg-violet-500/10 text-violet-300",
  };
  return (
    <span
      className={`rounded-full px-3 py-1 text-xs font-semibold ${
        allowed ? enabled[tone] : "bg-slate-800 text-slate-400"
      }`}
    >
      {allowed ? "Allowed" : "Denied"}
    </span>
  );
}

export default function AdminUsersPage() {
  const { session, refreshSession } = useOutletContext();
  const navigate = useNavigate();
  const [users, setUsers] = useState([]);
  const [query, setQuery] = useState("");
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState("");
  const [savingUser, setSavingUser] = useState("");
  const [deletingUser, setDeletingUser] = useState("");
  const [pendingDelete, setPendingDelete] = useState(null);
  const adminUsername = session?.username || "";

  const loadUsers = useCallback(async () => {
    if (!adminUsername) return;
    setIsLoading(true);
    setError("");
    try {
      const response = await apiClient.get("/auth/admin/users/");
      setUsers(Array.isArray(response.data) ? response.data : []);
    } catch (requestError) {
      if (requestError.response?.status === 401) {
        navigate("/login", { replace: true });
      } else {
        setError(requestError.response?.data?.detail || "Unable to fetch users.");
      }
    } finally {
      setIsLoading(false);
    }
  }, [adminUsername, navigate]);

  useEffect(() => {
    loadUsers();
  }, [loadUsers]);

  const filteredUsers = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    if (!normalized) return users;
    return users.filter(
      (user) =>
        user.username.toLowerCase().includes(normalized) ||
        (user.email || "").toLowerCase().includes(normalized)
    );
  }, [query, users]);

  const stats = useMemo(
    () => ({
      total: users.length,
      administrators: users.filter((user) => user.can_administer).length,
      live: users.filter((user) => user.can_live_trade).length,
      backtesting: users.filter((user) => user.can_backtest).length,
    }),
    [users]
  );

  const updatePermission = useCallback(
    async (username, permission, nextValue) => {
      setSavingUser(username);
      setError("");
      try {
        const response = await apiClient.patch("/auth/admin/users/", {
          username,
          [permission]: nextValue,
        });
        const updated = response.data?.user;
        if (!updated) throw new Error("Missing user in response");
        setUsers((current) =>
          current.map((user) =>
            user.username.toLowerCase() === updated.username.toLowerCase()
              ? updated
              : user
          )
        );
        if (updated.username.toLowerCase() === adminUsername.toLowerCase()) {
          refreshSession?.();
        }
      } catch (requestError) {
        setError(requestError.response?.data?.detail || "Unable to update user permissions.");
        await loadUsers();
      } finally {
        setSavingUser("");
      }
    },
    [adminUsername, loadUsers, refreshSession]
  );

  const confirmDelete = useCallback(async () => {
    if (!pendingDelete) return;
    const username = pendingDelete.username;
    setDeletingUser(username);
    setError("");
    try {
      await apiClient.delete("/auth/admin/users/", { data: { username } });
      setUsers((current) =>
        current.filter((user) => user.username.toLowerCase() !== username.toLowerCase())
      );
      setPendingDelete(null);
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to delete user.");
      await loadUsers();
    } finally {
      setDeletingUser("");
    }
  }, [loadUsers, pendingDelete]);

  return (
    <div className="space-y-6">
      <header className="flex flex-col justify-between gap-4 sm:flex-row sm:items-end">
        <div>
          <p className="text-xs uppercase tracking-[0.35em] text-brand-300">Administration</p>
          <h1 className="mt-2 text-3xl font-semibold text-white">User control</h1>
          <p className="mt-2 text-sm text-slate-400">
            Manage account roles and grant trading or research access to non-admin users.
          </p>
        </div>
        <button
          type="button"
          onClick={loadUsers}
          disabled={isLoading}
          className="self-start rounded-lg border border-slate-700 px-4 py-2 text-sm font-semibold text-slate-300 transition hover:border-brand-400 hover:text-brand-200 disabled:opacity-50"
        >
          {isLoading ? "Refreshing..." : "Refresh users"}
        </button>
      </header>

      <section className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        {[
          ["Accounts", stats.total],
          ["Administrators", stats.administrators],
          ["Live access", stats.live],
          ["Backtesting access", stats.backtesting],
        ].map(([label, value]) => (
          <div key={label} className="rounded-xl border border-slate-800 bg-slate-900/70 px-4 py-4">
            <p className="text-xs uppercase tracking-wide text-slate-500">{label}</p>
            <p className="mt-1 text-2xl font-semibold text-white">{value}</p>
          </div>
        ))}
      </section>

      {error ? (
        <div className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3 text-sm text-rose-200">
          {error}
        </div>
      ) : null}

      <section className="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/70 shadow-lg shadow-black/20">
        <div className="flex flex-col justify-between gap-3 border-b border-slate-800 px-5 py-4 sm:flex-row sm:items-center">
          <div>
            <h2 className="font-semibold text-white">Accounts and permissions</h2>
            <p className="text-xs text-slate-500">Admin accounts use only the administration workspace.</p>
          </div>
          <input
            type="search"
            aria-label="Search users"
            placeholder="Search username or email"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            className="h-10 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm text-white placeholder:text-slate-600 sm:w-72"
          />
        </div>

        {isLoading && users.length === 0 ? (
          <div className="px-5 py-10 text-center text-sm text-slate-400">Loading users...</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="min-w-[1050px] w-full divide-y divide-slate-800 text-left text-sm text-slate-200">
              <thead className="bg-slate-950/50 text-xs uppercase tracking-wide text-slate-500">
                <tr>
                  <th className="px-4 py-3">User</th>
                  <th className="px-4 py-3 text-center">Admin</th>
                  <th className="px-4 py-3 text-center">Live trading</th>
                  <th className="px-4 py-3 text-center">Backtesting</th>
                  <th className="px-4 py-3 text-center">Mode</th>
                  <th className="px-4 py-3 text-right">Actions</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-slate-800">
                {filteredUsers.length === 0 ? (
                  <tr><td colSpan={6} className="px-4 py-8 text-center text-slate-400">No matching users.</td></tr>
                ) : filteredUsers.map((user) => {
                  const busy = savingUser === user.username || deletingUser === user.username;
                  const isSelf = adminUsername.toLowerCase() === user.username.toLowerCase();
                  return (
                    <tr key={user.id ?? user.username} className="align-top">
                      <td className="px-4 py-4">
                        <p className="font-semibold text-white">{user.username}</p>
                        <p className="mt-1 text-xs text-slate-500">{user.email || "No email"}</p>
                        {isSelf ? <span className="mt-2 inline-block rounded-full bg-brand-500/10 px-2 py-0.5 text-[10px] font-semibold uppercase text-brand-300">Current admin</span> : null}
                      </td>
                      <td className="px-4 py-4 text-center"><PermissionBadge allowed={user.can_administer} tone="violet" /></td>
                      <td className="px-4 py-4 text-center"><PermissionBadge allowed={user.can_live_trade} /></td>
                      <td className="px-4 py-4 text-center"><PermissionBadge allowed={user.can_backtest} tone="sky" /></td>
                      <td className="px-4 py-4 text-center uppercase text-slate-300">{user.can_administer ? "Admin" : user.trading_mode}</td>
                      <td className="px-4 py-4">
                        <div className="flex flex-wrap justify-end gap-2">
                          <button type="button" disabled={busy || isSelf} onClick={() => updatePermission(user.username, "can_administer", !user.can_administer)} className="rounded-lg border border-violet-500/40 px-3 py-2 text-xs font-semibold text-violet-200 disabled:border-slate-700 disabled:text-slate-600">
                            {user.can_administer ? "Revoke admin" : "Grant admin"}
                          </button>
                          <button type="button" disabled={busy} onClick={() => updatePermission(user.username, "can_live_trade", !user.can_live_trade)} className="rounded-lg border border-amber-500/40 px-3 py-2 text-xs font-semibold text-amber-200 disabled:border-slate-700 disabled:text-slate-600">
                            {user.can_live_trade ? "Revoke live" : "Grant live"}
                          </button>
                          <button type="button" disabled={busy} onClick={() => updatePermission(user.username, "can_backtest", !user.can_backtest)} className="rounded-lg border border-sky-500/40 px-3 py-2 text-xs font-semibold text-sky-200 disabled:border-slate-700 disabled:text-slate-600">
                            {user.can_backtest ? "Revoke backtest" : "Grant backtest"}
                          </button>
                          <button type="button" disabled={busy || isSelf} onClick={() => setPendingDelete({ username: user.username })} className="rounded-lg bg-rose-500/80 px-3 py-2 text-xs font-semibold text-white disabled:bg-slate-700">
                            {deletingUser === user.username ? "Deleting..." : "Delete"}
                          </button>
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {pendingDelete ? (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-950/80 px-4 backdrop-blur-sm">
          <div role="dialog" aria-modal="true" aria-labelledby="delete-user-title" className="w-full max-w-md rounded-2xl border border-slate-700 bg-slate-900 p-6 shadow-2xl">
            <h2 id="delete-user-title" className="text-lg font-semibold text-white">Delete {pendingDelete.username}?</h2>
            <p className="mt-2 text-sm leading-6 text-slate-300">This permanently removes the account and all linked Rulenix data. This action cannot be undone.</p>
            <div className="mt-6 flex justify-end gap-3">
              <button type="button" disabled={Boolean(deletingUser)} onClick={() => setPendingDelete(null)} className="rounded-lg border border-slate-700 px-4 py-2 text-sm font-semibold text-slate-200 disabled:opacity-50">Cancel</button>
              <button type="button" disabled={Boolean(deletingUser)} onClick={confirmDelete} className="rounded-lg bg-rose-500 px-4 py-2 text-sm font-semibold text-white disabled:bg-slate-700">{deletingUser ? "Deleting..." : "Delete user"}</button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
