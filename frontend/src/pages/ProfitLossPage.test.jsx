/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Provider } from "react-redux";
import { configureStore } from "@reduxjs/toolkit";
import pnlReducer from "../features/pnl/pnlSlice.js";
import ProfitLossPage from "./ProfitLossPage.jsx";
import apiClient from "../utils/axiosConfig.js";

vi.mock("../utils/axiosConfig.js", () => ({
  default: { get: vi.fn(), post: vi.fn() },
}));

vi.mock("../utils/authCookies.js", () => ({
  getAuthUsername: () => "TRADER01",
}));

function renderPage(results) {
  apiClient.get.mockResolvedValue({
    data: {
      results,
      total_records: results.length,
      total_pages: 1,
      total_profit: 0,
      mode: "all",
    },
  });
  const store = configureStore({ reducer: { pnl: pnlReducer } });
  render(<Provider store={store}><ProfitLossPage /></Provider>);
}

describe("ProfitLossPage exit audit", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("shows TP1 and the later SL2 fill separately", async () => {
    renderPage([{
      id: "trade-1",
      status: "closed",
      direction: "BUY",
      quantity: 2,
      strategy_key: "futures_breakout_v3",
      strategy_name: "Futures Breakout v3",
      instrument_label: "SILVERMIC",
      contract_symbol: "SILVERMIC31AUG26FUT",
      entry_price: 223237,
      exit_price: 226902,
      exit_reason: "SL2",
      tp1_exit_price: 227100,
      tp1_exit_quantity: 1,
      tp1_exit_datetime: "2026-08-05T10:00:00Z",
      pnl: 6982,
    }]);

    expect(await screen.findByText("SL2 hit")).toBeInTheDocument();
    expect(screen.getByText("Futures Breakout v3")).toBeInTheDocument();
    expect(screen.getByText(/227100\.00.*Qty 1/)).toBeInTheDocument();
    expect(screen.getByText("226902.00")).toBeInTheDocument();
  });

  it("labels the 3:10 PM square-off reason", async () => {
    renderPage([{
      id: "trade-2",
      status: "closed",
      direction: "BUY",
      quantity: 20,
      strategy_key: "supertrend_index_options_v1",
      strategy_name: "SuperTrend Index Options v1",
      instrument_label: "SENSEX_CE",
      contract_symbol: "SENSEX26AUGCE",
      entry_price: 250,
      exit_price: 260,
      exit_reason: "MARKET_CLOSED",
      pnl: 200,
    }]);

    expect(await screen.findByText("Market closed (3:10 PM)")).toBeInTheDocument();
    expect(screen.getByText("SuperTrend Index Options v1")).toBeInTheDocument();
  });

  it("keeps the close action for an open LIVE trade", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    apiClient.post.mockResolvedValue({
      data: { status: "submitted", message: "Awaiting broker fill." },
    });
    renderPage([{
      id: "trade-live",
      status: "open",
      execution_mode: "live",
      direction: "BUY",
      quantity: 20,
      strategy_key: "futures_breakout_v3",
      instrument_label: "GOLDTEN",
      contract_symbol: "GOLDTEN30SEP26FUT",
      entry_price: 100,
      last_price: 101,
      pnl: 20,
    }]);

    fireEvent.click(await screen.findByRole("button", { name: "Close Trade" }));
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("remaining quantity 20"));
    await waitFor(() =>
      expect(apiClient.post).toHaveBeenCalledWith(
        "/pnl/trades/trade-live/close"
      )
    );
    expect(screen.getByText("LIVE")).toBeInTheDocument();
  });

  it("shows and completes the close action for an open DEMO trade", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    apiClient.post.mockResolvedValue({
      data: { status: "completed", message: "DEMO trade closed locally." },
    });
    renderPage([{
      id: "trade-demo",
      status: "open",
      execution_mode: "demo",
      direction: "SELL",
      quantity: 2,
      strategy_key: "futures_breakout_v3",
      instrument_label: "SILVERMIC",
      contract_symbol: "SILVERMIC30NOV26FUT",
      entry_price: 236315,
      last_price: 236901,
      pnl: 0,
    }]);

    fireEvent.click(await screen.findByRole("button", { name: "Close Trade" }));
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("running DEMO trade"));
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("No Angel One order will be sent"));
    await waitFor(() => expect(apiClient.post).toHaveBeenCalledTimes(1));
    expect(apiClient.post).toHaveBeenCalledWith("/pnl/trades/trade-demo/close");
    await waitFor(() => expect(apiClient.get.mock.calls.length).toBeGreaterThanOrEqual(2));
    expect(screen.getByText("DEMO")).toBeInTheDocument();
  });

  it("offers close only for running DEMO and LIVE rows", async () => {
    renderPage([
      { id: "demo-open", status: "open", execution_mode: "demo", direction: "BUY", quantity: 1 },
      { id: "live-open", status: "open", execution_mode: "live", direction: "SELL", quantity: 1 },
      { id: "demo-closed", status: "closed", execution_mode: "demo", direction: "BUY", quantity: 1 },
    ]);

    expect(await screen.findAllByRole("button", { name: "Close Trade" })).toHaveLength(2);
  });

  it("prevents duplicate clicks while closing and reports the server error without an optimistic close", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    let rejectClose;
    apiClient.post.mockReturnValue(new Promise((_, reject) => {
      rejectClose = reject;
    }));
    renderPage([{
      id: "demo-error",
      status: "open",
      execution_mode: "demo",
      direction: "BUY",
      quantity: 1,
    }]);

    const close = await screen.findByRole("button", { name: "Close Trade" });
    fireEvent.click(close);
    const pending = await screen.findByRole("button", { name: "Closing..." });
    expect(pending).toBeDisabled();
    fireEvent.click(pending);
    expect(apiClient.post).toHaveBeenCalledTimes(1);
    rejectClose({ response: { data: { detail: "Fresh quote is unavailable." } } });
    expect(await screen.findByText("Fresh quote is unavailable.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close Trade" })).toBeInTheDocument();
  });
});
