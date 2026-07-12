import { useEffect } from "react";
import { useDispatch, useSelector } from "react-redux";
import {
  exportTrades,
  fetchTrades,
  setMode,
  setPage,
} from "../features/pnl/pnlSlice";

const currencyFormatter = new Intl.NumberFormat("en-IN", {
  style: "currency",
  currency: "INR",
  maximumFractionDigits: 2,
});

const istDateFormatter = new Intl.DateTimeFormat("en-IN", {
  timeZone: "Asia/Kolkata",
  day: "2-digit",
  month: "2-digit",
  year: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});

export default function ProfitLossPage() {
  const dispatch = useDispatch();

  const {
    entries,
    status,
    error,
    page,
    pageSize,
    totalPages,
    totalRecords,
    totalProfit,
    totalBrokerage,
    totalNetProfit,
    exporting,
    exportError,
    mode,
  } = useSelector((state) => state.pnl);
  // Fetch trades on page/mode change
  useEffect(() => {
    dispatch(fetchTrades({ page, pageSize, mode }));
  }, [dispatch, page, pageSize, mode]);

  const handleRefresh = () => {
    dispatch(fetchTrades({ page, pageSize, mode }));
  };

  const handleDownload = () => {
    dispatch(
      exportTrades({
        page,
        page_size: pageSize,
        mode,
      })
    )
      .unwrap()
      .then((blob) => {
        console.log(blob);
        const url = window.URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url;
        link.download = `rulenix-pnl-${mode}-page-${page}.xlsx`;
        document.body.appendChild(link);
        link.click();
        link.remove();
        window.URL.revokeObjectURL(url);
      })
      .catch(() => {
        /* errors handled via slice */
      });
  };

  const handleChangePage = (nextPage) => {
    if (nextPage < 1 || nextPage > totalPages || nextPage === page) {
      return;
    }
    dispatch(setPage(nextPage));
  };

  const handleModeChange = (value) => {
    if (value === mode) {
      return;
    }
    dispatch(setMode(value));
  };

  const formatDateTime = (value) => {
    if (!value) {
      return "—";
    }
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) {
      return "—";
    }
    return istDateFormatter.format(date);
  };

  const formatPrice = (value) => {
    if (value === null || value === undefined || value === "") {
      return "—";
    }
    const numeric = Number(value);
    if (!Number.isFinite(numeric)) {
      return "—";
    }
    return numeric.toFixed(2);
  };

  const modeOptions = [
    { value: "all", label: "All" },
    { value: "demo", label: "Demo" },
    { value: "live", label: "Live" },
  ];

  return (
    <div className="mx-auto flex w-full max-w-7xl flex-col gap-6">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-3xl font-semibold text-white">
            Profit &amp; Loss
          </h1>
          <p className="text-sm text-slate-400">
            Review executed trades, monitor realized profit, and export trade
            history for reporting.
          </p>
        </div>
        <div className="flex flex-col items-end gap-3 sm:flex-row sm:items-center">
          <div className="flex items-center gap-2 rounded-full border border-slate-700 bg-slate-900/70 p-1">
            {modeOptions.map((option) => {
              const isActive = mode === option.value;
              return (
                <button
                  key={option.value}
                  type="button"
                  onClick={() => handleModeChange(option.value)}
                  className={`rounded-full px-3 py-1 text-xs font-semibold transition ${
                    isActive
                      ? "bg-brand-500 text-white shadow-md shadow-brand-500/30"
                      : "text-slate-300 hover:text-white"
                  }`}
                >
                  {option.label}
                </button>
              );
            })}
          </div>
          <button
            type="button"
            onClick={handleRefresh}
            disabled={status === "loading"}
            className="inline-flex items-center gap-2 rounded-lg border border-slate-600 bg-slate-800 px-4 py-2 text-sm font-semibold text-slate-100 transition hover:border-slate-500 hover:bg-slate-700 disabled:cursor-not-allowed disabled:opacity-50"
            title="Refresh table data"
          >
            <svg
              className="h-4 w-4"
              fill="none"
              stroke="currentColor"
              viewBox="0 0 24 24"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={2}
                d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"
              />
            </svg>
            Refresh
          </button>
          <button
            type="button"
            onClick={handleDownload}
            disabled={exporting || status === "loading"}
            className="inline-flex items-center gap-2 rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white shadow-lg shadow-brand-500/30 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:bg-slate-600"
          >
            {exporting ? "Preparing..." : "Download Excel"}
          </button>
        </div>
      </header>

      {error ? (
        <div className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3 text-sm text-rose-200">
          {error}
        </div>
      ) : null}
      {exportError ? (
        <div className="text-xs text-rose-300">{exportError}</div>
      ) : null}

      <div className="overflow-x-auto rounded-2xl border border-slate-800 bg-slate-900/60 shadow-xl shadow-black/30">
        <table className="min-w-full divide-y divide-slate-800 text-sm text-slate-100">
          <thead>
            <tr className="bg-slate-900/80 text-center text-xs uppercase tracking-normal text-slate-400">
              <th className="whitespace-nowrap px-4 py-3">#</th>
              <th className="whitespace-nowrap px-4 py-3">Entry Date</th>
              <th className="whitespace-nowrap px-4 py-3">Exit Date</th>
              <th className="whitespace-nowrap px-4 py-3">Instrument</th>
              <th className="whitespace-nowrap px-4 py-3">Symbol</th>
              <th className="whitespace-nowrap px-4 py-3">Side</th>
              <th className="whitespace-nowrap px-4 py-3">Qty</th>
              <th className="whitespace-nowrap px-4 py-3">Entry @</th>
              <th className="whitespace-nowrap px-4 py-3">Current @</th>
              <th className="whitespace-nowrap px-4 py-3">Exit @</th>
              <th className="whitespace-nowrap px-4 py-3">P/L</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-slate-800">
            {status === "loading" ? (
              <tr>
                <td
                  colSpan={11}
                  className="px-4 py-8 text-center text-sm text-slate-400"
                >
                  Loading trades...
                </td>
              </tr>
            ) : entries.length === 0 ? (
              <tr>
                <td
                  colSpan={11}
                  className="px-4 py-8 text-center text-sm text-slate-400"
                >
                  No trade history available.
                </td>
              </tr>
            ) : (
              entries.map((trade, index) => {
                const serial = (page - 1) * pageSize + index + 1;
                // Use real-time P&L calculation if available, otherwise fall back to stored pnl
                const profitValue = Number(
                  trade.pnl_realtime ?? trade.pnl ?? trade.pl ?? 0
                );
                const formattedProfit = Number.isFinite(profitValue)
                  ? currencyFormatter.format(profitValue)
                  : "—";
                const entryDisplay = formatDateTime(trade.entry_datetime);
                const exitDisplay = formatDateTime(trade.exit_datetime);
                const instrumentLabel =
                  trade.instrument_label ||
                  trade.instrument_symbol ||
                  trade.instrument ||
                  trade.scrip ||
                  "—";
                const tradingSymbol =
                  trade.contract_symbol ||
                  trade.instrument_trading_symbol ||
                  trade.symbol ||
                  trade.trading_symbol ||
                  trade.instrument_symbol ||
                  "—";
                const sideDisplay =
                  trade.direction_display ||
                  (typeof trade.direction === "string"
                    ? trade.direction.toUpperCase()
                    : "—");
                const quantityRaw = trade.quantity ?? trade.qty;
                const quantityDisplay =
                  quantityRaw === null ||
                  quantityRaw === undefined ||
                  quantityRaw === ""
                    ? "—"
                    : Number.isFinite(Number(quantityRaw))
                    ? Number(quantityRaw).toLocaleString("en-IN")
                    : quantityRaw;
                const entryPrice = formatPrice(
                  trade.entry_price ?? trade.buy_price
                );
                const currentPrice = formatPrice(trade.last_price);
                const exitPrice = formatPrice(
                  trade.exit_price ?? trade.sell_price
                );

                return (
                  <tr key={trade.id ?? serial} className="text-center">
                    <td className="whitespace-nowrap px-4 py-3 text-xs text-slate-400">
                      {serial}
                    </td>
                    <td className="whitespace-nowrap px-4 py-3">
                      {entryDisplay}
                    </td>
                    <td className="whitespace-nowrap px-4 py-3">
                      {exitDisplay}
                    </td>
                    <td className="whitespace-nowrap px-4 py-3">
                      {instrumentLabel}
                    </td>
                    <td className="whitespace-nowrap px-4 py-3">
                      {tradingSymbol}
                    </td>
                    <td className="whitespace-nowrap px-4 py-3 uppercase text-slate-300">
                      {sideDisplay}
                    </td>
                    <td className="whitespace-nowrap px-4 py-3">
                      {quantityDisplay}
                    </td>
                    <td className="whitespace-nowrap px-4 py-3">
                      {entryPrice}
                    </td>
                    <td className="whitespace-nowrap px-4 py-3 font-semibold text-cyan-300">
                      {currentPrice}
                    </td>
                    <td className="whitespace-nowrap px-4 py-3">{exitPrice}</td>
                    <td
                      className={`whitespace-nowrap px-4 py-3 text-center font-semibold ${
                        profitValue < 0 ? "text-rose-300" : "text-emerald-300"
                      }`}
                    >
                      {formattedProfit}
                    </td>
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>

      <footer className="flex flex-col gap-4 rounded-2xl border border-slate-800 bg-slate-900/60 px-5 py-4 shadow-inner shadow-black/40 sm:flex-row sm:items-center sm:gap-6">
        <div className="flex w-full flex-col text-sm text-slate-300 sm:w-[320px] sm:flex-none">
          <span>
            Showing page {page} of {totalPages} · {totalRecords} trades · Mode:{" "}
            {mode}
          </span>
          <span className="mt-1 inline-flex items-center gap-1 text-xs text-emerald-400">
            <span className="h-2 w-2 rounded-full bg-emerald-400 animate-pulse"></span>
            Live updates (every 5 seconds)
          </span>
        </div>
        <div className="flex w-full flex-1 items-center justify-center gap-2">
          <button
            type="button"
            onClick={() => handleChangePage(page - 1)}
            disabled={page === 1}
            className="rounded-lg border border-slate-700 bg-slate-900 px-3 py-1 text-sm text-slate-200 transition hover:border-brand-400 hover:text-brand-300 disabled:cursor-not-allowed disabled:border-slate-800 disabled:text-slate-500"
          >
            Prev
          </button>
          <div className="flex items-center gap-1 text-sm text-slate-300">
            Page
            <span className="rounded-md border border-slate-700 bg-slate-950 px-2 py-1 text-xs">
              {page}
            </span>
          </div>
          <button
            type="button"
            onClick={() => handleChangePage(page + 1)}
            disabled={page === totalPages}
            className="rounded-lg border border-slate-700 bg-slate-900 px-3 py-1 text-sm text-slate-200 transition hover:border-brand-400 hover:text-brand-300 disabled:cursor-not-allowed disabled:border-slate-800 disabled:text-slate-500"
          >
            Next
          </button>
        </div>
        <div className="w-full text-center text-sm text-white sm:w-[260px] sm:flex-none sm:text-right">
          <div className="flex flex-col items-center gap-1 sm:items-end">
            <span>
              Gross P&amp;L: {currencyFormatter.format(totalProfit || 0)}
            </span>
            <span className="text-slate-300">
              Brokerage: {currencyFormatter.format(totalBrokerage || 0)}
            </span>
            <span className="font-semibold text-brand-300">
              Net P&amp;L: {currencyFormatter.format(totalNetProfit || 0)}
            </span>
          </div>
        </div>
      </footer>
    </div>
  );
}
