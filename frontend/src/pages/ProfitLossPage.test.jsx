/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { Provider } from "react-redux";
import { configureStore } from "@reduxjs/toolkit";
import pnlReducer from "../features/pnl/pnlSlice.js";
import ProfitLossPage from "./ProfitLossPage.jsx";
import apiClient from "../utils/axiosConfig.js";

vi.mock("../utils/axiosConfig.js", () => ({
  default: { get: vi.fn() },
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

  it("labels the 3:20 PM square-off reason", async () => {
    renderPage([{
      id: "trade-2",
      status: "closed",
      direction: "BUY",
      quantity: 20,
      strategy_key: "option_entry_v1",
      strategy_name: "Option Entry Strategy V1.0",
      instrument_label: "SENSEX_CE",
      contract_symbol: "SENSEX26AUGCE",
      entry_price: 250,
      exit_price: 260,
      exit_reason: "MARKET_CLOSED",
      pnl: 200,
    }]);

    expect(await screen.findByText("Market closed (3:20 PM)")).toBeInTheDocument();
    expect(screen.getByText("Option Entry Strategy V1.0")).toBeInTheDocument();
  });
});
