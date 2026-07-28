/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import BacktestingPage from "./BacktestingPage.jsx";
import apiClient from "../utils/axiosConfig.js";

const navigate = vi.fn();

vi.mock("../utils/axiosConfig.js", () => ({
  default: { get: vi.fn(), post: vi.fn() },
}));

vi.mock("react-router-dom", async (importOriginal) => {
  const original = await importOriginal();
  return {
    ...original,
    useNavigate: () => navigate,
    useOutletContext: () => ({
      session: { ready: true, permissions: { backtesting: true } },
    }),
  };
});

describe("BacktestingPage", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("offers all supported gold futures breakout instruments", async () => {
    apiClient.get.mockResolvedValue({ data: { runs: [] } });
    const user = userEvent.setup();
    render(<BacktestingPage />);

    await user.selectOptions(screen.getByLabelText("Strategy"), "futures_breakout_v3");
    expect(screen.getByLabelText("Instrument")).toHaveValue("GOLDTEN");
    expect(screen.getByRole("option", { name: "GOLDM · Gold Mini" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "GOLD · Gold" })).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText("Instrument"), "GOLDM");
    expect(screen.getByLabelText("Instrument")).toHaveValue("GOLDM");
    expect(screen.queryByLabelText("Stop loss %")).not.toBeInTheDocument();
  });


  it("disables new backtests on an Indian trading day", async () => {
    apiClient.get.mockResolvedValue({
      data: {
        runs: [],
        availability: {
          allowed: false,
          trade_date: "2026-07-16",
          reason:
            "Backtesting is disabled for the entire Indian trading day to reserve Angel One API capacity for live market data and order execution.",
        },
      },
    });
    render(<BacktestingPage />);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Backtesting is unavailable today"
    );
    expect(
      screen.getByRole("button", { name: "Backtesting unavailable today" })
    ).toBeDisabled();
    expect(apiClient.post).not.toHaveBeenCalled();
  });

  it("offers an Excel trade download for each saved run", async () => {
    const runId = "a806c209-36bf-4284-b3bb-ec394e174225";
    apiClient.get.mockResolvedValue({
      data: {
        runs: [
          {
            id: runId,
            instrument: "GOLDTEN",
            interval: "FIFTEEN_MINUTE",
            trading_symbol: "GOLDTEN30JUL26FUT",
            from_time: "2026-04-01T00:00:00Z",
            created_at: "2026-07-18T00:00:00Z",
            lookback_months: 3,
            lots: 2,
            summary: { strategy_name: "Futures Breakout v3", net_pnl: 1200 },
          },
        ],
      },
    });

    render(<BacktestingPage />);

    const links = await screen.findAllByRole("link", { name: /download/i });
    expect(links).toHaveLength(2);
    for (const link of links) {
      expect(link).toHaveAttribute(
        "href",
        `${window.location.origin}/api/backtesting/runs/${runId}/export`
      );
      expect(link).toHaveAttribute("download");
    }
  });

  it("shows SL2 reversal entries and each GOLD exit event", async () => {
    apiClient.get.mockResolvedValue({ data: { runs: [] } });
    apiClient.post.mockResolvedValue({
      data: {
        run: {
          id: "b89eecb2-dc5a-42cf-bc3a-a447db0f728a",
          trading_symbol: "GOLDTEN30JUL26FUT",
          from_time: "2026-07-01T00:00:00Z",
          summary: { strategy_name: "Futures Breakout v3", trades: 1, net_pnl: 250 },
        },
        trades: [
          {
            id: "08a59b80-a384-4d55-8adb-a352dad70b08",
            direction: "SELL",
            entry_time: "2026-07-10T05:00:00Z",
            entry_price: 100,
            exit_time: "2026-07-11T05:00:00Z",
            exit_price: 98,
            lots: 2,
            quantity: 20,
            realized_pnl: 250,
            exit_reason: "SL2",
            levels: {
              entry_reason: "SL2_REVERSAL",
              exit_events: [
                {
                  event: "TP1",
                  at: "2026-07-10T06:00:00Z",
                  price: 96,
                  lots: 1,
                  realized_pnl: 200,
                  remaining_lots: 1,
                  position_closed: false,
                },
                {
                  event: "SL2",
                  at: "2026-07-11T05:00:00Z",
                  price: 98,
                  lots: 1,
                  realized_pnl: 50,
                  remaining_lots: 0,
                  position_closed: true,
                },
              ],
            },
          },
          {
            id: "e58de456-1d30-4378-a881-dfd08c876a44",
            direction: "BUY",
            entry_time: "2026-07-12T03:45:00Z",
            entry_price: 101.12,
            exit_time: "2026-07-12T05:00:00Z",
            exit_price: 102,
            lots: 1,
            quantity: 10,
            realized_pnl: 88,
            exit_reason: "END_OF_TEST",
            levels: {
              entry_reason: "BREAKOUT",
              entry_source: "OPENING_RANGE",
              gap_direction: "UP",
              exit_events: [],
            },
          },
        ],
      },
    });
    const user = userEvent.setup();
    render(<BacktestingPage />);

    await user.click(screen.getByRole("button", { name: "Run backtest" }));

    expect(await screen.findByText("SL2 reversal")).toBeInTheDocument();
    expect(screen.getByText("15 min gap breakout")).toBeInTheDocument();
    expect(screen.getByText(/TP1 1 lot @ 96/)).toBeInTheDocument();
    expect(screen.getByText(/1 lot remains/)).toBeInTheDocument();
    expect(screen.getByText(/SL2 1 lot @ 98/)).toBeInTheDocument();
    expect(screen.getAllByText(/position closed/).length).toBeGreaterThan(0);
  });
});
