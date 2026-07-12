/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Provider } from "react-redux";
import { configureStore } from "@reduxjs/toolkit";
import StrategiesPage from "./StrategiesPage.jsx";
import strategiesReducer from "../features/strategies/strategiesSlice.js";
import apiClient from "../utils/axiosConfig.js";
import { clearAuthUsername, setAuthUsername } from "../utils/authCookies.js";

vi.mock("../utils/axiosConfig.js", () => ({
  default: { get: vi.fn(), put: vi.fn() },
}));

const instrument = {
  instrument: "GOLDTEN",
  label: "Gold Futures",
  enabled: false,
  lots: 1,
  run_day_session: true,
  run_evening_session: true,
  snapshot: {
    status: "ready",
    contract_symbol: "GOLDTEN05AUG26FUT",
    contract_expiry: "2026-08-05",
    lot_size: 1,
  },
};

const strategy = {
  key: "futures_breakout_v3",
  name: "Futures Breakout v3",
  description: "Four-day MCX futures breakout.",
  active: false,
  instruments: [instrument],
};

describe("StrategiesPage", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    clearAuthUsername();
  });

  it("reveals instruments only after activation and saves GOLDTEN separately", async () => {
    setAuthUsername("TRADER01");
    apiClient.get
      .mockResolvedValueOnce({ data: { strategies: [strategy] } })
      .mockResolvedValueOnce({
        data: {
          strategies: [
            {
              ...strategy,
              active: true,
              instruments: [{ ...instrument, enabled: true, lots: 3 }],
            },
          ],
        },
      });
    apiClient.put
      .mockResolvedValueOnce({
        data: { strategies: [{ ...strategy, active: true }] },
      })
      .mockResolvedValueOnce({ data: {} });
    const store = configureStore({ reducer: { strategies: strategiesReducer } });
    const user = userEvent.setup();

    render(
      <Provider store={store}>
        <StrategiesPage />
      </Provider>
    );

    expect(await screen.findByText("Futures Breakout v3")).toBeInTheDocument();
    expect(screen.queryByText("Gold Futures")).not.toBeInTheDocument();

    await user.click(
      screen.getByRole("switch", { name: /Activate Futures Breakout v3/i })
    );
    expect(await screen.findByText("Gold Futures")).toBeInTheDocument();
    expect(screen.getByText("GOLDTEN05AUG26FUT")).toBeInTheDocument();

    const lotsInput = screen.getByLabelText("GOLDTEN trade lots");
    await user.clear(lotsInput);
    await user.type(lotsInput, "3");
    await user.click(
      screen.getByRole("switch", {
        name: /Use GOLDTEN in Futures Breakout v3/i,
      })
    );
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(apiClient.put).toHaveBeenLastCalledWith(
        "/strategy/futures-breakout",
        expect.objectContaining({
          instrument: "GOLDTEN",
          enabled: true,
          lots: 3,
        })
      )
    );
    expect(apiClient.put.mock.lastCall[1]).not.toHaveProperty("username");
  });

  it("shows only the newest operational alert", async () => {
    setAuthUsername("TRADER01");
    apiClient.get.mockResolvedValue({
      data: {
        strategies: [
          {
            ...strategy,
            active: true,
            operational_alerts: [
              {
                id: 2,
                severity: "error",
                code: "snapshot_refresh_failed",
                message: "Invalid Angel One response: error decoding response body",
                created_at: "2026-07-02T18:09:00Z",
              },
              {
                id: 1,
                severity: "error",
                message: "Older broker issue",
                created_at: "2026-07-02T18:03:00Z",
              },
            ],
          },
        ],
      },
    });
    const store = configureStore({ reducer: { strategies: strategiesReducer } });

    render(
      <Provider store={store}>
        <StrategiesPage />
      </Provider>
    );

    expect(
      await screen.findByText(
        "Trading paused: Market data is temporarily unavailable. No trades will be placed until it recovers"
      )
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/Invalid Angel One response/i)
    ).not.toBeInTheDocument();
    expect(screen.queryByText(/3\/7\/2026/)).not.toBeInTheDocument();
    expect(screen.queryByText("Older broker issue")).not.toBeInTheDocument();
  });
});
