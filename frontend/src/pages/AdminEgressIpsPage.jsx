import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate, useOutletContext } from "react-router-dom";
import apiClient from "../utils/axiosConfig.js";

function statusClass(value) {
  if (["CONFIGURED", "VERIFIED", "AVAILABLE"].includes(value)) return "bg-emerald-500/10 text-emerald-300";
  if (["CONFIGURING", "VERIFYING"].includes(value)) return "bg-amber-500/10 text-amber-200";
  return "bg-rose-500/10 text-rose-200";
}

function Status({ value }) {
  return <span className={`rounded-full px-2.5 py-1 text-xs font-semibold ${statusClass(value)}`}>{value.replaceAll("_", " ")}</span>;
}

export default function AdminEgressIpsPage() {
  const { session } = useOutletContext();
  const navigate = useNavigate();
  const [items, setItems] = useState([]);
  const [address, setAddress] = useState("");
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const load = useCallback(async () => {
    if (!session?.username) return;
    setBusy("load");
    setError("");
    try {
      const response = await apiClient.get("/admin/egress-ips");
      setItems(Array.isArray(response.data) ? response.data : []);
    } catch (requestError) {
      if (requestError.response?.status === 401) navigate("/login", { replace: true });
      else setError(requestError.response?.data?.detail || "Unable to load Angel egress inventory.");
    } finally {
      setBusy("");
    }
  }, [navigate, session?.username]);

  useEffect(() => { load(); }, [load]);

  const counts = useMemo(() => ({
    configured: items.filter((item) => item.configuration_status === "CONFIGURED").length,
    verified: items.filter((item) => item.verification_status === "VERIFIED").length,
    available: items.filter((item) => !item.assigned_user_id).length,
  }), [items]);

  const add = async (event) => {
    event.preventDefault();
    setBusy("add"); setError(""); setNotice("");
    try {
      await apiClient.post("/admin/egress-ips", { ip_address: address.trim() });
      setAddress("");
      setNotice("The address was configured and its outbound source was verified.");
      await load();
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to register the egress IP.");
    } finally { setBusy(""); }
  };

  const verify = async (id) => {
    setBusy(id); setError(""); setNotice("");
    try {
      await apiClient.post(`/admin/egress-ips/${id}/verify`);
      setNotice("Local configuration and external source-IP verification succeeded.");
      await load();
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Egress verification failed.");
      await load();
    } finally { setBusy(""); }
  };

  return (
    <div className="space-y-6">
      <header>
        <p className="text-xs uppercase tracking-[0.35em] text-brand-300">Administration</p>
        <h1 className="mt-2 text-3xl font-semibold text-white">Angel static egress IPs</h1>
        <p className="mt-2 max-w-3xl text-sm text-slate-400">Approved public IPv4 inventory. Removing an account assignment never removes an address from Linux.</p>
      </header>
      <section className="grid gap-3 sm:grid-cols-3">
        {[["Configured", counts.configured], ["Verified", counts.verified], ["Available", counts.available]].map(([label, value]) => <div key={label} className="rounded-xl border border-slate-800 bg-slate-900/70 p-4"><p className="text-xs uppercase tracking-wide text-slate-500">{label}</p><p className="mt-1 text-2xl font-semibold text-white">{value}</p></div>)}
      </section>
      <form onSubmit={add} className="rounded-xl border border-slate-800 bg-slate-900/70 p-5">
        <h2 className="font-semibold text-white">Register purchased Additional IPv4</h2>
        <p className="mt-1 text-xs text-slate-500">Only globally routable IPv4 addresses are accepted. The restricted host helper configures and verifies them.</p>
        <div className="mt-4 flex flex-col gap-3 sm:flex-row">
          <input aria-label="Public IPv4 address" required value={address} onChange={(event) => setAddress(event.target.value)} placeholder="51.161.140.103" className="h-10 flex-1 rounded-lg border border-slate-700 bg-slate-950 px-3 text-sm text-white" />
          <button type="submit" disabled={Boolean(busy)} className="rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white disabled:bg-slate-700">{busy === "add" ? "Configuring..." : "Add and verify"}</button>
        </div>
      </form>
      {error ? <div className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3 text-sm text-rose-200">{error}</div> : null}
      {notice ? <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-4 py-3 text-sm text-emerald-200">{notice}</div> : null}
      <section className="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/70">
        <div className="flex items-center justify-between border-b border-slate-800 px-5 py-4"><h2 className="font-semibold text-white">Approved inventory</h2><button type="button" onClick={load} disabled={Boolean(busy)} className="rounded-lg border border-slate-700 px-3 py-2 text-xs font-semibold text-slate-300">Refresh</button></div>
        <div className="overflow-x-auto"><table className="min-w-[900px] w-full divide-y divide-slate-800 text-left text-sm text-slate-200">
          <thead className="bg-slate-950/50 text-xs uppercase tracking-wide text-slate-500"><tr><th className="px-4 py-3">IP address</th><th className="px-4 py-3">Server</th><th className="px-4 py-3">Verification</th><th className="px-4 py-3">Assignment</th><th className="px-4 py-3">Last verified</th><th className="px-4 py-3 text-right">Action</th></tr></thead>
          <tbody className="divide-y divide-slate-800">{items.length === 0 ? <tr><td colSpan={6} className="px-4 py-8 text-center text-slate-400">No approved egress IPs.</td></tr> : items.map((item) => <tr key={item.id}>
            <td className="px-4 py-4 font-mono text-white">{item.ip_address}</td><td className="px-4 py-4"><Status value={item.configuration_status} /></td>
            <td className="px-4 py-4"><Status value={item.verification_status} />{item.status_message ? <p className="mt-2 max-w-xs text-xs text-rose-200">{item.status_message}</p> : null}</td>
            <td className="px-4 py-4">{item.assigned_username ? <><p className="font-semibold text-white">{item.assigned_username}</p><p className="text-xs text-slate-500">{item.assigned_broker_account || "Angel One"}</p></> : <Status value="AVAILABLE" />}</td>
            <td className="px-4 py-4 text-xs text-slate-400">{item.last_verified_at ? new Date(item.last_verified_at).toLocaleString() : "Never"}</td>
            <td className="px-4 py-4 text-right"><button type="button" disabled={Boolean(busy)} onClick={() => verify(item.id)} className="rounded-lg border border-brand-500/40 px-3 py-2 text-xs font-semibold text-brand-200 disabled:opacity-40">{busy === item.id ? "Verifying..." : "Verify"}</button></td>
          </tr>)}</tbody>
        </table></div>
      </section>
    </div>
  );
}
