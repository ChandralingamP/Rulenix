/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import AdminUsersPage from "./AdminUsersPage.jsx";
import apiClient from "../utils/axiosConfig.js";

vi.mock("../utils/axiosConfig.js", () => ({
  default: { get: vi.fn(), delete: vi.fn(), patch: vi.fn() },
}));

const navigateMock = vi.fn();
const outletContext = {
  session: { username: "ADMIN" },
  refreshSession: vi.fn(),
};

vi.mock("react-router-dom", async (importOriginal) => ({
  ...(await importOriginal()),
  useNavigate: () => navigateMock,
  useOutletContext: () => outletContext,
}));

const trader = {
  id: "trader-id",
  username: "TRADER01",
  email: "trader@example.test",
  can_administer: false,
  can_live_trade: true,
  can_backtest: true,
  can_backtest_on_trading_days: false,
  trading_mode: "demo",
};

describe("AdminUsersPage Clear Trades", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("refetches admin state after the complete demo reset succeeds", async () => {
    apiClient.get
      .mockResolvedValueOnce({ data: [trader] })
      .mockResolvedValueOnce({ data: [{ ...trader }] });
    apiClient.delete.mockResolvedValue({
      data: {
        deleted_trades: 3,
        deleted_demo_orders: 5,
        deleted_backtest_runs: 1,
      },
    });
    const user = userEvent.setup();
    render(<AdminUsersPage />);

    await screen.findByText("TRADER01");
    await user.click(screen.getByRole("button", { name: "Clear trade logs" }));
    expect(screen.getByText(/closed and running demo trades/i)).toBeInTheDocument();
    await user.click(screen.getAllByRole("button", { name: "Clear trade logs" })[1]);

    await waitFor(() => expect(apiClient.get).toHaveBeenCalledTimes(2));
    expect(apiClient.delete).toHaveBeenCalledWith("/auth/admin/users/trade-logs/", {
      data: { username: "TRADER01" },
    });
    expect(await screen.findByText(/Cleared 3 trade records, 5 demo orders/)).toBeInTheDocument();
  });
});
