import { useCallback, useEffect, useState } from "react";
import apiClient from "../utils/axiosConfig.js";

function formatTime(value) {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString("en-IN", {
    timeZone: "Asia/Kolkata",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    day: "2-digit",
    month: "short",
  });
}

function StatusBadge({ status }) {
  const classes = {
    running: "bg-amber-500/15 text-amber-300",
    completed: "bg-emerald-500/15 text-emerald-300",
    failed: "bg-rose-500/15 text-rose-300",
  };
  return (
    <span className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-semibold ${classes[status] || "bg-slate-800 text-slate-300"}`}>
      {status === "running" ? <span className="h-2 w-2 animate-pulse rounded-full bg-amber-400" /> : null}
      {status}
    </span>
  );
}

export default function AdminJobsPage() {
  const [jobs, setJobs] = useState([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [triggeringJob, setTriggeringJob] = useState("");

  const loadJobs = useCallback(async () => {
    setIsLoading(true);
    setError("");
    try {
      const response = await apiClient.get("/scheduler/jobs/");
      setJobs(Array.isArray(response.data) ? response.data : []);
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to load scheduled jobs.");
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    loadJobs();
  }, [loadJobs]);

  useEffect(() => {
    if (!jobs.some((job) => job.last_run?.status === "running")) return undefined;
    const id = window.setInterval(loadJobs, 3000);
    return () => window.clearInterval(id);
  }, [jobs, loadJobs]);

  const triggerJob = async (jobKey) => {
    setTriggeringJob(jobKey);
    setMessage("");
    setError("");
    try {
      const response = await apiClient.post("/scheduler/trigger/", { job_key: jobKey });
      setMessage(response.data?.detail || "Job triggered.");
      window.setTimeout(loadJobs, 500);
    } catch (requestError) {
      setError(requestError.response?.data?.detail || "Unable to trigger the job.");
    } finally {
      setTriggeringJob("");
    }
  };

  return (
    <div className="space-y-6">
      <header className="flex flex-col justify-between gap-4 sm:flex-row sm:items-end">
        <div>
          <p className="text-xs uppercase tracking-[0.35em] text-brand-300">Administration</p>
          <h1 className="mt-2 text-3xl font-semibold text-white">System jobs</h1>
          <p className="mt-2 text-sm text-slate-400">Review maintenance schedules and run operational jobs on demand.</p>
        </div>
        <button type="button" onClick={loadJobs} disabled={isLoading} className="self-start rounded-lg border border-slate-700 px-4 py-2 text-sm font-semibold text-slate-300 hover:border-brand-400 hover:text-brand-200 disabled:opacity-50">{isLoading ? "Refreshing..." : "Refresh jobs"}</button>
      </header>

      {message ? <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-4 py-3 text-sm text-emerald-200">{message}</div> : null}
      {error ? <div className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3 text-sm text-rose-200">{error}</div> : null}

      {isLoading && jobs.length === 0 ? (
        <section className="rounded-xl border border-slate-800 bg-slate-900/70 px-5 py-10 text-center text-sm text-slate-400">Loading jobs...</section>
      ) : jobs.length === 0 ? (
        <section className="rounded-xl border border-slate-800 bg-slate-900/70 px-5 py-10 text-center text-sm text-slate-400">No scheduled jobs are registered.</section>
      ) : (
        <section className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {jobs.map((job) => {
            const running = job.last_run?.status === "running" || triggeringJob === job.key;
            return (
              <article key={job.key} className="flex flex-col justify-between rounded-xl border border-slate-800 bg-slate-900/70 p-5">
                <div>
                  <div className="flex items-start justify-between gap-3">
                    <h2 className="font-semibold text-white">{job.label}</h2>
                    {job.last_run?.status ? <StatusBadge status={job.last_run.status} /> : null}
                  </div>
                  <p className="mt-2 text-sm leading-6 text-slate-400">{job.description}</p>
                  <dl className="mt-4 grid gap-2 text-xs">
                    <div className="flex justify-between gap-3"><dt className="text-slate-500">Schedule</dt><dd className="text-right text-slate-300">{job.schedule}</dd></div>
                    <div className="flex justify-between gap-3"><dt className="text-slate-500">Next run</dt><dd className="text-right text-slate-300">{formatTime(job.next_run)}</dd></div>
                    <div className="flex justify-between gap-3"><dt className="text-slate-500">Last triggered</dt><dd className="text-right text-slate-300">{formatTime(job.last_run?.started_at)}</dd></div>
                  </dl>
                  {job.last_run?.error ? <p className="mt-3 rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-300">{job.last_run.error}</p> : null}
                </div>
                <button type="button" disabled={running} onClick={() => triggerJob(job.key)} className="mt-5 w-full rounded-lg bg-brand-500 px-3 py-2 text-sm font-semibold text-white hover:bg-brand-400 disabled:cursor-wait disabled:bg-slate-700">{running ? "Running..." : "Run now"}</button>
              </article>
            );
          })}
        </section>
      )}
    </div>
  );
}
