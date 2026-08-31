/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import AdminUsersPage from "./AdminUsersPage.jsx";
import apiClient from "../utils/axiosConfig.js";

vi.mock("../utils/axiosConfig.js", () => ({
  default: { get: vi.fn(), delete: vi.fn(), patch: vi.fn(), put: vi.fn() },
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
  brokerage_user_id: "ANGEL01",
  broker_egress_ip_id: null,
};

describe("AdminUsersPage Clear Trades", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("refetches admin state after the complete demo reset succeeds", async () => {
    apiClient.get
      .mockResolvedValueOnce({ data: [trader] })
      .mockResolvedValueOnce({ data: [] })
      .mockResolvedValueOnce({ data: [{ ...trader }] })
      .mockResolvedValueOnce({ data: [] });
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

    await waitFor(() => expect(apiClient.get).toHaveBeenCalledTimes(4));
    expect(apiClient.delete).toHaveBeenCalledWith("/auth/admin/users/trade-logs/", {
      data: { username: "TRADER01" },
    });
    expect(await screen.findByText(/Cleared 3 trade records, 5 demo orders/)).toBeInTheDocument();
  });

  it("guides the administrator to the explicit Global Kill Switch when clear is blocked", async () => {
    apiClient.get
      .mockResolvedValueOnce({ data: [trader] })
      .mockResolvedValueOnce({ data: [] });
    apiClient.delete.mockRejectedValue({
      response: { data: { detail: "Clear Trades requires the global kill switch to be enabled." } },
    });
    const user = userEvent.setup();
    render(<AdminUsersPage />);

    await screen.findByText("TRADER01");
    await user.click(screen.getByRole("button", { name: "Clear trade logs" }));
    expect(screen.getByRole("dialog")).toHaveTextContent(/LIVE trades, LIVE orders, credentials/i);
    await user.click(screen.getAllByRole("button", { name: "Clear trade logs" })[1]);

    expect(await screen.findByText("Enable the Global Kill Switch from Admin → Risk limits before clearing DEMO trading records.")).toBeInTheDocument();
  });

  it("assigns an available IP and supports server-default networking", async () => {
    const inventory = [{
      id: "ip-id",
      ip_address: "51.161.140.103",
      configuration_status: "CONFIGURED",
      verification_status: "VERIFIED",
      assigned_user_id: null,
    }];
    apiClient.get
      .mockResolvedValueOnce({ data: [trader] })
      .mockResolvedValueOnce({ data: inventory })
      .mockResolvedValueOnce({ data: [{ ...trader, broker_egress_ip_id: "ip-id", broker_egress_ip: "51.161.140.103" }] })
      .mockResolvedValueOnce({ data: [{ ...inventory[0], assigned_user_id: trader.id }] });
    apiClient.put.mockResolvedValue({ data: { egress_mode: "explicit" } });
    const user = userEvent.setup();
    render(<AdminUsersPage />);
    const select = await screen.findByLabelText("Angel static egress IP for TRADER01");
    await user.selectOptions(select, "ip-id");
    await waitFor(() => expect(apiClient.put).toHaveBeenCalledWith("/admin/users/trader-id/angel-egress", { egress_ip_id: "ip-id" }));
  });
});
