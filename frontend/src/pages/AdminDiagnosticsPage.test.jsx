/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import AdminDiagnosticsPage from "./AdminDiagnosticsPage.jsx";
import apiClient from "../utils/axiosConfig.js";

vi.mock("../utils/axiosConfig.js", () => ({
  default: { get: vi.fn() },
}));

describe("AdminDiagnosticsPage", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders SuperTrend line levels and diagnostic skip events", async () => {
    apiClient.get.mockResolvedValue({
      data: {
        date: "2026-11-26",
        summary: {
          total: 2,
          warnings: 1,
          errors: 0,
          info: 1,
          by_reason: { stale_candles: 1, all_runners_skipped: 1 },
        },
        supertrend_levels: {
          NIFTY: {
            supertrend_price: 24185.45,
            supertrend_direction: "UP",
            supertrend_candle_at: "2026-11-26T10:10:00+05:30",
            index_close: 24240.1,
          },
          SENSEX: {
            supertrend_price: 80120.5,
            supertrend_direction: "DOWN",
            supertrend_candle_at: "2026-11-26T10:10:00+05:30",
            index_close: 80010.0,
          },
        },
        events: [
          {
            id: 101,
            strategy_key: "supertrend_index_options_v1",
            instrument: "NIFTY",
            event_type: "strategy_skip",
            severity: "WARNING",
            reason: "stale_candles",
            message: "stale_candles (latest=09:40)",
            context: { latest: "09:40" },
            username: null,
            created_at: "2026-11-26T04:42:00Z",
          },
        ],
      },
    });

    render(<AdminDiagnosticsPage />);

    expect(await screen.findByText("Strategy diagnostics")).toBeInTheDocument();
    expect(screen.getByText("24,185.45")).toBeInTheDocument();
    expect(screen.getByText("80,120.50")).toBeInTheDocument();
    expect(screen.getByText("stale_candles")).toBeInTheDocument();
    expect(screen.getByText("latest=09:40")).toBeInTheDocument();
  });
});
