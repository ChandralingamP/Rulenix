import { useCallback, useEffect, useState } from "react";
import axios from "axios";
import { API_BASE_URL } from "../utils/constants";
import { getAuthUsername } from "../utils/authCookies";

export default function LogsViewerPage() {
  const username = getAuthUsername();
  const [files, setFiles] = useState([]);
  const [selected, setSelected] = useState("");
  const [content, setContent] = useState("");
  const [lines, setLines] = useState(500);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [autoRefresh, setAutoRefresh] = useState(false);

  const loadFiles = useCallback(async () => {
    if (!username) return;
    try {
      setError("");
      const response = await axios.get(`${API_BASE_URL}/logs/files/`);
      const nextFiles = response.data?.files || [];
      setFiles(nextFiles);
      setSelected((current) => current || nextFiles[0]?.filename || "");
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to load logs.");
    }
  }, [username]);

  const loadContent = useCallback(async () => {
    if (!username || !selected) return;
    try {
      setLoading(true);
      setError("");
      const response = await axios.get(`${API_BASE_URL}/logs/content/`, {
        params: { filename: selected, lines, tail: true },
      });
      setContent(response.data?.content || "");
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to read this log.");
    } finally {
      setLoading(false);
    }
  }, [lines, selected, username]);

  useEffect(() => {
    loadFiles();
  }, [loadFiles]);

  useEffect(() => {
    loadContent();
  }, [loadContent]);

  useEffect(() => {
    if (!autoRefresh) return undefined;
    const timer = window.setInterval(loadContent, 5000);
    return () => window.clearInterval(timer);
  }, [autoRefresh, loadContent]);

  return (
    <div className="min-h-screen bg-slate-950 px-6 py-8 text-slate-100">
      <div className="mx-auto max-w-7xl space-y-5">
        <header className="flex flex-wrap items-end justify-between gap-4">
          <div>
            <p className="text-xs uppercase tracking-[0.4em] text-brand-300">Rulenix</p>
            <h1 className="mt-2 text-3xl font-semibold">Activity Logs</h1>
            <p className="mt-1 text-sm text-slate-400">Inspect broker sessions and market-data activity.</p>
          </div>
          <a href="/" className="rounded-lg border border-slate-700 px-4 py-2 text-sm hover:bg-slate-800">Back to dashboard</a>
        </header>

        <section className="flex flex-wrap items-center gap-3 rounded-2xl border border-slate-800 bg-slate-900/70 p-4">
          <select value={selected} onChange={(event) => setSelected(event.target.value)} className="min-w-64 rounded-lg border border-slate-700 bg-slate-950 px-3 py-2">
            {files.length === 0 ? <option value="">No log files</option> : null}
            {files.map((file) => <option key={file.filename} value={file.filename}>{file.filename} ({file.size_mb} MB)</option>)}
          </select>
          <select value={lines} onChange={(event) => setLines(Number(event.target.value))} className="rounded-lg border border-slate-700 bg-slate-950 px-3 py-2">
            {[100, 500, 1000, 5000].map((count) => <option key={count} value={count}>Last {count} lines</option>)}
          </select>
          <label className="flex items-center gap-2 text-sm text-slate-300">
            <input type="checkbox" checked={autoRefresh} onChange={(event) => setAutoRefresh(event.target.checked)} /> Auto-refresh
          </label>
          <button type="button" onClick={loadContent} disabled={!selected || loading} className="rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold disabled:bg-slate-700">
            {loading ? "Loading…" : "Refresh"}
          </button>
        </section>

        {error ? <p className="rounded-xl border border-rose-500/40 bg-rose-500/10 p-4 text-rose-200">{error}</p> : null}
        <pre className="min-h-[60vh] overflow-auto whitespace-pre-wrap rounded-2xl border border-slate-800 bg-black/60 p-5 font-mono text-xs leading-6 text-emerald-200">
          {content || "No log content to display."}
        </pre>
      </div>
    </div>
  );
}
