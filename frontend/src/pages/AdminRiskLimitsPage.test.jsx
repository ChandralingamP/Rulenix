/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import AdminRiskLimitsPage from "./AdminRiskLimitsPage.jsx";
import apiClient from "../utils/axiosConfig.js";

vi.mock("../utils/axiosConfig.js", () => ({
  default: { get: vi.fn(), put: vi.fn() },
}));

const limits = {
  max_lots: 20,
  max_quantity: 10000,
  max_notional: 100000000,
  max_open_positions: 20,
  max_trades_per_day: 100,
  max_daily_realized_loss: 1000000,
  max_daily_unrealized_loss: 1000000,
  max_price_age_seconds: 30,
};

function arrange(initialEnabled = false) {
  let enabled = initialEnabled;
  apiClient.get.mockImplementation((url) => {
    if (url === "/risk/admin/kill-switch") {
      return Promise.resolve({ data: { enabled, reason: "test state" } });
    }
    return Promise.resolve({
      data: {
        global_limits: limits,
        global_kill_switch: { enabled },
        users: [
          {
            id: "trader-id",
            username: "TRADER01",
            limits: null,
            kill_switch: { enabled: false },
          },
        ],
      },
    });
  });
  apiClient.put.mockImplementation((_url, body) => {
    enabled = body.enabled;
    return Promise.resolve({ data: { enabled } });
  });
}

describe("AdminRiskLimitsPage Global Kill Switch", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders the authoritative state as a separate global safety control", async () => {
    arrange(false);
    render(<AdminRiskLimitsPage />);

    expect(await screen.findByText("DISABLED — NORMAL ELIGIBILITY")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Enable Global Kill Switch" })).toBeEnabled();
    expect(screen.getByText(/affects every user/i)).toBeInTheDocument();
  });

  it("requires enable confirmation, prevents duplicate submission, and refetches state", async () => {
    arrange(false);
    let resolveMutation;
    apiClient.put.mockImplementation(
      () => new Promise((resolve) => { resolveMutation = resolve; })
    );
    const user = userEvent.setup();
    render(<AdminRiskLimitsPage />);

    await user.click(await screen.findByRole("button", { name: "Enable Global Kill Switch" }));
    expect(screen.getByRole("dialog")).toHaveTextContent(/block new trading entries across the platform/i);
    const confirm = screen.getByRole("button", { name: "Enable globally" });
    await user.click(confirm);
    expect(screen.getAllByRole("button", { name: "Updating..." })).toHaveLength(2);
    expect(screen.getAllByRole("button", { name: "Updating..." })[0]).toBeDisabled();
    expect(apiClient.put).toHaveBeenCalledTimes(1);
    resolveMutation({ data: { enabled: true } });

    await waitFor(() => {
      expect(apiClient.get.mock.calls.filter(([url]) => url === "/risk/admin/kill-switch")).toHaveLength(2);
    });
    expect(apiClient.put).toHaveBeenCalledWith("/risk/admin/kill-switch", expect.objectContaining({ enabled: true }));
  });

  it("uses explicit disable language and does not claim an order will be placed", async () => {
    arrange(true);
    const user = userEvent.setup();
    render(<AdminRiskLimitsPage />);

    await user.click(await screen.findByRole("button", { name: "Disable Global Kill Switch" }));
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveTextContent(/may become eligible again/i);
    expect(dialog).toHaveTextContent(/does not place an order by itself/i);
    await user.click(screen.getByRole("button", { name: "Disable globally" }));

    await screen.findByText("DISABLED — NORMAL ELIGIBILITY");
    expect(apiClient.put).toHaveBeenCalledWith("/risk/admin/kill-switch", expect.objectContaining({ enabled: false }));
  });

  it("shows mutation errors and refetches the actual backend state", async () => {
    arrange(false);
    apiClient.put.mockRejectedValue({ response: { data: { detail: "Denied by safety policy." } } });
    const user = userEvent.setup();
    render(<AdminRiskLimitsPage />);

    await user.click(await screen.findByRole("button", { name: "Enable Global Kill Switch" }));
    await user.click(screen.getByRole("button", { name: "Enable globally" }));

    expect(await screen.findByText("Denied by safety policy.")).toBeInTheDocument();
    expect(screen.getByText("DISABLED — NORMAL ELIGIBILITY")).toBeInTheDocument();
    expect(apiClient.get.mock.calls.filter(([url]) => url === "/risk/admin/kill-switch")).toHaveLength(2);
  });
});
