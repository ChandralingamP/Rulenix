/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

  it("submits the PDF strategy with the requested index universe and defaults", async () => {
    apiClient.get.mockResolvedValue({ data: { runs: [] } });
    apiClient.post.mockResolvedValue({
      data: {
        run: {
          trading_symbol: "Nifty Bank",
          from_time: "2026-04-01T00:00:00Z",
          summary: { strategy_key: "ichimoku_keltner_tsi", trades: 0 },
        },
        trades: [],
      },
    });
    const user = userEvent.setup();
    render(<BacktestingPage />);

    expect(await screen.findByText("Ichimoku + Keltner + TSI", { selector: "option" })).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText("Instrument"), "BANKNIFTY");
    fireEvent.submit(screen.getByRole("button", { name: "Run backtest" }).closest("form"));

    await waitFor(() =>
      expect(apiClient.post).toHaveBeenCalledWith(
        "/backtesting/run",
        expect.objectContaining({
          strategy_key: "ichimoku_keltner_tsi",
          instrument: "BANKNIFTY",
          interval: "FIVE_MINUTE",
          stop_loss_percent: 5,
          target_percent: 20,
          keltner_multiplier: 2,
          require_volume: false,
        })
      )
    );
  });

  it("keeps the existing GOLDTEN breakout backtest available", async () => {
    apiClient.get.mockResolvedValue({ data: { runs: [] } });
    const user = userEvent.setup();
    render(<BacktestingPage />);

    await user.selectOptions(screen.getByLabelText("Strategy"), "futures_breakout_v3");
    expect(screen.getByLabelText("Instrument")).toHaveValue("GOLDTEN");
    expect(screen.queryByLabelText("Stop loss %")).not.toBeInTheDocument();
  });
});
