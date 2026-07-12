/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import AccountSettingsModal from "./AccountSettingsModal.jsx";
import apiClient from "../utils/axiosConfig.js";

vi.mock("../utils/axiosConfig.js", () => ({
  default: { get: vi.fn(), put: vi.fn(), post: vi.fn(), patch: vi.fn() },
}));

const profile = {
  username: "TRADER01",
  email: "trader@example.com",
  mobile_number: "9999999999",
  client_id: "A123",
  trading_mode: "demo",
  permissions: { administer_users: false, live_trading: true },
};

describe("AccountSettingsModal trading mode", () => {
  afterEach(cleanup);
  beforeEach(() => {
    vi.clearAllMocks();
    apiClient.get.mockImplementation((url) => Promise.resolve({
      data: url === "/account/profile" ? profile : { mode: "demo", balance: 200000 },
    }));
  });

  it("requires explicit confirmation before requesting live mode", async () => {
    const user = userEvent.setup();
    const changed = vi.fn();
    apiClient.put.mockResolvedValue({
      data: { detail: "Trading mode changed to live.", trading_mode: "live", profile: { ...profile, trading_mode: "live" } },
    });
    render(<AccountSettingsModal open username="TRADER01" permissions={{ live_trading: true }} tradingMode="demo" onClose={() => {}} onProfileChanged={() => {}} onBalanceChanged={() => {}} onTradingModeChanged={changed} />);

    const liveButton = await screen.findByRole("button", { name: "Switch to live" });
    expect(liveButton).toBeDisabled();
    await user.click(screen.getByRole("checkbox"));
    await user.click(liveButton);

    await waitFor(() => expect(apiClient.put).toHaveBeenCalledWith("/account/trading-mode", { mode: "live", confirm_live: true }));
    expect(changed).toHaveBeenCalledWith("live");
  });

  it("does not offer live confirmation without server permission", async () => {
    apiClient.get.mockImplementation((url) => Promise.resolve({
      data: url === "/account/profile" ? { ...profile, permissions: { administer_users: false, live_trading: false } } : { mode: "demo", balance: 200000 },
    }));
    render(<AccountSettingsModal open username="TRADER01" permissions={{ live_trading: false }} tradingMode="demo" onClose={() => {}} onProfileChanged={() => {}} onBalanceChanged={() => {}} onTradingModeChanged={() => {}} />);
    expect(await screen.findByText(/requires permission from an authorized administrator/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Switch to live" })).toBeDisabled();
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
  });
});
